//! A deliberately simple governor for AI empires (those with an `[empire.x.ai]` table).
//!
//! Every `ai_interval_days` (staggered per empire) it looks at what the empire is short of and
//! issues a handful of ordinary [`Command`]s: housing where pops are crowded, job buildings
//! where they are unemployed (or on empty habitable worlds, to attract settlers), automated
//! barren-world buildings, the occasional upgrade and terraforming project. Decisions only read
//! the world, so all empires decide in parallel.

use crate::defs::BuildingDef;
use crate::ids::*;
use crate::sim::commands::{Command, can_afford};
use crate::world::{AiPersonality, Construction, FleetOrder, World};

/// Needs, most pressing first.
struct Needs {
    /// Resources in crisis or running down, ordered by days of stock left.
    urgent: Vec<ResourceId>,
    /// Everything else the empire uses, ordered by days of stock (well-stocked ones omitted).
    all: Vec<ResourceId>,
}

/// Whether `r` is consumed by anything the empire can currently build or maintain.
fn used_by_unlocked(world: &World, empire: EmpireId, r: ResourceId) -> bool {
    let defs = &world.defs;
    let e = world.empire(empire);
    let has = |list: &[crate::defs::ResourceAmount]| list.iter().any(|a| a.resource == r);
    let known = |techs: &[TechId]| techs.iter().all(|t| e.knows(*t));
    defs.buildings.iter().any(|b| {
        b.buildable
            && known(&b.requires_tech)
            && (has(&b.build_cost) || has(&b.inputs) || has(&b.upkeep))
    }) || defs.upgrades.iter().any(|u| {
        known(&u.requires_tech)
            && (has(&u.cost) || has(&u.modifier.inputs) || has(&u.modifier.upkeep))
    }) || [
        &world.rules().terraform_large,
        &world.rules().terraform_small,
    ]
    .into_iter()
    .flatten()
    .any(|t| t.requires_tech.is_none_or(|t| e.knows(t)) && has(&t.cost))
}

fn rank_needs(
    world: &World,
    empire: EmpireId,
    pool: &crate::world::Pool,
    traits: AiPersonality,
) -> Needs {
    let defs = &world.defs;
    let e = world.empire(empire);
    let mut urgent: Vec<(f64, ResourceId)> = Vec::new();
    let mut rest: Vec<(f64, ResourceId)> = Vec::new();
    for (i, res) in defs.resources.iter().enumerate() {
        let r = ResourceId::from_idx(i);
        if !defs
            .buildings
            .iter()
            .any(|b| b.buildable && b.outputs.iter().any(|o| o.resource == r))
        {
            continue;
        }
        let income = e.income[i];
        let demand = pool.demand_full[i];
        if !res.stockpile {
            // Research and other flow resources: always wanted, scientific empires much more so.
            rest.push((90.0 * (1.0 - traits.scientific) + 5.0, r));
            continue;
        }
        let stock = e.storage[i];
        if e.has_crisis(r) || (income < -1e-6 && stock / -income < 180.0) {
            let days_left = if income < 0.0 { stock / -income } else { 0.0 };
            urgent.push((days_left, r));
        } else if demand > 0.0 || used_by_unlocked(world, empire, r) {
            let days = stock / demand.max(1.0);
            if days < 365.0 {
                rest.push((days, r));
            }
        }
    }
    urgent.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    rest.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let urgent: Vec<ResourceId> = urgent.into_iter().map(|(_, r)| r).collect();
    let all = urgent
        .iter()
        .copied()
        .chain(rest.into_iter().map(|(_, r)| r))
        .collect();
    Needs { urgent, all }
}

fn output_of(def: &BuildingDef, r: ResourceId) -> f64 {
    def.outputs
        .iter()
        .filter(|o| o.resource == r)
        .map(|o| o.count)
        .sum()
}

