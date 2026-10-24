//! Phase 1: fleets. Runs once per tick before any economy, simulated independently (and in
//! parallel) for each system that contains fleets.
//!
//! Within a system:
//!
//! * Fleets heading elsewhere charge their hyperdrives. A fleet jumps to the next system on
//!   its route once *every* ship is charged (`hyperjump_days * hyperjump_mult`, so the slowest
//!   ship sets the pace) and arrives at the rim of the destination, on the side facing where it
//!   came from.
//! * Fleets of empires at war fight. The day is split into `combat_subticks` steps; fleets
//!   close to weapon range of the nearest enemy and every weapon fires when its cooldown
//!   (`fire_rate`, divided by outfit `fire_rate_mult`) allows. Damage hits shields first, and
//!   any part of a shot that breaks through is applied to the hull proportionally.
//! * Fleets with no enemies present carry out their orders: move within the system, or close
//!   on a planet and bombard it. Fighting enemy fleets always takes priority over bombardment.
//!
//! Bombardment results are applied serially afterwards (see [`apply_bombardment`]): buildings
//! take damage and are eventually destroyed, and the planet surrenders once accumulated siege
//! damage exceeds its resistance. Barren or unpopulated planets surrender immediately.

use rayon::prelude::*;

use crate::defs::{Defs, Ruleset};
use crate::ids::*;
use crate::sim::events::{Event, EventKind};
use crate::world::{Empire, Fleet, FleetOrder, Planet, System, World};

/// Bombardment from one empire's fleets on a planet this tick.
#[derive(Clone, Debug)]
struct Hit {
    attacker: EmpireId,
    planet: PlanetId,
    tile: Option<usize>,
    damage: f64,
}

#[derive(Default)]
struct SystemOut {
    hits: Vec<Hit>,
    events: Vec<Event>,
    jumps: Vec<FleetId>,
}

struct Ctx<'a> {
    defs: &'a Defs,
    rules: &'a Ruleset,
    empires: &'a [Empire],
    planets: &'a [Planet],
    systems: &'a [System],
    /// Per empire: is fleet upkeep fully paid (needed for shield regeneration and repairs)?
    upkeep_ok: Vec<bool>,
    day: u64,
}

impl Ctx<'_> {
    fn hostile(&self, a: EmpireId, b: EmpireId) -> bool {
        a != b && self.empires[a.idx()].at_war_with(b)
    }
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Moves `pos` towards `target` by at most `step`, stopping `stop` units short.
fn approach(pos: &mut [f64; 2], target: [f64; 2], step: f64, stop: f64) {
    let d = dist(*pos, target);
    if d <= stop || d < 1e-9 {
        return;
    }
    let travel = step.min(d - stop);
    pos[0] += (target[0] - pos[0]) / d * travel;
    pos[1] += (target[1] - pos[1]) / d * travel;
}

/// The system a fleet's order wants it to be in.
fn destination(f: &Fleet, planets: &[Planet]) -> Option<SystemId> {
    match f.order {
        FleetOrder::Idle => None,
        FleetOrder::Move { system, .. } => Some(system),
        FleetOrder::Bombard { planet, .. } => planets.get(planet.idx()).map(|p| p.system),
    }
}

