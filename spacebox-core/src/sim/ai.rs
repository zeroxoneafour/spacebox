//! A deliberately simple governor for AI empires (those with an `[empire.x.ai]` table).
//!
//! Every `ai_interval_days` (staggered per empire) it looks at what the empire is short of and
//! issues a handful of ordinary [`Command`]s: housing where pops are crowded, job buildings
//! where they are unemployed (or on empty habitable worlds, to attract settlers), automated
//! barren-world buildings, the occasional upgrade and terraforming project. Decisions only read
//! the world, so all empires decide in parallel.

use std::collections::HashMap;

use crate::defs::{BuildingDef, ResourceAmount};
use crate::ids::*;
use crate::sim::commands::{Command, can_afford};
use crate::sim::economy::next_research;

use crate::world::{
    AiPersonality, Fleet, FleetOrder, ProjectKind, Side, WarGoal, WarOutcome, World,
};

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
    let has = |list: &[ResourceAmount]| list.iter().any(|a| a.resource == r);
    let known = |techs: &[TechId]| techs.iter().all(|t| e.knows(*t));
    defs.buildings.iter().any(|b| {
        !b.blocker
            && known(&b.requires_tech)
            && (has(&b.build_cost.resources) || has(&b.inputs) || has(&b.upkeep))
    }) || defs.upgrades.iter().any(|u| {
        known(&u.requires_tech)
            && (has(&u.build_cost.resources)
                || has(&u.modifier.input_add)
                || has(&u.modifier.upkeep_add))
    }) || [
        &world.rules().terraform_large,
        &world.rules().terraform_small,
    ]
    .into_iter()
    .flatten()
    .any(|t| t.requires_tech.is_none_or(|t| e.knows(t)) && has(&t.cost.resources))
}