/// Picks the best free tile where the building may stand (land/water rules): highest tile
/// multiplier for `r`, then the most matching adjacency neighbours, then the lowest index.
fn best_tile(
    world: &World,
    empire: EmpireId,
    pid: PlanetId,
    b: BuildingId,
    r: Option<ResourceId>,
) -> Option<usize> {
    let defs = &world.defs;
    let def = defs.building(b);
    let p = world.planet(pid);
    let shape = p.size.shape();
    p.free_tiles()
        .filter(|t| world.build_plan(empire, pid, *t, b).is_ok())
        .max_by(|&x, &y| {
            let score = |t: usize| {
                let mult = r.map_or(1.0, |r| p.tiles[t].mult(r));
                let adj = shape
                    .neighbors(t)
                    .iter()
                    .filter_map(|&n| p.tiles[n as usize].building.as_ref())
                    .filter(|nb| {
                        def.adjacency
                            .iter()
                            .any(|a| defs.target_matches(a.target, nb.def))
                    })
                    .count();
                mult + adj as f64 * 0.05
            };
            score(x).total_cmp(&score(y)).then(y.cmp(&x))
        })
}

fn pick_building(
    world: &World,
    empire: EmpireId,
    pid: PlanetId,
    budget: &[f64],
    filter: impl Fn(&BuildingDef) -> bool,
    score: impl Fn(&BuildingDef) -> f64,
) -> Option<BuildingId> {
    let p = world.planet(pid);
    // Placement only depends on the terrain, so one free tile of each kind is representative.
    let land = p.free_tiles().find(|t| !p.tiles[*t].water);
    let sea = p.free_tiles().find(|t| p.tiles[*t].water);
    world
        .defs
        .buildings
        .iter()
        .enumerate()
        .map(|(i, d)| (BuildingId::from_idx(i), d))
        .filter(|(_, d)| filter(d) && score(d) > 0.0)
        .filter(|(id, d)| {
            let tile = if d.water {
                sea
            } else if d.land {
                land
            } else {
                land.or(sea)
            };
            tile.is_some_and(|t| world.can_build(empire, pid, t, *id, budget).is_ok())
        })
        .max_by(|a, b| score(a.1).total_cmp(&score(b.1)).then(b.0.cmp(&a.0)))
        .map(|(id, _)| id)
}