pub(crate) fn tick(world: &mut World) {
    if world.fleets.is_empty() {
        return;
    }
    let defs = world.defs.clone();
    let rules = defs.ruleset(world.ruleset);

    // Routes are searched (one breadth-first search per fleet) only when a fleet's position or
    // destination changed, or its cached hop is no longer a hyperlane.
    /// A recomputed route: the next hop and the `(from, to)` it was computed for.
    type Route = (Option<SystemId>, Option<(SystemId, SystemId)>);
    let hops: Vec<Option<Route>> = world
        .fleets
        .iter()
        .map(|f| {
            let key = destination(f, &world.planets).map(|to| (f.system, to));
            let valid = f
                .next_hop
                .is_none_or(|h| world.systems[f.system.idx()].links.contains(&h));
            if key == f.route_key && valid {
                return None;
            }
            Some((key.and_then(|(from, to)| world.next_hop(from, to)), key))
        })
        .collect();
    for (f, update) in world.fleets.iter_mut().zip(hops) {
        let Some((hop, key)) = update else { continue };
        if f.next_hop != hop {
            f.jump_charge = 0.0;
        }
        f.next_hop = hop;
        f.route_key = key;
    }

    let upkeep_ok = world
        .empires
        .iter()
        .map(|e| {
            e.core_pool.is_none_or(|p| {
                let sat = &world.pools[p.idx()].sat;
                e.fleet_upkeep
                    .iter()
                    .zip(sat)
                    .all(|(u, s)| *u <= 0.0 || *s >= 0.999)
            })
        })
        .collect();

    let mut fleets = std::mem::take(&mut world.fleets);
    fleets.sort_by_key(|f| (f.system, f.id));
    let ctx = Ctx {
        defs: &defs,
        rules,
        empires: &world.empires,
        planets: &world.planets,
        systems: &world.systems,
        upkeep_ok,
        day: world.day,
    };
    let outs: Vec<SystemOut> = fleets
        .chunk_by_mut(|a, b| a.system == b.system)
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|chunk| simulate(chunk, &ctx))
        .collect();

    let mut hits = Vec::new();
    let mut jumps = Vec::new();
    for out in outs {
        world.events.extend(out.events);
        hits.extend(out.hits);
        jumps.extend(out.jumps);
    }

    // Destroyed fleets disappear; fleets that finished charging jump.
    fleets.retain(|f| !f.ships.is_empty());
    for f in &mut fleets {
        if !jumps.contains(&f.id) {
            continue;
        }
        let Some(to) = f.next_hop else { continue };
        let from = world.systems[f.system.idx()].coordinates;
        let dest = world.systems[to.idx()].coordinates;
        let (dx, dy) = (from[0] - dest[0], from[1] - dest[1]);
        let len = dx.hypot(dy);
        let r = rules.arrival_radius;
        f.coordinates = if len > 1e-9 {
            [dx / len * r, dy / len * r]
        } else {
            [r, 0.0]
        };
        f.system = to;
        f.jump_charge = 0.0;
        f.next_hop = None;
        if destination(f, &world.planets) == Some(to) {
            if let FleetOrder::Move {
                coordinates: None, ..
            } = f.order
            {
                f.order = FleetOrder::Idle;
            }
            world.events.push(
                Event::new(
                    world.day,
                    EventKind::FleetArrived,
                    format!(
                        "{} arrives at {}",
                        f.display_name,
                        world.systems[to.idx()].display_name
                    ),
                )
                .empire(Some(f.owner)),
            );
        }
    }
    fleets.sort_by_key(|f| f.id);
    world.fleets = fleets;

    apply_bombardment(world, hits);
}