/// Whether some project of the empire still needs transient resource `r`.
fn projects_need(world: &World, empire: EmpireId, r: ResourceId) -> bool {
    let e = world.empire(empire);
    e.research
        .iter()
        .map(|p| &p.remaining)
        .chain(e.shipbuilding.iter().map(|p| &p.remaining))
        .any(|c| c.get(r) > 0.0)
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
            .any(|b| !b.blocker && output_of(b, r) > 0.0)
        {
            continue;
        }
        let income = e.income[i];
        let demand = pool.demand_full[i];
        if res.transient {
            // Transient resources are only worth producing while projects use all of them.
            // Research is always wanted, by scientific empires much more so.
            let research = defs
                .technologies
                .iter()
                .any(|t| t.cost.iter().any(|a| a.resource == r));
            if e.wasted[i] > 1e-6 || !projects_need(world, empire, r) {
                continue;
            }
            let days = if research {
                90.0 * (1.0 - traits.scientific) + 5.0
            } else {
                60.0 * (1.0 - traits.militant) + 5.0
            };
            rest.push((days, r));
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
    def.outputs.get(r) + def.passive_outputs.get(r)
}

fn has_jobs(def: &BuildingDef) -> bool {
    def.max_workers.is_some_and(|w| w > 0)
}

/// Amenities a building provides (from workers and passively).
fn amenities_of(def: &BuildingDef) -> f64 {
    def.outputs.amenities + def.passive_outputs.amenities
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
                            .any(|a| defs.targets(&a.targets, nb.def))
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
            let tile = if d.land && land.is_some() {
                land
            } else if d.water {
                sea
            } else {
                None
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
            for a in &defs.building(b).build_cost.resources {
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
            if let Some(def) = t.construction.as_ref().and_then(|c| c.building()) {
                underway.extend(
                    defs.building(def)
                        .outputs
                        .resources
                        .iter()
                        .map(|o| o.resource),
                );
            }
        }
    }

    // Habitable worlds first, most populous first.
    let mut habitable: Vec<PlanetId> = planets
        .iter()
        .copied()
        .filter(|p| world.planet(*p).habitable)
        .collect();
    habitable.sort_by_key(|p| std::cmp::Reverse(world.planet(*p).pop()));
    for &pid in &habitable {
        if cmds.len() >= max_builds {
            break;
        }
        let p = world.planet(pid);
        if p.is_constructing() || p.free_tiles().next().is_none() {
            continue;
        }
        let s = &p.stats;
        let pop = p.pop();
        // Worlds have no housing until it is built, and pops can't settle without it.
        let crowded = s.homeless > 0
            || s.housing <= 0.0
            || (pop > 0 && s.housing - (pop as f64) < s.housing * 0.05 + 5.0);
        if crowded
            && let Some(b) =
                pick_building(world, empire, pid, &budget, |d| !has_jobs(d), |d| d.housing)
        {
            build(&mut cmds, &mut budget, pid, b, None);
            continue;
        }
        if s.amenities < s.amenities_needed * 0.9
            && pop > 0
            && let Some(b) = pick_building(world, empire, pid, &budget, has_jobs, amenities_of)
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
        } else if pop > 0 {
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
            if let Some(b) =
                pick_building(world, empire, pid, &budget, has_jobs, |d| output_of(d, r))
            {
                build(&mut cmds, &mut budget, pid, b, Some(r));
                underway.extend(
                    defs.building(b)
                        .outputs
                        .resources
                        .iter()
                        .map(|o| o.resource),
                );
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
        if p.is_constructing() || p.terraform.is_some() {
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
                let cost = &defs.upgrade(u).build_cost.resources;
                let rich: Vec<f64> = budget.iter().map(|b| b / 3.0).collect();
                if can_afford(defs, &rich, cost) && world.can_upgrade(pid, tile, u).is_ok() {
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
    let terraforming = planets.iter().any(|p| world.planet(*p).terraform.is_some());
    if !terraforming {
        let mut candidates: Vec<PlanetId> = planets
            .iter()
            .copied()
            .filter(|p| !world.planet(*p).habitable)
            .collect();
        candidates.sort_by_key(|p| std::cmp::Reverse(world.planet(*p).tiles.len()));
        let rich: Vec<f64> = budget.iter().map(|b| b / 2.0).collect();
        let affordable = |p: PlanetId| {
            world
                .rules()
                .terraform(world.planet(p).size)
                .is_some_and(|r| can_afford(defs, &rich, &r.cost.resources))
        };
        if let Some(&pid) = candidates
            .iter()
            .find(|p| world.can_terraform(**p).is_ok() && affordable(**p))
        {
            cmds.push(Command::Terraform { planet: pid });
        }
    }

    clear_blocker(world, &planets, &mut budget, &mut cmds);
    // Diplomacy spends influence before expansion does, so wars and claims get funded.
    let declared = diplomacy(world, empire, traits, &mut budget, &mut cmds);
    expand(world, empire, traits, &mut budget, &mut cmds);
    choose_research(world, empire, traits, &mut cmds);
    military(
        world,
        empire,
        traits,
        declared,
        &planets,
        &mut budget,
        &mut cmds,
    );
    toggle_transient(world, empire, &planets, &mut cmds);
    cmds
}

/// Transient output that no project uses is wasted, so buildings that only produce transient
/// resources (research labs, shipyards) are switched off while nothing needs what they make,
/// and back on once something does (including projects started by this decision).
fn toggle_transient(
    world: &World,
    empire: EmpireId,
    planets: &[PlanetId],
    cmds: &mut Vec<Command>,
) {
    let defs = &world.defs;
    let e = world.empire(empire);
    // Research uses the empire's transient output; ships only that of their own planet.
    let mut needed = vec![false; defs.resource_count()];
    let mut local: Vec<(PlanetId, ResourceId)> = Vec::new();
    let mut need = |c: &[ResourceAmount]| {
        for a in c.iter().filter(|a| a.count > 0.0) {
            needed[a.resource.idx()] = true;
        }
    };
    for p in &e.research {
        need(&p.remaining.resources);
    }
    for c in cmds.iter() {
        if let Command::Research { tech, .. } = c {
            need(&defs.tech(*tech).cost);
        }
    }
    let mut ship_need = |planet: PlanetId, c: &[ResourceAmount]| {
        for a in c.iter().filter(|a| a.count > 0.0) {
            local.push((planet, a.resource));
        }
    };
    for p in &e.shipbuilding {
        ship_need(p.planet, &p.remaining.resources);
    }
    for c in cmds.iter() {
        if let Command::BuildShip { planet, ship } = c {
            ship_need(*planet, &defs.ship(*ship).build_cost.resources);
        }
    }
    let mut toggles = Vec::new();
    for &pid in planets {
        for (tile, t) in world.planet(pid).tiles.iter().enumerate() {
            let Some(b) = &t.building else { continue };
            let d = defs.building(b.def);
            let outputs: Vec<ResourceId> = d
                .outputs
                .resources
                .iter()
                .chain(&d.passive_outputs.resources)
                .filter(|a| a.count > 0.0)
                .map(|a| a.resource)
                .collect();
            let only_transient = !outputs.is_empty()
                && d.outputs.amenities == 0.0
                && d.passive_outputs.amenities == 0.0
                && d.housing == 0.0
                && outputs.iter().all(|r| defs.is_transient(*r));
            if !only_transient {
                continue;
            }
            let wanted = outputs
                .iter()
                .any(|r| needed[r.idx()] || local.contains(&(pid, *r)));
            if b.enabled != wanted {
                toggles.push(Command::SetEnabled {
                    planet: pid,
                    tile,
                    enabled: wanted,
                });
            }
        }
    }
    cmds.extend(toggles);
}

/// Clears one natural blocker on a crowded world (no free tile left), most populous first,
/// when the removal cost is comfortably affordable.
fn clear_blocker(world: &World, planets: &[PlanetId], budget: &mut [f64], cmds: &mut Vec<Command>) {
    let mut crowded: Vec<PlanetId> = planets
        .iter()
        .copied()
        .filter(|p| world.planet(*p).free_tiles().next().is_none())
        .collect();
    crowded.sort_by_key(|p| (std::cmp::Reverse(world.planet(*p).pop()), *p));
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
            .filter_map(|(i, _)| {
                world
                    .demolish_plan(pid, i)
                    .ok()
                    .map(|(_, cost)| (i, cost.map(|c| c.resources).unwrap_or_default()))
            })
            .filter(|(_, cost)| can_afford(&world.defs, &half, cost))
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
fn cost_value(world: &World, cost: &[ResourceAmount]) -> f64 {
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
    let (dist, reached) = world.jumps_within(empire, max_jumps + 1);
    let mut pending = 0;
    let mut candidates: Vec<(i64, SystemId)> = Vec::new();
    for &sid in &reached {
        let (i, s) = (sid.idx(), world.system(sid));
        let d = dist[i];
        if s.owner.is_some() || d == 0 || s.planet_range().is_empty() {
            continue;
        }
        let mut staked = false;
        let mut score = 0i64;
        for p in s.planet_ids().map(|p| world.planet(p)) {
            staked |= p.tiles.iter().any(|t| {
                t.construction.as_ref().is_some_and(|c| {
                    c.empire == Some(empire) && matches!(c.kind, ProjectKind::Building(_))
                })
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
    // Buildings the empire could put anywhere, cheapest first.
    let e = world.empire(empire);
    let mut constructible: Vec<BuildingId> = (0..world.defs.buildings.len())
        .map(BuildingId::from_idx)
        .filter(|b| {
            let d = world.defs.building(*b);
            !d.blocker && (d.land || d.water) && d.requires_tech.iter().all(|t| e.knows(*t))
        })
        .collect();
    constructible.sort_by(|a, b| {
        let cost =
            |x: &BuildingId| cost_value(world, &world.defs.building(*x).build_cost.resources);
        cost(a).total_cmp(&cost(b)).then(a.cmp(b))
    });
    for (_, sys) in candidates {
        let mut planets: Vec<PlanetId> = world.system(sys).planet_ids().collect();
        planets.sort_by_key(|p| {
            (
                !world.planet(*p).habitable,
                std::cmp::Reverse(world.planet(*p).tiles.len()),
            )
        });
        for pid in planets {
            let p = world.planet(pid);
            // Cheapest building first (the expansion surcharge is the same for all of them),
            // on the first free tile whose terrain suits it.
            let best = constructible.iter().find_map(|&b| {
                let d = world.defs.building(b);
                if !p.habitable && !d.barren {
                    return None;
                }
                let tile = p.free_tiles().find(|t| d.fits_terrain(p.tiles[*t].water))?;
                world
                    .can_build_with(empire, pid, tile, b, budget, Some(&dist))
                    .ok()
                    .map(|plan| (tile, b, plan))
            });
            if let Some((tile, b, plan)) = best {
                for a in &plan.cost.resources {
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

/// Keeps enough technologies in research that the empire's research output is not wasted
/// before the next decision (and a few more for scientific empires), picking the cheapest
/// available, with military research discounted for militant empires and everything else for
/// scientific ones.
fn choose_research(
    world: &World,
    empire: EmpireId,
    traits: AiPersonality,
    cmds: &mut Vec<Command>,
) {
    let e = world.empire(empire);
    let defs = &world.defs;
    let wanted = 1 + (traits.scientific * 2.0).round() as usize;
    // Transient output that research could use over the next two decision intervals.
    let horizon = 2.0 * world.rules().ai_interval_days as f64;
    let research_resources: Vec<ResourceId> = defs
        .technologies
        .iter()
        .flat_map(|t| t.cost.iter().map(|a| a.resource))
        .filter(|r| defs.is_transient(*r))
        .collect();
    let output: f64 = research_resources
        .iter()
        .map(|r| e.income[r.idx()] * world.rules().research_speed_mult)
        .sum::<f64>()
        * horizon;
    let queued = |e: &crate::world::Empire| -> f64 {
        e.research
            .iter()
            .flat_map(|p| p.remaining.resources.iter())
            .filter(|a| research_resources.contains(&a.resource))
            .map(|a| a.count)
            .sum()
    };
    let weight = |t: TechId| {
        if defs
            .tech(t)
            .tags
            .iter()
            .any(|g| defs.tag_name(*g) == "military")
        {
            1.0 + 2.0 * traits.militant
        } else {
            1.0 + traits.scientific
        }
    };
    // Plan on a copy so several technologies can be picked at once.
    let mut plan = e.clone();
    while plan.research.len() < wanted || queued(&plan) < output {
        let Some(tech) = next_research(&plan, defs, weight) else {
            break;
        };
        plan.research.push(crate::world::ResearchProject {
            tech,
            paid: Vec::new(),
            remaining: defs.research_cost(tech),
        });
        cmds.push(Command::Research { empire, tech });
        if plan.research.len() >= 8 {
            break;
        }
    }
}

/// Deterministic pseudo-random number in `[0, 1)` for a decision, so runs are reproducible.
fn roll(world: &World, salt: &[u64]) -> f64 {
    let mut all = vec![world.day];
    all.extend_from_slice(salt);
    crate::rng::hash01(world.seed, &all)
}

/// Current fighting strength of an empire's fleets (damaged ships count for less).
/// Fighting strength of every empire's fleets (one pass over all fleets).
fn fleet_powers(world: &World) -> Vec<f64> {
    let mut power = vec![0.0; world.empires.len()];
    for f in &world.fleets {
        for s in &f.ships {
            let max = world.ship_stats(f.owner, s.def).max_hull;
            power[f.owner.idx()] += ship_power(world, f.owner, s.def) * (0.3 + 0.7 * s.hull / max);
        }
    }
    power
}

/// War and peace.
///
/// The total-war policy follows militancy. Militant empires claim neighbouring systems and
/// declare war on reachable neighbours they believe they can beat: a claim war if they hold
/// claims on them, a total war if their policy allows it and they are very militant, and a
/// humiliation war otherwise. Defenders pick their goal the same way. A side forces its
/// victory once the enemy is exhausted, offers a status quo when losing or weary (sooner
/// for peaceful empires), concedes when exhausted, and accepts offers at least that good.
fn diplomacy(
    world: &World,
    empire: EmpireId,
    traits: AiPersonality,
    budget: &mut [f64],
    cmds: &mut Vec<Command>,
) -> Option<EmpireId> {
    let e = world.empire(empire);
    let day = world.day;
    let rules = world.rules();
    let power = fleet_powers(world);
    let side_power = |members: &[EmpireId]| members.iter().map(|m| power[m.idx()]).sum::<f64>();
    let total_war = traits.militant >= 0.75;
    if e.total_war != total_war {
        cmds.push(Command::SetPolicy {
            empire,
            accepts_migrants: None,
            market_access: None,
            total_war: Some(total_war),
        });
    }
    let goal_for = |enemies: &[EmpireId], total_ok: bool| {
        if e.claims
            .iter()
            .any(|c| world.system(*c).owner.is_some_and(|o| enemies.contains(&o)))
        {
            WarGoal::Claim
        } else if total_ok && traits.militant >= 0.85 {
            WarGoal::Total
        } else {
            WarGoal::Humiliate
        }
    };

    let mut active = 0;
    for (war, side) in world.active_wars(empire) {
        active += 1;
        let w = world.war(war);
        let other = side.other();
        let enemies = world.members(war, other);
        let ours = side_power(&world.members(war, side));
        let theirs = side_power(&enemies);
        if side == Side::Defense && w.goals[side.idx()].is_none() {
            let total_ok = w.goals[other.idx()] == Some(WarGoal::Total) || total_war;
            let goal = goal_for(&enemies, total_ok);
            if world.can_set_war_goal(empire, war, goal).is_ok() {
                cmds.push(Command::SetWarGoal { empire, war, goal });
            }
        }
        let (mine, theirs_ex) = (w.exhaustion[side.idx()], w.exhaustion[other.idx()]);
        let days = day.saturating_sub(w.starting_day);
        let losing = mine > theirs_ex + 0.25 || (days > 120 && ours < theirs * 0.6);
        let weary = mine > 0.4 + 0.5 * traits.militant;
        let beaten = mine >= 0.9 && losing;
        // Whether a victory now would get what the war is for: every claim on the enemy
        // occupied (or already ours) in a claim war; in a total war, as much as the empire
        // cares to take before it tires.
        let allies = world.members(war, side);
        let achieved = weary
            || match w.goals[side.idx()] {
                Some(WarGoal::Claim) => allies.iter().all(|a| {
                    world.empire(*a).claims.iter().all(|c| {
                        let s = world.system(*c);
                        !s.owner.is_some_and(|o| enemies.contains(&o))
                            || s.occupier.is_some_and(|o| allies.contains(&o))
                    })
                }),
                Some(WarGoal::Total) => mine > 0.2 + 0.4 * traits.militant,
                _ => true,
            };
        if theirs_ex >= 1.0 && achieved {
            cmds.push(Command::EnforcePeace {
                empire,
                war,
                outcome: WarOutcome::victory(side),
            });
            continue;
        }
        // The best outcome this empire settles for.
        let acceptable = |o: WarOutcome| match o.winner() {
            Some(s) if s == side => achieved,
            None => losing || weary,
            Some(_) => beaten,
        };
        if let Some(offer) = w.proposals[other.idx()]
            && acceptable(offer)
        {
            cmds.push(Command::AcceptPeace { empire, war });
            continue;
        }
        let want = if beaten {
            Some(WarOutcome::victory(other))
        } else if losing || weary {
            Some(WarOutcome::StatusQuo)
        } else if theirs_ex > 0.75 && achieved {
            Some(WarOutcome::victory(side))
        } else {
            None
        };
        if let Some(outcome) = want
            && w.proposals[side.idx()] != Some(outcome)
        {
            cmds.push(Command::ProposePeace {
                empire,
                war,
                outcome,
            });
        }
    }

    alliances(world, empire, cmds);

    let mine = power[empire.idx()];
    if traits.militant < 0.05 || mine <= 0.0 {
        return None;
    }
    let influence = rules.expansion_resource;
    let spend = |budget: &mut [f64], cost: f64| {
        if let Some(r) = influence {
            budget[r.idx()] -= cost;
        }
    };
    let can_pay = |budget: &[f64], cost: f64| influence.is_none_or(|r| budget[r.idx()] >= cost);
    // Neighbours: empires holding a system within three jumps of our borders.
    let (dist, _) = world.jumps_within(empire, 3);
    let peaceful = |o: EmpireId| {
        o != empire
            && !world.at_war(empire, o)
            && !world.in_truce(empire, o)
            && !world.allied(empire, o)
    };

    // Claims on adjacent systems of neighbours, to fight for later.
    let max_claims = (traits.militant * 4.0).round() as usize;
    if e.claims.len() < max_claims
        && roll(world, &[empire.0 as u64, 0xC1A1]) < traits.militant * 0.5
        && let Some((sys, cost)) = world
            .systems
            .iter()
            .enumerate()
            .filter(|(i, s)| dist[*i] == 1 && s.owner.is_some_and(peaceful))
            .filter_map(|(i, _)| {
                let sid = SystemId::from_idx(i);
                world.claim_cost(empire, sid).ok().map(|c| (sid, c))
            })
            .max_by_key(|(sid, _)| {
                let pop: u64 = world
                    .system(*sid)
                    .planet_ids()
                    .map(|p| world.planet(p).pop())
                    .sum();
                (pop, std::cmp::Reverse(*sid))
            })
        && can_pay(budget, cost)
    {
        spend(budget, cost);
        cmds.push(Command::Claim {
            empire,
            system: sys,
        });
    }

    let max_wars = if traits.militant > 0.75 { 2 } else { 1 };
    if active >= max_wars {
        return None;
    }
    let needed = 1.8 - 1.2 * traits.militant;
    let target = (0..world.empires.len())
        .map(EmpireId::from_idx)
        .filter(|o| peaceful(*o))
        .filter(|o| !world.empire_planets(*o).is_empty())
        .filter(|o| world.empire_systems(*o).iter().any(|s| dist[s.idx()] <= 3))
        .map(|o| {
            let [_, def] = world.war_sides(empire, o);
            (o, mine / side_power(&def).max(1.0))
        })
        .filter(|(_, ratio)| *ratio >= needed)
        .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
    if let Some((target, _)) = target
        && roll(world, &[empire.0 as u64, target.0 as u64]) < 0.25 + 0.5 * traits.militant
    {
        let [_, def] = world.war_sides(empire, target);
        // Policy changes take effect with this tick's commands, before the declaration.
        let goal = goal_for(&def, total_war);
        let cost = rules.war_cost(goal);
        if can_pay(budget, cost) {
            spend(budget, cost);
            cmds.push(Command::DeclareWar {
                empire,
                target,
                goal,
            });
            return Some(target);
        }
    }
    None
}

/// Alliances: an empire at war invites those fighting one of its enemies into its alliance
/// (founding one if it has none), and accepts invitations into alliances that share an enemy
/// with it, or when it has no wars of its own to worry about.
fn alliances(world: &World, empire: EmpireId, cmds: &mut Vec<Command>) {
    let e = world.empire(empire);
    let enemies: Vec<EmpireId> = (0..world.empires.len())
        .map(EmpireId::from_idx)
        .filter(|o| world.at_war(empire, *o))
        .collect();
    let shares_enemy = |o: EmpireId| enemies.iter().any(|x| world.at_war(o, *x));
    for (a, alliance) in world.alliances.iter().enumerate() {
        let a = AllianceId::from_idx(a);
        if !alliance.invites.contains(&empire) || world.can_join_alliance(empire, a).is_err() {
            continue;
        }
        let members = world.alliance_members(a);
        if members.iter().any(|m| shares_enemy(*m)) || enemies.is_empty() {
            cmds.push(Command::JoinAlliance {
                empire,
                alliance: a,
            });
            return;
        }
    }
    let invited = |o: EmpireId| {
        world.alliances.iter().any(|a| a.invites.contains(&o)) || world.allied(empire, o)
    };
    let Some(friend) = (0..world.empires.len())
        .map(EmpireId::from_idx)
        .find(|o| *o != empire && shares_enemy(*o) && !world.at_war(empire, *o) && !invited(*o))
    else {
        return;
    };
    match e.alliances.first() {
        Some(a) if world.can_join_alliance(friend, *a).is_ok() => {
            cmds.push(Command::InviteToAlliance {
                empire,
                alliance: *a,
                invitee: friend,
            })
        }
        Some(_) => {}
        None => cmds.push(Command::CreateAlliance {
            empire,
            display_name: format!("{} Pact", e.display_name),
            invite: vec![friend],
        }),
    }
}

/// A rough measure of a ship's fighting strength for an empire.
fn ship_power(world: &World, empire: EmpireId, ship: ShipId) -> f64 {
    let d = world.ship_stats(empire, ship);
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
    let at_war = world.active_wars(empire).next().is_some() || declared.is_some();
    let own_fleets: Vec<&crate::world::Fleet> =
        world.fleets.iter().filter(|f| f.owner == empire).collect();
    let ships: usize =
        own_fleets.iter().map(|f| f.ships.len()).sum::<usize>() + e.shipbuilding.len();
    let defs = &world.defs;

    // Shipbuilding is transient: shipyards (buildings producing what ship designs need) must
    // be working while ships are under construction. One goes on the most populous world once
    // the empire can afford to arm itself.
    let ship_inputs: Vec<ResourceId> = defs
        .ships
        .iter()
        .flat_map(|s| s.build_cost.resources.iter())
        .map(|a| a.resource)
        .filter(|r| defs.is_transient(*r))
        .collect();
    let shipyard_like = |d: &BuildingDef| ship_inputs.iter().any(|r| output_of(d, *r) > 0.0);
    // Any shipyard counts, even one switched off while idle (see `toggle_transient`).
    let shipyard = planets.iter().copied().find(|p| {
        world.planet(*p).tiles.iter().any(|t| {
            t.building
                .as_ref()
                .is_some_and(|b| shipyard_like(defs.building(b.def)))
        })
    });
    if shipyard.is_none() && !ship_inputs.is_empty() {
        let mut habitable: Vec<PlanetId> = planets
            .iter()
            .copied()
            .filter(|p| world.planet(*p).habitable)
            .collect();
        habitable.sort_by_key(|p| std::cmp::Reverse(world.planet(*p).pop()));
        for pid in habitable.into_iter().take(1) {
            if world.planet(pid).is_constructing() {
                continue;
            }
            if let Some(b) = pick_building(world, empire, pid, budget, shipyard_like, |_| 1.0)
                && let Some(tile) = best_tile(world, empire, pid, b, None)
            {
                for a in &defs.building(b).build_cost.resources {
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
    // keep bigger fleets and more ships in production; everyone doubles up in wartime.
    let base = (3 + 2 * world.empire_systems(empire).len()) as f64 * (0.4 + 1.2 * traits.militant);
    let wanted = (base.round() as usize).clamp(2, 40) * if at_war { 2 } else { 1 };
    let in_production = 1 + usize::from(traits.militant > 0.6 || at_war);
    if let Some(yard) = shipyard
        && e.shipbuilding.len() < in_production
        && ships < wanted
    {
        let half: Vec<f64> = budget.iter().map(|b| b / 2.0).collect();
        let best = (0..defs.ships.len())
            .map(ShipId::from_idx)
            .filter(|s| {
                world.can_build_ship(yard, *s).is_ok()
                    && can_afford(defs, &half, &defs.ship(*s).build_cost.resources)
            })
            .max_by(|a, b| {
                ship_power(world, empire, *a)
                    .total_cmp(&ship_power(world, empire, *b))
                    .then(b.cmp(a))
            });
        if let Some(ship) = best {
            for a in &defs.ship(ship).build_cost.resources {
                if !defs.is_transient(a.resource) {
                    budget[a.resource.idx()] -= a.count;
                }
            }
            cmds.push(Command::BuildShip { planet: yard, ship });
        }
    }

    command_fleets(world, empire, traits, declared, cmds);
}

/// Fleet command alone, for the days between full decisions in wartime.
pub(crate) fn fleets(world: &World, empire: EmpireId) -> Vec<Command> {
    let traits = world.empire(empire).ai.unwrap_or_default();
    let mut cmds = Vec::new();
    command_fleets(world, empire, traits, None, &mut cmds);
    cmds
}

/// Fighting strength of one fleet (damaged ships count for less).
fn fleet_power(world: &World, f: &Fleet) -> f64 {
    f.ships
        .iter()
        .map(|s| {
            let max = world.ship_stats(f.owner, s.def).max_hull;
            ship_power(world, f.owner, s.def) * (0.3 + 0.7 * s.hull / max)
        })
        .sum()
}

/// Share of its hull a fleet has left.
fn hull_left(world: &World, f: &Fleet) -> f64 {
    let (hull, max) = f.ships.iter().fold((0.0, 0.0), |(h, m), s| {
        (h + s.hull, m + world.ship_stats(f.owner, s.def).max_hull)
    });
    if max > 0.0 { hull / max } else { 1.0 }
}

/// The planet a fleet should attack next: the nearest one an enemy controls that the fleet
/// can take on. Worlds of our own under occupation come first, then systems we fight a claim
/// war for, systems where we or our allies already attack (so fleets fight together) or hold
/// a planet, then populated worlds.
fn war_target(
    world: &World,
    hostile: &dyn Fn(EmpireId) -> bool,
    fleet: &Fleet,
    power: f64,
    focus: &[SystemId],
    enemy_at: &HashMap<SystemId, f64>,
) -> Option<PlanetId> {
    let empire = fleet.owner;
    // Systems we fight a claim war for.
    let claims: Vec<SystemId> = world
        .active_wars(empire)
        .filter(|(w, s)| world.war(*w).goals[s.idx()] == Some(WarGoal::Claim))
        .flat_map(|_| world.empire(empire).claims.iter().copied())
        .collect();
    let dist = world.distances_from([fleet.system]);
    (0..world.empires.len())
        .map(EmpireId::from_idx)
        .filter(|o| hostile(*o))
        .flat_map(|o| world.empire_planets(o).iter().copied())
        .map(|id| (id, world.planet(id)))
        .filter(|(_, p)| dist[p.system.idx()] != u32::MAX)
        // Leave systems guarded by much stronger fleets alone.
        .filter(|(_, p)| enemy_at.get(&p.system).copied().unwrap_or(0.0) <= power * 1.25)
        .min_by_key(|(id, p)| {
            let foothold = world
                .system(p.system)
                .planet_ids()
                .any(|q| world.planet(q).controller() == Some(empire));
            let bonus = if p.owner == Some(empire) {
                4
            } else if p.pop() > 0 {
                2
            } else {
                0
            } + if focus.contains(&p.system) { 2 } else { 0 }
                + if claims.contains(&p.system) { 3 } else { 0 };
            (
                dist[p.system.idx()].saturating_sub(bonus),
                !foothold,
                std::cmp::Reverse(p.pop()),
                *id,
            )
        })
        .map(|(id, _)| id)
}

/// Fleet command, run daily while the empire is at war (and with the other decisions in
/// peacetime):
///
/// * damaged fleets outside our territory fall back to the nearest own system to repair;
/// * own systems with enemy fleets get the nearest fleets sent to them, until what is there
///   or on the way matches the invaders;
/// * idle fleets sharing a system with a bigger one (or one on the attack) merge into it;
/// * every idle fleet of at least the attack size (smaller for militant empires) attacks,
///   as do fleets whose target is guarded by an overwhelming enemy (they pick another);
/// * smaller fleets gather at the own system closest to the front, where they merge into
///   attack-sized fleets; in peacetime they gather at the capital.
pub(crate) fn command_fleets(
    world: &World,
    empire: EmpireId,
    traits: AiPersonality,
    declared: Option<EmpireId>,
    cmds: &mut Vec<Command>,
) {
    let e = world.empire(empire);
    let hostile = |o: EmpireId| o != empire && (world.at_war(empire, o) || declared == Some(o));
    let at_war = world.active_wars(empire).next().is_some() || declared.is_some();
    let own: Vec<&Fleet> = world.fleets.iter().filter(|f| f.owner == empire).collect();
    if own.is_empty() {
        return;
    }
    let power: HashMap<FleetId, f64> = own.iter().map(|f| (f.id, fleet_power(world, f))).collect();
    let armed = |f: &Fleet| f.max_range(&e.ship_stats) > 0.0;
    let mut enemy_at: HashMap<SystemId, f64> = HashMap::new();
    for f in world.fleets.iter().filter(|f| hostile(f.owner)) {
        *enemy_at.entry(f.system).or_default() += fleet_power(world, f);
    }
    let target_system = |f: &Fleet| {
        f.orders.iter().find_map(|o| match o {
            FleetOrder::Bombard { planet, .. } => Some(world.planet(*planet).system),
            _ => None,
        })
    };
    let mut busy: Vec<FleetId> = Vec::new();

    // Retreat.
    for f in &own {
        let home = |s: SystemId| world.system(s).owner == Some(empire);
        if f.in_combat || home(f.system) || home(f.destination()) || hull_left(world, f) >= 0.4 {
            continue;
        }
        let dist = world.distances_from([f.system]);
        if let Some(to) = world
            .empire_systems(empire)
            .iter()
            .copied()
            .filter(|s| dist[s.idx()] != u32::MAX)
            .min_by_key(|s| (dist[s.idx()], *s))
        {
            busy.push(f.id);
            cmds.push(Command::MoveFleet {
                fleet: f.id,
                system: to,
                coordinates: None,
            });
        }
    }

    // Defence.
    let mut threats: Vec<(SystemId, f64)> = enemy_at
        .iter()
        .filter(|(s, _)| world.system(**s).owner == Some(empire))
        .map(|(s, p)| (*s, *p))
        .collect();
    threats.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    for (sys, threat) in threats {
        let mut defending: f64 = own
            .iter()
            .filter(|f| f.system == sys || f.destination() == sys)
            .map(|f| power[&f.id])
            .sum();
        if defending >= threat {
            continue;
        }
        let dist = world.distances_from([sys]);
        let mut free: Vec<&&Fleet> = own
            .iter()
            .filter(|f| {
                !busy.contains(&f.id)
                    && armed(f)
                    && !f.in_combat
                    && f.system != sys
                    && dist[f.system.idx()] <= 4
                    && (f.is_idle() || target_system(f).is_some())
            })
            .collect();
        free.sort_by_key(|f| (dist[f.system.idx()], f.id));
        // Feeding fleets in one by one only loses them: commit only if together they can
        // match the invaders.
        if defending + free.iter().map(|f| power[&f.id]).sum::<f64>() < threat * 0.8 {
            continue;
        }
        for f in free {
            if defending >= threat {
                break;
            }
            defending += power[&f.id];
            busy.push(f.id);
            cmds.push(Command::MoveFleet {
                fleet: f.id,
                system: sys,
                coordinates: None,
            });
        }
    }

    // Merge idle fleets into a bigger (or attacking) fleet in the same system.
    let limit = e.max_fleet_size(world.rules());
    let mut sizes: HashMap<FleetId, usize> =
        own.iter().map(|f| (f.id, f.size(&world.defs))).collect();
    for f in &own {
        if !f.is_idle() || busy.contains(&f.id) {
            continue;
        }
        let mine = sizes[&f.id];
        if let Some(big) = own
            .iter()
            .filter(|o| {
                o.system == f.system
                    && o.id != f.id
                    && !busy.contains(&o.id)
                    && sizes[&o.id] + mine <= limit
                    && (target_system(o).is_some() || (o.is_idle() && sizes[&o.id] >= mine))
            })
            .max_by_key(|o| {
                (
                    target_system(o).is_some(),
                    sizes[&o.id],
                    std::cmp::Reverse(o.id),
                )
            })
        {
            busy.push(f.id);
            *sizes.get_mut(&big.id).unwrap() += mine;
            sizes.insert(f.id, 0);
            cmds.push(Command::MergeFleets {
                into: big.id,
                from: f.id,
            });
        }
    }

    // Offence.
    let attack_size = ((6.0 - 4.0 * traits.militant).round().max(2.0) as usize).min(limit);
    // Systems our side is already attacking: ours and our co-belligerents'.
    let mut focus: Vec<SystemId> = world
        .fleets
        .iter()
        .filter(|f| !hostile(f.owner))
        .filter_map(|f| {
            let sys = target_system(f)?;
            let p = f.orders.iter().find_map(|o| match o {
                FleetOrder::Bombard { planet, .. } => Some(*planet),
                _ => None,
            })?;
            world
                .planet(p)
                .controller()
                .filter(|c| hostile(*c))
                .map(|_| sys)
        })
        .collect();
    let mut gathering: Vec<&&Fleet> = Vec::new();
    if at_war {
        let mut strike: Vec<&&Fleet> = own
            .iter()
            .filter(|f| !busy.contains(&f.id) && sizes[&f.id] > 0 && armed(f) && !f.in_combat)
            .filter(|f| {
                // Idle, or attacking a target that has become too well guarded.
                f.is_idle()
                    || target_system(f).is_some_and(|s| {
                        enemy_at.get(&s).copied().unwrap_or(0.0) > power[&f.id] * 1.5
                    })
            })
            .collect();
        strike.sort_by_key(|f| (std::cmp::Reverse(sizes[&f.id]), f.id));
        for f in strike {
            if sizes[&f.id] < attack_size {
                if f.is_idle() {
                    gathering.push(f);
                }
                continue;
            }
            match war_target(world, &hostile, f, power[&f.id], &focus, &enemy_at) {
                Some(planet) => {
                    busy.push(f.id);
                    focus.push(world.planet(planet).system);
                    cmds.push(Command::Bombard {
                        fleet: f.id,
                        planet,
                        tile: None,
                    });
                }
                None if f.is_idle() => gathering.push(f),
                None => {}
            }
        }
    } else {
        gathering = own
            .iter()
            .filter(|f| f.is_idle() && !busy.contains(&f.id) && sizes[&f.id] > 0)
            .collect();
    }

    // Gathering point: the own system closest to the front, or the capital.
    let staging = focus
        .first()
        .and_then(|front| {
            let dist = world.distances_from([*front]);
            world
                .empire_systems(empire)
                .iter()
                .copied()
                .filter(|s| dist[s.idx()] != u32::MAX)
                .min_by_key(|s| (dist[s.idx()], *s))
        })
        .or(e.capital);
    if let Some(to) = staging {
        for f in gathering {
            if f.system != to {
                cmds.push(Command::MoveFleet {
                    fleet: f.id,
                    system: to,
                    coordinates: None,
                });
            }
        }
    }
}