pub(crate) fn decide(world: &World, empire: EmpireId) -> Vec<Command> {
    let e = world.empire(empire);
    let Some(core) = e.core_pool else {
        return Vec::new();
    };
    let core = &world.pools[core.idx()];
    let defs = &world.defs;
    let traits = e.ai.unwrap_or_default();
    let needs = rank_needs(world, empire, core, traits);
    let mut budget = e.storage.clone();
    let mut cmds = Vec::new();
    let max_builds = 2 + core.systems.len() / 4;

    let build = |cmds: &mut Vec<Command>,
                 budget: &mut Vec<f64>,
                 pid: PlanetId,
                 b: BuildingId,
                 r: Option<ResourceId>| {
        if let Some(tile) = best_tile(world, empire, pid, b, r) {
            for a in &defs.building(b).build_cost {
                budget[a.resource.idx()] -= a.count;
            }
            cmds.push(Command::Build {
                planet: pid,
                tile,
                building: b,
                empire: Some(empire),
            });
        }
    };

    let planets: Vec<PlanetId> = core.planets.clone();

    // Resources that some building already under construction will produce.
    let mut underway: Vec<ResourceId> = Vec::new();
    for &pid in &planets {
        for t in &world.planet(pid).tiles {
            if let Some(Construction::Building { def, .. }) = &t.construction {
                underway.extend(defs.building(*def).outputs.iter().map(|o| o.resource));
            }
        }
    }

    // Habitable worlds first, most populous first.
    let mut habitable: Vec<PlanetId> = planets
        .iter()
        .copied()
        .filter(|p| world.planet(*p).habitable)
        .collect();
    habitable.sort_by_key(|p| std::cmp::Reverse(world.planet(*p).pop));
    for &pid in &habitable {
        if cmds.len() >= max_builds {
            break;
        }
        let p = world.planet(pid);
        if p.is_constructing() || p.free_tiles().next().is_none() {
            continue;
        }
        let s = &p.stats;
        let crowded =
            s.homeless > 0 || (p.pop > 0 && s.housing - (p.pop as f64) < s.housing * 0.05 + 5.0);
        if crowded
            && let Some(b) = pick_building(
                world,
                empire,
                pid,
                &budget,
                |d| d.max_workers == 0,
                |d| d.housing,
            )
        {
            build(&mut cmds, &mut budget, pid, b, None);
            continue;
        }
        if s.amenities < s.amenities_needed * 0.9
            && p.pop > 0
            && let Some(b) = pick_building(
                world,
                empire,
                pid,
                &budget,
                |d| d.max_workers > 0,
                |d| d.amenities,
            )
        {
            build(&mut cmds, &mut budget, pid, b, None);
            continue;
        }
        // Build jobs for idle pops (or to attract settlers to an empty world). A shortage
        // justifies new jobs even at full employment, since scarcity pulls workers over, as
        // long as nothing producing that resource is already being built.
        let wants_jobs = s.unemployed > 0 || (s.jobs == 0 && s.free_jobs == 0);
        let list: Vec<ResourceId> = if wants_jobs {
            needs.all.clone()
        } else if p.pop > 0 {
            needs
                .urgent
                .iter()
                .copied()
                .filter(|r| !underway.contains(r))
                .collect()
        } else {
            Vec::new()
        };
        for r in list {
            if let Some(b) = pick_building(
                world,
                empire,
                pid,
                &budget,
                |d| d.max_workers > 0,
                |d| output_of(d, r),
            ) {
                build(&mut cmds, &mut budget, pid, b, Some(r));
                underway.extend(defs.building(b).outputs.iter().map(|o| o.resource));
                break;
            }
        }
    }

    // Automated buildings on barren worlds when the most urgent needs can be met that way.
    for &pid in planets.iter().filter(|p| !world.planet(**p).habitable) {
        if cmds.len() >= max_builds {
            break;
        }
        let p = world.planet(pid);
        if p.is_constructing() || p.terraform_days_left.is_some() {
            continue;
        }
        for &r in needs.all.iter().take(3) {
            if let Some(b) = pick_building(
                world,
                empire,
                pid,
                &budget,
                |d| d.barren,
                |d| output_of(d, r),
            ) {
                build(&mut cmds, &mut budget, pid, b, Some(r));
                break;
            }
        }
    }

    // One upgrade per decision, only with a comfortable surplus.
    'upgrade: for &pid in &habitable {
        let p = world.planet(pid);
        for (tile, t) in p.tiles.iter().enumerate() {
            let Some(inst) = &t.building else { continue };
            for &u in &defs.building(inst.def).available_upgrades {
                let cost = &defs.upgrade(u).cost;
                let rich: Vec<f64> = budget.iter().map(|b| b / 3.0).collect();
                if can_afford(&rich, cost) && world.can_upgrade(pid, tile, u, &budget).is_ok() {
                    for a in cost {
                        budget[a.resource.idx()] -= a.count;
                    }
                    cmds.push(Command::Upgrade {
                        planet: pid,
                        tile,
                        upgrade: u,
                    });
                    break 'upgrade;
                }
            }
        }
    }

    // One terraforming project at a time, large worlds first.
    let terraforming = planets
        .iter()
        .any(|p| world.planet(*p).terraform_days_left.is_some());
    if !terraforming {
        let mut candidates: Vec<PlanetId> = planets
            .iter()
            .copied()
            .filter(|p| !world.planet(*p).habitable)
            .collect();
        candidates.sort_by_key(|p| std::cmp::Reverse(world.planet(*p).tiles.len()));
        let rich: Vec<f64> = budget.iter().map(|b| b / 2.0).collect();
        if let Some(&pid) = candidates
            .iter()
            .find(|p| world.can_terraform(**p, &rich).is_ok())
        {
            cmds.push(Command::Terraform { planet: pid });
        }
    }

    clear_blocker(world, &planets, &mut budget, &mut cmds);
    expand(world, empire, traits, &mut budget, &mut cmds);
    choose_research(world, empire, traits, &mut cmds);
    let declared = diplomacy(world, empire, traits, &mut cmds);
    military(
        world,
        empire,
        traits,
        declared,
        &planets,
        &mut budget,
        &mut cmds,
    );
    cmds
}

