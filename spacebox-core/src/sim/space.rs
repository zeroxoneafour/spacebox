//! Phase 1: fleets. Runs once per tick before any economy, simulated independently (and in
//! parallel) for each system that contains fleets.
//!
//! Each fleet works through its order queue (see [`FleetOrder`]):
//!
//! * `Move`: fly to a point in the current system.
//! * `Jump`: fly to the edge of the system facing the next one (`arrival_radius` from the
//!   star), then, standing still there, charge every ship's hyperdrive from 0 to 1 (taking
//!   `hyperjump_days * hyperjump_mult` days per ship). Once all are charged the fleet jumps
//!   and arrives on the rim of the next system facing where it came from.
//! * `Bombard`: close on a planet in the current system and fire on it until it surrenders:
//!   at the given tile's building, or at random buildings if no tile is given (or once that
//!   building is destroyed).
//!
//! A fleet is *in combat* while an enemy fleet (of an empire at war with it) shares its system
//! and either side is armed. Orders are suspended in combat: the day is split into
//! `combat_subticks` steps, fleets close to weapon range of the nearest enemy, and every
//! weapon fires when its cooldown (`fire_rate`, divided by `fire_rate_mult`) allows. Damage
//! hits shields first, and any part of a shot that breaks through hits the hull.
//!
//! Bombardment results are applied serially afterwards (see [`apply_bombardment`]): buildings
//! take damage and are eventually destroyed, and the planet surrenders once accumulated siege
//! damage exceeds its resistance. Barren or unpopulated planets surrender immediately.
//!
//! A surrendering planet changes owner if the attacker fights a total war against its owner
//! (or it was unowned); otherwise the attacker only occupies it. A planet its owner's side
//! takes back from an occupier is liberated.

use rayon::prelude::*;

use crate::defs::{Defs, Ruleset, ShipStats};
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
    /// Fleet slots of destroyed ships, per owner.
    lost: Vec<(EmpireId, f64)>,
}

struct Ctx<'a> {
    defs: &'a Defs,
    rules: &'a Ruleset,
    /// `hostile[a * n + b]`, see [`World::hostility`].
    hostile: Vec<bool>,
    empires: &'a [Empire],
    planets: &'a [Planet],
    systems: &'a [System],
    /// Per empire: is fleet upkeep fully paid (needed for shield regeneration and repairs)?
    upkeep_ok: Vec<bool>,
    day: u64,
}

impl Ctx<'_> {
    fn hostile(&self, a: EmpireId, b: EmpireId) -> bool {
        self.hostile[a.idx() * self.empires.len() + b.idx()]
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

/// Where a fleet leaving `from` for `to` charges its hyperdrive: the system's edge closest to
/// the destination.
pub fn departure_point(systems: &[System], from: SystemId, to: SystemId, radius: f64) -> [f64; 2] {
    let a = systems[from.idx()].coordinates;
    let b = systems[to.idx()].coordinates;
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dy);
    if len > 1e-9 {
        [dx / len * radius, dy / len * radius]
    } else {
        [radius, 0.0]
    }
}

/// How close to a point a fleet must be to count as there.
const ARRIVED: f64 = 0.01;

/// Whether `owner` may bombard `planet` while its fleet is in `system`: the planet must be in
/// that system and be controlled (owned, or occupied) by an enemy, or be unowned in a system
/// that is unowned or its own (unowned planets in another empire's system cannot be taken).
pub fn can_bombard(world: &World, owner: EmpireId, system: SystemId, planet: PlanetId) -> bool {
    let Some(p) = world.planets.get(planet.idx()) else {
        return false;
    };
    if p.system != system {
        return false;
    }
    match p.controller() {
        Some(o) => o != owner && world.at_war(owner, o),
        None => world.system(system).owner.is_none_or(|o| o == owner),
    }
}

/// Drops orders that can no longer be carried out from the front of a fleet's queue: jumps
/// to systems without a hyperlane (and everything after them), and bombardment of planets
/// that are no longer valid targets.
fn validate_orders(world: &mut World, i: usize) {
    loop {
        let f = &world.fleets[i];
        let ok = match f.order() {
            None => return,
            Some(FleetOrder::Move { .. }) => true,
            Some(FleetOrder::Jump { system }) => {
                if !world.systems[f.system.idx()].links.contains(system) {
                    world.fleets[i].orders.clear();
                    return;
                }
                true
            }
            Some(FleetOrder::Bombard { planet, .. }) => {
                can_bombard(world, f.owner, f.system, *planet)
                    && f.max_range(&world.empires[f.owner.idx()].ship_stats) > 0.0
            }
        };
        if ok {
            return;
        }
        world.fleets[i].orders.remove(0);
    }
}