fn simulate(fleets: &mut [Fleet], ctx: &Ctx) -> SystemOut {
    let mut out = SystemOut::default();
    let defs = ctx.defs;
    let rules = ctx.rules;
    let sys = fleets[0].system;
    let sys_name = &ctx.systems[sys.idx()].display_name;
    let n = fleets.len();
    let owners: Vec<EmpireId> = fleets.iter().map(|f| f.owner).collect();
    let hostile = |i: usize, j: usize| i != j && ctx.hostile(owners[i], owners[j]);
    let any_hostile = (0..n).any(|i| (0..n).any(|j| hostile(i, j)));
    let was_fighting = fleets.iter().any(|f| f.in_combat);
    for f in fleets.iter_mut() {
        f.in_combat = false;
    }
    let steps = if any_hostile {
        rules.combat_subticks.max(1)
    } else {
        1
    };
    let dt = 1.0 / steps as f64;
    let speeds: Vec<f64> = fleets.iter().map(|f| f.speed(defs)).collect();
    let ranges: Vec<f64> = fleets.iter().map(|f| f.max_range(defs)).collect();
    let mut losses: Vec<Vec<ShipId>> = vec![Vec::new(); n];

    // A bombardment target in this system, if the fleet has one.
    let target_planet = |f: &Fleet| match f.order {
        FleetOrder::Bombard { planet, tile } => ctx
            .planets
            .get(planet.idx())
            .filter(|p| p.system == sys && p.owner != Some(f.owner))
            .map(|p| (planet, tile, p.coordinates)),
        _ => None,
    };

    for step in 0..steps {
        let alive: Vec<usize> = fleets.iter().map(|f| f.ships.len()).collect();
        let pos: Vec<[f64; 2]> = fleets.iter().map(|f| f.coordinates).collect();
        let nearest_enemy = |i: usize, pos: &[[f64; 2]]| {
            (0..n)
                .filter(|&j| alive[j] > 0 && hostile(i, j))
                .min_by(|&a, &b| {
                    dist(pos[i], pos[a])
                        .total_cmp(&dist(pos[i], pos[b]))
                        .then(a.cmp(&b))
                })
        };

        // Movement.
        for i in 0..n {
            if alive[i] == 0 {
                continue;
            }
            let step_len = speeds[i] * dt;
            let leaving = fleets[i].next_hop.is_some();
            if leaving {
                fleets[i].jump_charge += dt;
                continue;
            }
            if let Some(j) = nearest_enemy(i, &pos) {
                if ranges[i] > 0.0 {
                    approach(
                        &mut fleets[i].coordinates,
                        pos[j],
                        step_len,
                        ranges[i] * 0.8,
                    );
                }
                continue;
            }
            let f = &fleets[i];
            let goal = match f.order {
                FleetOrder::Move {
                    system,
                    coordinates: Some(c),
                } if system == sys => Some((c, 0.0)),
                _ => target_planet(f).map(|(_, _, c)| (c, (ranges[i] * 0.8).max(0.5))),
            };
            if let Some((c, stop)) = goal {
                approach(&mut fleets[i].coordinates, c, step_len, stop);
            }
        }

        // Fire. All damage from this step lands simultaneously afterwards.
        let pos: Vec<[f64; 2]> = fleets.iter().map(|f| f.coordinates).collect();
        let mut damage: Vec<(usize, usize, f64, f64)> = Vec::new();
        for i in 0..n {
            if alive[i] == 0 {
                continue;
            }
            let has_enemy = nearest_enemy(i, &pos).is_some();
            let bombard = if has_enemy {
                None
            } else {
                target_planet(&fleets[i])
            };
            let mut fought = false;
            for (si, ship) in fleets[i].ships.iter_mut().enumerate() {
                for (wi, arm) in defs.ship(ship.def).armament.iter().enumerate() {
                    ship.cooldown[wi] -= dt;
                    if ship.cooldown[wi] > 0.0 {
                        continue;
                    }
                    // Nearest enemy fleet in range; spread fire across its ships.
                    let target = (0..n)
                        .filter(|&j| {
                            alive[j] > 0 && hostile(i, j) && dist(pos[i], pos[j]) <= arm.range
                        })
                        .min_by(|&a, &b| {
                            dist(pos[i], pos[a])
                                .total_cmp(&dist(pos[i], pos[b]))
                                .then(a.cmp(&b))
                        });
                    if let Some(j) = target {
                        let k = (si + wi + step as usize) % alive[j];
                        damage.push((j, k, arm.shield_damage, arm.hull_damage));
                        ship.cooldown[wi] += arm.cooldown;
                        fought = true;
                    } else if let Some((planet, tile, _)) =
                        bombard.filter(|(_, _, at)| dist(pos[i], *at) <= arm.range)
                    {
                        out.hits.push(Hit {
                            attacker: owners[i],
                            planet,
                            tile,
                            damage: arm.hull_damage,
                        });
                        ship.cooldown[wi] += arm.cooldown;
                    } else {
                        ship.cooldown[wi] = 0.0;
                    }
                }
            }
            if fought || has_enemy {
                fleets[i].in_combat = true;
            }
        }
        for (j, k, shield_dmg, hull_dmg) in damage {
            let ship = &mut fleets[j].ships[k];
            if ship.hull <= 0.0 {
                continue;
            }
            if ship.shield > 0.0 && shield_dmg > 0.0 {
                if ship.shield >= shield_dmg {
                    ship.shield -= shield_dmg;
                } else {
                    // The part of the shot that breaks through hits the hull.
                    let through = 1.0 - ship.shield / shield_dmg;
                    ship.shield = 0.0;
                    ship.hull -= hull_dmg * through;
                }
            } else {
                ship.hull -= hull_dmg;
            }
        }

        for (i, f) in fleets.iter_mut().enumerate() {
            let regen = ctx.upkeep_ok[f.owner.idx()];
            f.ships.retain(|s| {
                if s.hull <= 0.0 {
                    losses[i].push(s.def);
                    false
                } else {
                    true
                }
            });
            if regen {
                for s in &mut f.ships {
                    let d = defs.ship(s.def);
                    s.shield = (s.shield + d.shield_regen * dt).min(d.max_shield);
                }
            }
        }
    }

    // Repairs in friendly, quiet systems.
    let sys_owner = ctx.systems[sys.idx()].owner;
    for f in fleets.iter_mut() {
        if !f.in_combat && sys_owner == Some(f.owner) && ctx.upkeep_ok[f.owner.idx()] {
            for s in &mut f.ships {
                let max = defs.ship(s.def).max_hull;
                s.hull = (s.hull + max * rules.ship_repair_rate).min(max);
            }
        }
    }

    // Narration.
    let fighting = fleets.iter().any(|f| f.in_combat);
    if fighting && !was_fighting {
        let sides: Vec<String> = fleets
            .iter()
            .filter(|f| f.in_combat)
            .map(|f| {
                format!(
                    "{} ({})",
                    f.display_name,
                    ctx.empires[f.owner.idx()].display_name
                )
            })
            .collect();
        out.events.push(
            Event::new(
                ctx.day,
                EventKind::Battle,
                format!("Battle at {sys_name}: {}", sides.join(" vs ")),
            )
            .empire(Some(owners[0])),
        );
    }
    for (i, lost) in losses.iter().enumerate() {
        if lost.is_empty() {
            continue;
        }
        let f = &fleets[i];
        let (kind, msg) = if f.ships.is_empty() {
            (
                EventKind::FleetDestroyed,
                format!("{} has been destroyed at {sys_name}", f.display_name),
            )
        } else {
            (
                EventKind::ShipDestroyed,
                format!(
                    "{} lost {} ship{} at {sys_name}",
                    f.display_name,
                    lost.len(),
                    if lost.len() == 1 { "" } else { "s" }
                ),
            )
        };
        out.events
            .push(Event::new(ctx.day, kind, msg).empire(Some(f.owner)));
    }

    for f in fleets.iter() {
        if f.next_hop.is_some() && !f.ships.is_empty() && f.jump_charge >= f.jump_time(defs, rules)
        {
            out.jumps.push(f.id);
        }
    }
    out
}