/// Clears one natural blocker on a crowded world (no free tile left), most populous first,
/// when the removal cost is comfortably affordable.
fn clear_blocker(world: &World, planets: &[PlanetId], budget: &mut [f64], cmds: &mut Vec<Command>) {
    let mut crowded: Vec<PlanetId> = planets
        .iter()
        .copied()
        .filter(|p| world.planet(*p).free_tiles().next().is_none())
        .collect();
    crowded.sort_by_key(|p| (std::cmp::Reverse(world.planet(*p).pop), *p));
    let half: Vec<f64> = budget.iter().map(|b| b / 2.0).collect();
    for pid in crowded {
        let p = world.planet(pid);
        let cheapest = p
            .tiles
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.construction.is_none()
                    && t.building
                        .as_ref()
                        .is_some_and(|b| world.defs.building(b.def).blocker)
            })
            .filter_map(|(i, _)| world.demolish_plan(pid, i).ok().map(|(_, cost)| (i, cost)))
            .filter(|(_, cost)| can_afford(&half, cost))
            .min_by(|a, b| {
                cost_value(world, &a.1)
                    .total_cmp(&cost_value(world, &b.1))
                    .then(a.0.cmp(&b.0))
            });
        if let Some((tile, cost)) = cheapest {
            for a in &cost {
                budget[a.resource.idx()] -= a.count;
            }
            cmds.push(Command::Demolish { planet: pid, tile });
            return;
        }
    }
}

/// Total cost of a resource list, weighted by market value (for picking cheap options).
fn cost_value(world: &World, cost: &[crate::defs::ResourceAmount]) -> f64 {
    cost.iter()
        .map(|a| a.count * world.defs.resource(a.resource).base_value)
        .sum()
}

/// Claims nearby unowned systems with cheap buildings. Expansionist empires reach further,
/// keep less influence in reserve and run more claims at once.
fn expand(
    world: &World,
    empire: EmpireId,
    traits: AiPersonality,
    budget: &mut [f64],
    cmds: &mut Vec<Command>,
) {
    let rules = world.rules();
    let Some(influence) = rules.expansion_resource else {
        return;
    };
    if traits.expansionist < 0.05 {
        return;
    }
    let max_jumps = 1 + (traits.expansionist * 2.0).round() as u32;
    let max_sites = 1 + (traits.expansionist * 2.0).floor() as usize;
    let reserve = 2.0 - traits.expansionist;
    let dist = world.jumps_from_borders(empire);
    let mut pending = 0;
    let mut candidates: Vec<(i64, SystemId)> = Vec::new();
    for (i, s) in world.systems.iter().enumerate() {
        let d = dist[i];
        if s.owner.is_some() || d == 0 || d > max_jumps + 1 || s.planet_range().is_empty() {
            continue;
        }
        let mut staked = false;
        let mut score = 0i64;
        for p in s.planet_ids().map(|p| world.planet(p)) {
            staked |= p.tiles.iter().any(|t| {
                matches!(t.construction, Some(Construction::Building { empire: Some(e), .. }) if e == empire)
            });
            score += if p.habitable { 6 } else { 1 };
        }
        if staked {
            pending += 1;
            continue;
        }
        let (cost, _) = rules.expansion(d);
        if d <= max_jumps && budget[influence.idx()] >= cost * reserve {
            candidates.push((score - d as i64 * 4, SystemId::from_idx(i)));
        }
    }
    if pending >= max_sites {
        return;
    }
    candidates.sort_by_key(|(score, s)| (std::cmp::Reverse(*score), *s));
    for (_, sys) in candidates {
        let mut planets: Vec<PlanetId> = world.system(sys).planet_ids().collect();
        planets.sort_by_key(|p| {
            (
                !world.planet(*p).habitable,
                std::cmp::Reverse(world.planet(*p).tiles.len()),
            )
        });
        for pid in planets {
            let best = world
                .planet(pid)
                .free_tiles()
                .flat_map(|tile| {
                    (0..world.defs.buildings.len()).map(move |b| (tile, BuildingId::from_idx(b)))
                })
                .filter_map(|(tile, b)| {
                    world
                        .can_build_with(empire, pid, tile, b, budget, Some(&dist))
                        .ok()
                        .map(|plan| (tile, b, plan))
                })
                .min_by(|a, b| {
                    cost_value(world, &a.2.cost)
                        .total_cmp(&cost_value(world, &b.2.cost))
                        .then(a.0.cmp(&b.0))
                        .then(a.1.cmp(&b.1))
                });
            if let Some((tile, b, plan)) = best {
                for a in &plan.cost {
                    budget[a.resource.idx()] -= a.count;
                }
                cmds.push(Command::Build {
                    planet: pid,
                    tile,
                    building: b,
                    empire: Some(empire),
                });
                return;
            }
        }
    }
}