pub(crate) fn tick(world: &mut World) {
    if world.fleets.is_empty() {
        return;
    }
    let defs = world.defs.clone();
    let rules = defs.ruleset(world.ruleset);
    for i in 0..world.fleets.len() {
        validate_orders(world, i);
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
        hostile: world.hostility(),
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
    world.ship_losses.resize(world.empires.len(), 0.0);
    for out in outs {
        world.events.extend(out.events);
        hits.extend(out.hits);
        jumps.extend(out.jumps);
        for (e, slots) in out.lost {
            world.ship_losses[e.idx()] += slots;
        }
    }

    // Destroyed fleets disappear; fleets that finished charging jump.
    fleets.retain(|f| !f.ships.is_empty());
    for f in &mut fleets {
        if !jumps.contains(&f.id) {
            continue;
        }
        let Some(FleetOrder::Jump { system: to }) = f.orders.first().cloned() else {
            continue;
        };
        f.coordinates = departure_point(&world.systems, to, f.system, rules.arrival_radius);
        f.system = to;
        f.orders.remove(0);
        for s in &mut f.ships {
            s.jump_charge = 0.0;
        }
        if !f
            .orders
            .iter()
            .any(|o| matches!(o, FleetOrder::Jump { .. }))
        {
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
    let rules = ctx.rules;
    let sys = fleets[0].system;
    let sys_name = &ctx.systems[sys.idx()].display_name;
    let n = fleets.len();
    let owners: Vec<EmpireId> = fleets.iter().map(|f| f.owner).collect();
    let stats = |f: &Fleet| -> &[ShipStats] { &ctx.empires[f.owner.idx()].ship_stats };
    let speeds: Vec<f64> = fleets.iter().map(|f| f.speed(stats(f))).collect();
    let ranges: Vec<f64> = fleets.iter().map(|f| f.max_range(stats(f))).collect();
    // Enemies only fight if at least one side is armed.
    let hostile = |i: usize, j: usize| {
        i != j && ctx.hostile(owners[i], owners[j]) && (ranges[i] > 0.0 || ranges[j] > 0.0)
    };
    let any_hostile = (0..n).any(|i| (0..n).any(|j| hostile(i, j)));
    let was_fighting = fleets.iter().any(|f| f.in_combat);
    let steps = if any_hostile {
        rules.combat_subticks.max(1)
    } else {
        1
    };
    let dt = 1.0 / steps as f64;
    let mut losses: Vec<Vec<ShipId>> = vec![Vec::new(); n];
    let mut fought = vec![false; n];

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

        // Movement and orders.
        for i in 0..n {
            if alive[i] == 0 || out.jumps.contains(&fleets[i].id) {
                continue;
            }
            let step_len = speeds[i] * dt;
            if let Some(j) = nearest_enemy(i, &pos) {
                // In combat: orders wait while the fleet closes to weapon range.
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
            let f = &mut fleets[i];
            match f.orders.first().cloned() {
                None => {}
                Some(FleetOrder::Move { coordinates }) => {
                    approach(&mut f.coordinates, coordinates, step_len, 0.0);
                    if dist(f.coordinates, coordinates) <= ARRIVED {
                        f.coordinates = coordinates;
                        f.orders.remove(0);
                    }
                }
                Some(FleetOrder::Jump { system }) => {
                    let edge = departure_point(ctx.systems, sys, system, rules.arrival_radius);
                    if dist(f.coordinates, edge) > ARRIVED {
                        approach(&mut f.coordinates, edge, step_len, 0.0);
                        if dist(f.coordinates, edge) <= ARRIVED {
                            f.coordinates = edge;
                        }
                        continue;
                    }
                    // Stationary at the edge: charge.
                    let ship_stats = stats(f);
                    for s in &mut f.ships {
                        let days = Fleet::charge_days(&ship_stats[s.def.idx()], rules);
                        s.jump_charge = if days <= 0.0 {
                            1.0
                        } else {
                            (s.jump_charge + dt / days).min(1.0)
                        };
                    }
                    if f.charged() {
                        out.jumps.push(f.id);
                    }
                }
                Some(FleetOrder::Bombard { planet, .. }) => {
                    let at = ctx.planets[planet.idx()].coordinates;
                    approach(&mut f.coordinates, at, step_len, (ranges[i] * 0.8).max(0.5));
                }
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
            let bombard = match fleets[i].orders.first() {
                Some(FleetOrder::Bombard { planet, tile }) if !has_enemy => {
                    Some((*planet, *tile, ctx.planets[planet.idx()].coordinates))
                }
                _ => None,
            };
            let ship_stats = &ctx.empires[owners[i].idx()].ship_stats;
            for (si, ship) in fleets[i].ships.iter_mut().enumerate() {
                for (wi, arm) in ship_stats[ship.def.idx()].armament.iter().enumerate() {
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
                        fought[i] = true;
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
            if has_enemy {
                fought[i] = true;
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
            let ship_stats = &ctx.empires[f.owner.idx()].ship_stats;
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
                    let d = &ship_stats[s.def.idx()];
                    s.shield = (s.shield + d.shield_regen * dt).min(d.max_shield);
                }
            }
        }
    }

    // Combat lasts while enemies remain.
    let alive: Vec<usize> = fleets.iter().map(|f| f.ships.len()).collect();
    for i in 0..n {
        fleets[i].in_combat = alive[i] > 0 && (0..n).any(|j| alive[j] > 0 && hostile(i, j));
    }

    // Repairs in friendly, quiet systems.
    let sys_owner = ctx.systems[sys.idx()].owner;
    for (i, f) in fleets.iter_mut().enumerate() {
        if !fought[i] && sys_owner == Some(f.owner) && ctx.upkeep_ok[f.owner.idx()] {
            let ship_stats = &ctx.empires[f.owner.idx()].ship_stats;
            for s in &mut f.ships {
                let max = ship_stats[s.def.idx()].max_hull;
                s.hull = (s.hull + max * rules.ship_repair_rate).min(max);
            }
        }
    }

    // Narration.
    let fighting = fought.iter().any(|f| *f);
    if fighting && !was_fighting {
        let sides: Vec<String> = fleets
            .iter()
            .enumerate()
            .filter(|(i, _)| fought[*i])
            .map(|(_, f)| {
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
        let slots: u32 = lost.iter().map(|s| ctx.defs.ship(*s).fleet_size).sum();
        out.lost.push((f.owner, slots as f64));
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
    let seed = world.seed;
    let defs = world.defs.clone();
    let rules = defs.ruleset(world.ruleset);
    for h in merged {
        let p = &world.planets[h.planet.idx()];
        if p.controller() == Some(h.attacker) {
            continue;
        }
        if let Some(o) = p.controller()
            && !world.at_war(h.attacker, o)
        {
            continue;
        }
        let attacker_name = world.empires[h.attacker.idx()].display_name.clone();
        let (pop, stability, habitable) = (p.pop(), p.stats.stability, p.habitable);
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
        // A given tile while it still has a building; otherwise (or with no tile given) a
        // random building, picked with a deterministic roll.
        let tile = h
            .tile
            .filter(|t| p.tiles.get(*t).is_some_and(target))
            .or_else(|| {
                let candidates: Vec<usize> = (0..p.tiles.len())
                    .filter(|t| target(&p.tiles[*t]))
                    .collect();
                let roll =
                    crate::rng::hash01(seed, &[day, h.planet.0 as u64, h.attacker.0 as u64, 0xB0B]);
                (!candidates.is_empty()).then(|| {
                    candidates[(roll * candidates.len() as f64) as usize % candidates.len()]
                })
            });
        if let Some(t) = tile {
            let b = p.tiles[t].building.as_mut().unwrap();
            let hp = defs.building(b.def).hp.unwrap_or(rules.building_hp);
            b.damage += h.damage / hp;
            p.flags.damaged = true;
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

/// Hands a planet to `attacker`: it takes it over if it was unowned or the attacker fights a
/// total war against its owner, liberates it if the attacker owns it or fights on its owner's
/// side, and occupies it otherwise. Fleets that were bombarding it stand down, except AI
/// fleets, which move on to the nearest remaining planet in the system they can take (an
/// enemy's or an unowned one) so the whole system ends up theirs.
fn surrender(world: &mut World, planet: PlanetId, attacker: EmpireId) {
    let p = &world.planets[planet.idx()];
    let (owner, old) = (p.owner, p.controller());
    let populated = p.pop() > 0;
    if let Some(o) = old {
        world.planet_losses.push((o, populated));
    }
    let liberated = match owner {
        None => {
            world.set_planet_owner(planet, Some(attacker));
            false
        }
        // Taken back by its owner, or freed by someone the owner is not at war with.
        Some(o) if o == attacker || !world.at_war(attacker, o) => {
            world.set_planet_occupier(planet, None);
            true
        }
        Some(o) => {
            if world.goal_against(attacker, o) == Some(crate::world::WarGoal::Total) {
                world.set_planet_owner(planet, Some(attacker));
            } else {
                world.set_planet_occupier(planet, Some(attacker));
            }
            false
        }
    };
    let ai = world.empires[attacker.idx()].ai.is_some();
    let sys = world.planets[planet.idx()].system;
    // Fleets bombarding the planet move on to the rest of their orders. AI fleets first take
    // on the nearest remaining planet in the system they can bombard, so the whole system
    // ends up theirs.
    let next_target = |world: &World, from: [f64; 2]| {
        world
            .system(sys)
            .planet_ids()
            .filter(|p| *p != planet && can_bombard(world, attacker, sys, *p))
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
                && matches!(f.order(), Some(FleetOrder::Bombard { planet: p, .. }) if *p == planet)
        })
        .map(|(i, _)| i)
        .collect();
    for i in bombarding {
        let target = if ai {
            next_target(world, world.fleets[i].coordinates)
        } else {
            None
        };
        let f = &mut world.fleets[i];
        match target {
            Some(p) => {
                f.orders[0] = FleetOrder::Bombard {
                    planet: p,
                    tile: None,
                }
            }
            None => {
                f.orders.remove(0);
            }
        }
    }
    let p = &world.planets[planet.idx()];
    let who = world.empires[attacker.idx()].display_name.clone();
    let msg = match old {
        Some(_) if liberated => format!("{} is liberated by {who}", p.display_name),
        Some(_) if p.occupier == Some(attacker) => {
            format!("{} surrenders to {who}, who occupies it", p.display_name)
        }
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
        && !liberated
        && Some(o) == owner
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