/// Applies a tick's bombardment: damages buildings, advances sieges and resolves surrenders.
fn apply_bombardment(world: &mut World, mut hits: Vec<Hit>) {
    if hits.is_empty() {
        return;
    }
    // Merge per attacker, planet and tile so each gets one deterministic application.
    hits.sort_by_key(|h| (h.planet, h.attacker, h.tile));
    let mut merged: Vec<Hit> = Vec::new();
    for h in hits {
        match merged.last_mut() {
            Some(m) if m.planet == h.planet && m.attacker == h.attacker && m.tile == h.tile => {
                m.damage += h.damage
            }
            _ => merged.push(h),
        }
    }
    let day = world.day;
    let defs = world.defs.clone();
    let rules = defs.ruleset(world.ruleset);
    for h in merged {
        let p = &world.planets[h.planet.idx()];
        if p.owner == Some(h.attacker) {
            continue;
        }
        if let Some(o) = p.owner
            && !world.at_war(h.attacker, o)
        {
            continue;
        }
        let attacker_name = world.empires[h.attacker.idx()].display_name.clone();
        let (pop, stability, habitable) = (p.pop, p.stats.stability, p.habitable);
        let p = &mut world.planets[h.planet.idx()];
        if !p.flags.bombarded && p.siege == 0.0 && habitable && pop > 0 {
            world.events.push(
                Event::new(
                    day,
                    EventKind::Bombardment,
                    format!("{attacker_name} begins bombarding {}", p.display_name),
                )
                .empire(Some(h.attacker))
                .planet(h.planet),
            );
        }
        p.flags.bombarded = true;
        if !habitable || pop == 0 {
            surrender(world, h.planet, h.attacker);
            continue;
        }
        // Natural blockers are not targets.
        let target = |t: &crate::world::Tile| {
            t.building
                .as_ref()
                .is_some_and(|b| !defs.building(b.def).blocker)
        };
        let tile = h
            .tile
            .filter(|t| p.tiles.get(*t).is_some_and(target))
            .or_else(|| p.tiles.iter().position(target));
        if let Some(t) = tile {
            let b = p.tiles[t].building.as_mut().unwrap();
            let hp = defs.building(b.def).hp.unwrap_or(rules.building_hp);
            b.damage += h.damage / hp;
            if b.damage >= 1.0 {
                let name = defs.building(b.def).display_name.clone();
                p.tiles[t].building = None;
                p.dirty = true;
                world.events.push(
                    Event::new(
                        day,
                        EventKind::BuildingDestroyed,
                        format!("Bombardment destroys the {name} on {}", p.display_name),
                    )
                    .empire(p.owner)
                    .planet(h.planet),
                );
            }
        }
        p.siege += h.damage;
        if p.siege >= rules.surrender_threshold(pop, stability) {
            surrender(world, h.planet, h.attacker);
        }
    }
}