/// Picks the next technology when nothing is being researched: cheapest first, with military
/// research discounted for militant empires and everything else for scientific ones.
fn choose_research(
    world: &World,
    empire: EmpireId,
    traits: AiPersonality,
    cmds: &mut Vec<Command>,
) {
    let e = world.empire(empire);
    if e.researching.is_some() && e.research_progress > 0.0 {
        return;
    }
    let defs = &world.defs;
    let weight = |t: TechId| {
        let tech = defs.tech(t);
        let military = tech.category.as_deref() == Some("military");
        let w = if military {
            1.0 + 2.0 * traits.militant
        } else {
            1.0 + traits.scientific
        };
        tech.cost / w
    };
    let pick = (0..defs.technologies.len())
        .map(TechId::from_idx)
        .filter(|t| defs.tech_available(*t, &e.techs))
        .min_by(|a, b| weight(*a).total_cmp(&weight(*b)).then(a.cmp(b)));
    if pick.is_some() && pick != e.researching {
        cmds.push(Command::SetResearch { empire, tech: pick });
    }
}

/// Deterministic pseudo-random number in `[0, 1)` for a decision, so runs are reproducible.
fn roll(world: &World, salt: &[u64]) -> f64 {
    let mut x = world.seed ^ world.day.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for s in salt {
        x = (x ^ s.wrapping_mul(0xBF58_476D_1CE4_E5B9)).rotate_left(29);
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    }
    x ^= x >> 31;
    (x >> 11) as f64 / (1u64 << 53) as f64
}

/// Current fighting strength of an empire's fleets (damaged ships count for less).
fn fleet_power(world: &World, empire: EmpireId) -> f64 {
    world
        .fleets
        .iter()
        .filter(|f| f.owner == empire)
        .flat_map(|f| &f.ships)
        .map(|s| ship_power(world, s.def) * (0.3 + 0.7 * s.hull / world.defs.ship(s.def).max_hull))
        .sum()
}

/// War and peace. Militant empires declare war on reachable neighbours they believe they can
/// beat, and fight longer before accepting peace. Every empire sues for peace when it is
/// clearly losing, once its enemy is gone, or when the war has dragged on past its patience.
fn diplomacy(
    world: &World,
    empire: EmpireId,
    traits: AiPersonality,
    cmds: &mut Vec<Command>,
) -> Option<EmpireId> {
    let e = world.empire(empire);
    let day = world.day;
    let mine = fleet_power(world, empire);
    for w in &e.wars {
        let theirs = fleet_power(world, w.enemy);
        let days = day.saturating_sub(w.since_day) as f64;
        let enemy_gone = world.empire_planets(w.enemy).is_empty();
        let losing = days > 120.0 && mine < theirs * 0.6;
        let weary = days > 360.0 + 1440.0 * traits.militant;
        if enemy_gone || losing || weary {
            cmds.push(Command::MakePeace {
                empire,
                target: w.enemy,
            });
        }
    }

    let max_wars = if traits.militant > 0.75 { 2 } else { 1 };
    if traits.militant < 0.05 || e.wars.len() >= max_wars || mine <= 0.0 {
        return None;
    }
    // Neighbours: empires holding a system within three jumps of our borders.
    let dist = world.jumps_from_borders(empire);
    let needed = 1.8 - 1.2 * traits.militant;
    let target = (0..world.empires.len())
        .map(EmpireId::from_idx)
        .filter(|o| *o != empire && !e.at_war_with(*o) && !e.in_truce(*o, day))
        .filter(|o| !world.empire_planets(*o).is_empty())
        .filter(|o| world.empire_systems(*o).iter().any(|s| dist[s.idx()] <= 3))
        .map(|o| (o, mine / fleet_power(world, o).max(1.0)))
        .filter(|(_, ratio)| *ratio >= needed)
        .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
    if let Some((target, _)) = target
        && roll(world, &[empire.0 as u64, target.0 as u64]) < 0.25 + 0.5 * traits.militant
    {
        cmds.push(Command::DeclareWar { empire, target });
        return Some(target);
    }
    None
}

