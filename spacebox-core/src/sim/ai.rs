//! A deliberately simple governor for AI empires (those with an `[empire.x.ai]` table).
//!
//! Every `ai_interval_days` (staggered per empire) it looks at what the empire is short of and
//! issues a handful of ordinary [`Command`]s: housing where pops are crowded, job buildings
//! where they are unemployed (or on empty habitable worlds, to attract settlers), automated
//! barren-world buildings, the occasional upgrade and terraforming project. Decisions only read
//! the world, so all empires decide in parallel.

use crate::defs::{BuildingDef, ResourceAmount};
use crate::ids::*;
use crate::sim::commands::{Command, can_afford};
use crate::sim::economy::next_research;
use crate::world::{AiPersonality, FleetOrder, ProjectKind, World};

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
                    cost_value(world, &a.2.cost.resources)
                        .total_cmp(&cost_value(world, &b.2.cost.resources))
                        .then(a.0.cmp(&b.0))
                        .then(a.1.cmp(&b.1))
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
fn fleet_power(world: &World, empire: EmpireId) -> f64 {
    world
        .fleets
        .iter()
        .filter(|f| f.owner == empire)
        .flat_map(|f| &f.ships)
        .map(|s| {
            let max = world.ship_stats(empire, s.def).max_hull;
            ship_power(world, empire, s.def) * (0.3 + 0.7 * s.hull / max)
        })
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
    let hostile = |o: EmpireId| world.at_war(empire, o) || declared == Some(o);
    let at_war = !e.wars.is_empty() || declared.is_some();
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
        if own_fleets
            .iter()
            .any(|f| f.system == sys || f.destination() == sys)
        {
            continue;
        }
        let dist = world.distances_from([sys]);
        if let Some(f) = own_fleets
            .iter()
            .filter(|f| f.is_idle() && !busy.contains(&f.id) && dist[f.system.idx()] != u32::MAX)
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

    // New ships start as fleets of their own: idle fleets gather at the capital and merge,
    // in peace as in war.
    let idle: Vec<&&crate::world::Fleet> = own_fleets
        .iter()
        .filter(|f| f.is_idle() && !busy.contains(&f.id))
        .collect();

    // Merge idle fleets that share a system into the largest one with room.
    let limit = e.max_fleet_size(world.rules());
    let mut sizes: Vec<(FleetId, usize)> =
        idle.iter().map(|f| (f.id, f.size(&world.defs))).collect();
    let size = |id: FleetId, sizes: &[(FleetId, usize)]| {
        sizes.iter().find(|(i, _)| *i == id).map_or(0, |(_, n)| *n)
    };
    for f in &idle {
        let mine = size(f.id, &sizes);
        if mine == 0 || busy.contains(&f.id) {
            continue;
        }
        if let Some(big) = idle
            .iter()
            .filter(|o| {
                o.system == f.system
                    && o.id != f.id
                    && !busy.contains(&o.id)
                    && size(o.id, &sizes) >= mine
                    && size(o.id, &sizes) + mine <= limit
            })
            .max_by_key(|o| (size(o.id, &sizes), std::cmp::Reverse(o.id)))
        {
            busy.push(f.id);
            let total = size(big.id, &sizes) + mine;
            for s in sizes.iter_mut() {
                if s.0 == big.id {
                    s.1 = total;
                } else if s.0 == f.id {
                    s.1 = 0;
                }
            }
            cmds.push(Command::MergeFleets {
                into: big.id,
                from: f.id,
            });
        }
    }

    // Offence: the largest idle fleet bombards the nearest enemy world once big enough.
    let attacking = own_fleets.iter().any(|f| {
        f.orders
            .iter()
            .any(|o| matches!(o, FleetOrder::Bombard { .. }))
    });
    let threshold = (6.0 - 4.0 * traits.militant).round().max(2.0) as usize;
    let main = idle
        .iter()
        .filter(|f| !busy.contains(&f.id))
        .max_by_key(|f| (f.ships.len(), std::cmp::Reverse(f.id)));
    if at_war
        && !attacking
        && let Some(fleet) = main
        && fleet.size(&world.defs) >= threshold.min(limit)
        && fleet.max_range(&e.ship_stats) > 0.0
    {
        let dist = world.distances_from([fleet.system]);
        let target = world
            .planets
            .iter()
            .enumerate()
            .filter(|(_, p)| p.owner.is_some_and(hostile))
            .filter(|(_, p)| dist[p.system.idx()] != u32::MAX)
            .min_by_key(|(i, p)| {
                // Finish off systems where we already hold a planet before moving on, and go
                // for populated worlds (the ones that matter) over empty outposts nearby.
                let foothold = world
                    .system(p.system)
                    .planet_ids()
                    .any(|q| world.planet(q).owner == Some(empire));
                let populated_bonus = if p.pop() > 0 { 2 } else { 0 };
                (
                    dist[p.system.idx()].saturating_sub(populated_bonus),
                    !foothold,
                    std::cmp::Reverse(p.pop()),
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