/// Hands a planet to `attacker` and stands down fleets that were bombarding it for them.
/// Hands a planet to `attacker`. Fleets that were bombarding it stand down, except AI fleets,
/// which move on to the nearest remaining planet in the system they can take (an enemy's or an
/// unowned one) so the whole system ends up theirs.
fn surrender(world: &mut World, planet: PlanetId, attacker: EmpireId) {
    let old = world.planets[planet.idx()].owner;
    world.set_planet_owner(planet, Some(attacker));
    let ai = world.empires[attacker.idx()].ai.is_some();
    let sys = world.planets[planet.idx()].system;
    let next_target = |world: &World, from: [f64; 2]| {
        world
            .system(sys)
            .planet_ids()
            .filter(|p| match world.planet(*p).owner {
                None => true,
                Some(o) => o != attacker && world.at_war(attacker, o),
            })
            .min_by(|a, b| {
                let d = |p: &PlanetId| dist(from, world.planet(*p).coordinates);
                d(a).total_cmp(&d(b)).then(a.cmp(b))
            })
    };
    let bombarding: Vec<usize> = world
        .fleets
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            f.owner == attacker
                && matches!(f.order, FleetOrder::Bombard { planet: p, .. } if p == planet)
        })
        .map(|(i, _)| i)
        .collect();
    for i in bombarding {
        let target = if ai {
            next_target(world, world.fleets[i].coordinates)
        } else {
            None
        };
        world.fleets[i].order = match target {
            Some(p) => FleetOrder::Bombard {
                planet: p,
                tile: None,
            },
            None => FleetOrder::Idle,
        };
    }
    let p = &world.planets[planet.idx()];
    let who = world.empires[attacker.idx()].display_name.clone();
    let msg = match old {
        Some(o) => format!(
            "{} surrenders to {who}, ending {}'s rule",
            p.display_name,
            world.empires[o.idx()].display_name
        ),
        None => format!("{} submits to {who}", p.display_name),
    };
    world.events.push(
        Event::new(world.day, EventKind::Surrendered, msg)
            .empire(Some(attacker))
            .planet(planet),
    );
    if let Some(o) = old
        && !world.planets.iter().any(|p| p.owner == Some(o))
    {
        let msg = format!(
            "{} has fallen: its last world has been conquered by {who}",
            world.empires[o.idx()].display_name
        );
        world
            .events
            .push(Event::new(world.day, EventKind::EmpireFallen, msg).empire(Some(o)));
    }
}