/// A rough measure of a ship's fighting strength.
fn ship_power(world: &World, ship: ShipId) -> f64 {
    let d = world.defs.ship(ship);
    let dps: f64 = d
        .armament
        .iter()
        .map(|a| (a.hull_damage + a.shield_damage) / a.cooldown)
        .sum();
    d.max_hull + d.max_shield + dps * 20.0
}

/// Keeps a fleet sized to the empire and its temperament, defends its systems from invaders,
/// and goes on the offensive when at war: idle fleets rally and merge, and the main fleet
/// bombards the nearest enemy world once it is big enough (sooner for militant empires).
/// `declared` is a war declared this decision, which the offensive already counts.
fn military(
    world: &World,
    empire: EmpireId,
    traits: AiPersonality,
    declared: Option<EmpireId>,
    planets: &[PlanetId],
    budget: &mut [f64],
    cmds: &mut Vec<Command>,
) {
    let e = world.empire(empire);
    let hostile = |o: EmpireId| world.at_war(empire, o) || declared == Some(o);
    let at_war = !e.wars.is_empty() || declared.is_some();
    let own_fleets: Vec<&crate::world::Fleet> =
        world.fleets.iter().filter(|f| f.owner == empire).collect();
    let ships: usize = own_fleets.iter().map(|f| f.ships.len()).sum::<usize>() + e.ship_queue.len();

    // A shipyard on the most populous world once the empire can afford to arm itself.
    let shipyard = planets.iter().copied().find(|p| world.has_shipyard(*p));
    if shipyard.is_none() {
        let mut habitable: Vec<PlanetId> = planets
            .iter()
            .copied()
            .filter(|p| world.planet(*p).habitable)
            .collect();
        habitable.sort_by_key(|p| std::cmp::Reverse(world.planet(*p).pop));
        for pid in habitable.into_iter().take(1) {
            if world.planet(pid).is_constructing() {
                continue;
            }
            let shipyard_like = |d: &BuildingDef| {
                d.outputs
                    .iter()
                    .any(|o| world.defs.resource(o.resource).on_demand)
            };
            if let Some(b) = pick_building(world, empire, pid, budget, shipyard_like, |_| 1.0)
                && let Some(tile) = best_tile(world, empire, pid, b, None)
            {
                for a in &world.defs.building(b).build_cost {
                    budget[a.resource.idx()] -= a.count;
                }
                cmds.push(Command::Build {
                    planet: pid,
                    tile,
                    building: b,
                    empire: Some(empire),
                });
            }
        }
    }

    // Ship production: the strongest design affordable with half the budget. Militant empires
    // keep bigger fleets and a deeper queue; everyone doubles up in wartime.
    let base = (3 + 2 * world.empire_systems(empire).len()) as f64 * (0.4 + 1.2 * traits.militant);
    let wanted = (base.round() as usize).clamp(2, 40) * if at_war { 2 } else { 1 };
    let queue_depth = 1 + usize::from(traits.militant > 0.6 || at_war);
    if let Some(yard) = shipyard
        && e.ship_queue.len() < queue_depth
        && ships < wanted
    {
        let half: Vec<f64> = budget.iter().map(|b| b / 2.0).collect();
        let best = (0..world.defs.ships.len())
            .map(ShipId::from_idx)
            .filter(|s| world.can_build_ship(yard, *s, &half).is_ok())
            .max_by(|a, b| {
                ship_power(world, *a)
                    .total_cmp(&ship_power(world, *b))
                    .then(b.cmp(a))
            });
        if let Some(ship) = best {
            if let Ok(upfront) = world.can_build_ship(yard, ship, budget) {
                for a in upfront {
                    budget[a.resource.idx()] -= a.count;
                }
            }
            cmds.push(Command::BuildShip {
                planet: yard,
                ship,
                fleet: None,
            });
        }
    }

    // Defence: send idle fleets to own systems where enemy fleets are present.
    let mut busy: Vec<FleetId> = Vec::new();
    let threatened: Vec<SystemId> = world
        .fleets
        .iter()
        .filter(|f| hostile(f.owner))
        .map(|f| f.system)
        .filter(|s| world.system(*s).owner == Some(empire))
        .collect();
    for sys in threatened {
        if own_fleets.iter().any(|f| {
            f.system == sys || matches!(f.order, FleetOrder::Move { system, .. } if system == sys)
        }) {
            continue;
        }
        let dist = world.distances_from([sys]);
        if let Some(f) = own_fleets
            .iter()
            .filter(|f| {
                f.order == FleetOrder::Idle
                    && !busy.contains(&f.id)
                    && dist[f.system.idx()] != u32::MAX
            })
            .min_by_key(|f| (dist[f.system.idx()], f.id))
        {
            busy.push(f.id);
            cmds.push(Command::MoveFleet {
                fleet: f.id,
                system: sys,
                coordinates: None,
            });
        }
    }

    if !at_war {
        return;
    }
    let idle: Vec<&&crate::world::Fleet> = own_fleets
        .iter()
        .filter(|f| f.order == FleetOrder::Idle && !busy.contains(&f.id))
        .collect();

    // Merge idle fleets that share a system into the largest one.
    for f in &idle {
        if let Some(big) = idle
            .iter()
            .filter(|o| o.system == f.system)
            .max_by_key(|o| (o.ships.len(), std::cmp::Reverse(o.id)))
            && big.id != f.id
        {
            busy.push(f.id);
            cmds.push(Command::MergeFleets {
                into: big.id,
                from: f.id,
            });
        }
    }

    // Offence: the largest idle fleet bombards the nearest enemy world once big enough.
    let attacking = own_fleets
        .iter()
        .any(|f| matches!(f.order, FleetOrder::Bombard { .. }));
    let threshold = (6.0 - 4.0 * traits.militant).round().max(2.0) as usize;
    let main = idle
        .iter()
        .filter(|f| !busy.contains(&f.id))
        .max_by_key(|f| (f.ships.len(), std::cmp::Reverse(f.id)));
    if !attacking
        && let Some(fleet) = main
        && fleet.ships.len() >= threshold
        && fleet.max_range(&world.defs) > 0.0
    {
        let dist = world.distances_from([fleet.system]);
        let target = world
            .planets
            .iter()
            .enumerate()
            .filter(|(_, p)| p.owner.is_some_and(hostile))
            .filter(|(_, p)| dist[p.system.idx()] != u32::MAX)
            .min_by_key(|(i, p)| {
                // Finish off systems where we already hold a planet before moving on.
                let foothold = world
                    .system(p.system)
                    .planet_ids()
                    .any(|q| world.planet(q).owner == Some(empire));
                (
                    dist[p.system.idx()],
                    !foothold,
                    std::cmp::Reverse(p.pop),
                    *i,
                )
            })
            .map(|(i, _)| PlanetId::from_idx(i));
        if let Some(planet) = target {
            busy.push(fleet.id);
            cmds.push(Command::Bombard {
                fleet: fleet.id,
                planet,
                tile: None,
            });
        }
    }

    // Rally the remaining idle fleets at the capital so they can merge next time.
    if let Some(capital) = e.capital {
        for f in idle
            .iter()
            .filter(|f| !busy.contains(&f.id) && f.system != capital)
        {
            cmds.push(Command::MoveFleet {
                fleet: f.id,
                system: capital,
                coordinates: None,
            });
        }
    }
}
