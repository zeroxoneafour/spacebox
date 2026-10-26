//! Phase 2: per-planet economy and population. Runs in parallel; each planet only reads
//! immutable data (definitions, its pool's shortage ratios from the previous tick, and its
//! owner's technology modifiers).

use crate::defs::{Defs, Modifier, ResourceAmount, Ruleset, Yield};
use crate::ids::*;
use crate::rng::{hash01, stochastic_round};
use crate::sim::events::{Event, EventKind};
use crate::sim::migration::Vacancy;
use crate::world::{BuildingInstance, Empire, Planet, ProjectKind};

/// A building's effective daily numbers at full employment, after upgrades, adjacency bonuses,
/// technology modifiers and tile multipliers.
#[derive(Clone, Debug)]
pub struct Profile {
    pub tile: u8,
    /// 0: the building has no workers and always runs at 100%.
    pub max_workers: u32,
    /// Base appeal of the building's job.
    pub appeal: i32,
    /// Appeal this tick, including the scarcity bonus.
    pub appeal_now: i32,
    pub job: Option<JobId>,
    pub pinned: bool,
    /// Daily, independent of workers.
    pub upkeep: Vec<(ResourceId, f64)>,
    /// Daily at full employment.
    pub inputs: Vec<(ResourceId, f64)>,
    /// Daily at full employment.
    pub outputs: Vec<(ResourceId, f64)>,
    /// Amenities at full employment.
    pub amenities: f64,
    /// Daily, independent of workers.
    pub passive: Vec<(ResourceId, f64)>,
    pub passive_amenities: f64,
    /// Union of upkeep and input resources: a shortage of any throttles the building.
    pub consumes: Vec<ResourceId>,
    pub housing: f64,
}

impl Profile {
    fn workforce(&self, inst: &BuildingInstance) -> f64 {
        if self.max_workers == 0 {
            1.0
        } else {
            inst.workers as f64 / self.max_workers as f64
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProfileCache {
    pub(crate) key: (Option<EmpireId>, u64),
    pub(crate) valid: bool,
    pub profiles: Vec<Profile>,
    /// Indices into `profiles` of staffed buildings: pinned first, then by appeal.
    pub(crate) job_order: Vec<u16>,
    pub housing: f64,
}

pub(crate) struct PlanetCtx<'a> {
    pub defs: &'a Defs,
    pub rules: &'a Ruleset,
    /// Shortage ratios of the planet's economic pool from the previous tick.
    pub sat: &'a [f64],
    pub scarcity: &'a [f64],
    pub owner: Option<(EmpireId, &'a Empire)>,
    pub day: u64,
    pub seed: u64,
}

/// How well a building works after bombardment: `1 - damage`, or nothing once badly damaged.
fn condition(inst: &BuildingInstance, rules: &Ruleset) -> f64 {
    if inst.damage >= rules.building_disable_damage {
        0.0
    } else {
        1.0 - inst.damage
    }
}

pub(crate) fn tick(p: &mut Planet, pid: PlanetId, ctx: &PlanetCtx) {
    let owner_id = ctx.owner.map(|(id, _)| id);
    let key = (owner_id, ctx.owner.map_or(0, |(_, e)| e.modifier_version));
    // Most planets in a large galaxy are empty, unowned rocks: skip them while nothing has
    // changed since a tick that found nothing to do.
    if p.flags.idle
        && !p.dirty
        && p.cache.key == key
        && p.population.is_empty()
        && p.terraform.is_none()
        && !p.flags.bombarded
    {
        return;
    }
    p.events.clear();
    p.flows.clear();
    p.emigrants = 0;
    p.vacancy = None;

    advance_projects(p, pid, ctx);

    if p.dirty || !p.cache.valid || p.cache.key != key {
        rebuild_profiles(p, ctx);
        p.cache.key = key;
        p.cache.valid = true;
        p.dirty = false;
    }

    let rules = ctx.rules;
    let defs = ctx.defs;
    let sat = ctx.sat;
    let pop = p.pop();
    // There is no housing without housing buildings.
    let housing = p.cache.housing;

    // Jobs: pinned allocations first, then the most appealing jobs. Appeal rises for jobs
    // whose associated resources are scarce in the pool (or, for jobs producing amenities,
    // while this planet lacks them), which is how essential low-appeal jobs get staffed.
    let amenity_gap = if p.stats.amenities_needed > 0.0 {
        ((p.stats.amenities_needed - p.stats.amenities) / p.stats.amenities_needed * 2.0)
            .clamp(0.0, 1.0)
    } else {
        0.0
    };
    for &i in &p.cache.job_order {
        let prof = &mut p.cache.profiles[i as usize];
        let mut bonus = prof.job.map_or(0.0, |j| {
            defs.job(j)
                .associated_resources
                .iter()
                .map(|r| ctx.scarcity[r.idx()])
                .fold(0.0f64, f64::max)
        });
        if prof.amenities > 0.0 {
            bonus = bonus.max(amenity_gap);
        }
        prof.appeal_now = prof.appeal + (bonus * rules.scarcity_appeal).round() as i32;
    }
    {
        let profiles = &p.cache.profiles;
        p.cache.job_order.sort_by_key(|&i| {
            let pr = &profiles[i as usize];
            (!pr.pinned, -pr.appeal_now, pr.tile)
        });
    }
    let mut remaining = pop;
    let mut jobs = 0u64;
    let mut free_jobs = 0u64;
    let mut best_free_appeal = i32::MIN;
    for &i in &p.cache.job_order {
        let prof = &p.cache.profiles[i as usize];
        let inst = p.tiles[prof.tile as usize]
            .building
            .as_mut()
            .expect("profiled tile has a building");
        let cap = if condition(inst, rules) == 0.0 {
            0
        } else {
            inst.job_cap
                .map_or(prof.max_workers, |c| c.min(prof.max_workers)) as u64
        };
        let w = cap.min(remaining);
        inst.workers = w as u32;
        remaining -= w;
        jobs += cap;
        if w < cap {
            free_jobs += cap - w;
            best_free_appeal = best_free_appeal.max(prof.appeal_now);
        }
    }
    let unemployed = remaining;
    let employed = pop - unemployed;

    let mut amenities = if pop > 0 { rules.base_amenities } else { 0.0 };
    for prof in &p.cache.profiles {
        let inst = p.tiles[prof.tile as usize].building.as_ref().unwrap();
        let cond = condition(inst, rules);
        amenities += (prof.amenities * prof.workforce(inst) + prof.passive_amenities) * cond;
    }
    if p.flags.bombarded {
        amenities = (amenities - rules.bombardment_amenities_penalty).max(0.0);
    }
    let amenities_needed = pop as f64 * rules.amenities_per_pop;
    let homeless = (pop as f64 - housing.floor()).max(0.0) as u64;

    // Stability. Each species weighs in on comfort by its share of the population.
    let pop_f = pop.max(1) as f64;
    let mut stability = rules.base_stability;
    if let Some((_, e)) = ctx.owner {
        stability -= e.crisis_penalty;
    }
    if amenities_needed > 0.0 {
        let ratio = (amenities - amenities_needed) / amenities_needed;
        stability += (ratio * 20.0).clamp(-30.0, 15.0);
    }
    stability -= unemployed as f64 / pop_f * rules.unemployment_stability_penalty;
    stability -= homeless as f64 / pop_f * rules.homelessness_stability_penalty;
    for &(s, n) in &p.population {
        if let Some(c) = defs.species_def(s).comfort_resource {
            stability -= n as f64 / pop_f * (1.0 - sat[c.idx()]) * rules.comfort_stability_penalty;
        }
    }
    let stability = stability.clamp(0.0, 100.0);
    let output_mult = (1.0 + (stability - 50.0) * rules.stability_output_factor).max(0.0);

    // Production and consumption.
    let (supply, demand, demand_full) = p.flows.parts_mut();
    for prof in &p.cache.profiles {
        let inst = p.tiles[prof.tile as usize].building.as_ref().unwrap();
        let wf = prof.workforce(inst);
        let cond = condition(inst, rules);
        let eff = prof
            .consumes
            .iter()
            .fold(1.0f64, |m, r| m.min(sat[r.idx()]));
        for &(r, c) in &prof.upkeep {
            demand_full[r.idx()] += c;
            demand[r.idx()] += c * eff;
        }
        for &(r, c) in &prof.inputs {
            demand_full[r.idx()] += c * wf;
            demand[r.idx()] += c * wf * eff;
        }
        for &(r, c) in &prof.outputs {
            supply[r.idx()] += c * wf * eff * cond * output_mult;
        }
        for &(r, c) in &prof.passive {
            supply[r.idx()] += c * eff * cond * output_mult;
        }
    }
    for &(s, n) in &p.population {
        for a in &defs.species_def(s).upkeep {
            let need = a.count * n as f64;
            demand_full[a.resource.idx()] += need;
            demand[a.resource.idx()] += need * sat[a.resource.idx()];
        }
    }

    grow_population(p, pid, ctx, housing);

    // Recovery from bombardment, unless it continued today.
    if p.flags.bombarded {
        p.flags.bombarded = false;
    } else {
        p.siege *= 1.0 - rules.siege_decay;
        if p.siege < 1.0 {
            p.siege = 0.0;
        }
        if p.flags.damaged {
            let mut any = false;
            for t in &mut p.tiles {
                if let Some(b) = t.building.as_mut().filter(|b| b.damage > 0.0) {
                    b.damage = (b.damage - rules.building_repair_rate).max(0.0);
                    any |= b.damage > 0.0;
                }
            }
            p.flags.damaged = any;
        }
    }

    // Migration offers, at the population's average migration rate.
    let pop = p.pop();
    let push = unemployed.max(homeless).min(pop);
    if push > 0 {
        let rate: f64 = p
            .population
            .iter()
            .map(|&(s, n)| n as f64 * defs.species_def(s).migration_rate)
            .sum::<f64>()
            / pop.max(1) as f64;
        p.emigrants = ((push as f64 * rate).ceil() as u64).clamp(1, push);
    }
    if unemployed == 0 && free_jobs > 0 {
        let free_housing = (housing - pop as f64).floor().max(0.0) as u64;
        let count = free_jobs.min(free_housing);
        if count > 0 {
            p.vacancy = Some(Vacancy {
                planet: pid,
                count,
                appeal: best_free_appeal,
            });
        }
    }

    // Unrest notifications on threshold crossings only.
    if stability < 20.0 && pop > 0 && !p.flags.unrest {
        p.flags.unrest = true;
        p.events.push(
            Event::new(
                ctx.day,
                EventKind::Unrest,
                format!(
                    "Unrest spreads on {} (stability {:.0})",
                    p.display_name, stability
                ),
            )
            .empire(owner_id)
            .planet(pid),
        );
    } else if stability >= 30.0 {
        p.flags.unrest = false;
    }

    p.stats.housing = housing;
    p.stats.amenities = amenities;
    p.stats.amenities_needed = amenities_needed;
    p.stats.stability = stability;
    p.stats.jobs = jobs;
    p.stats.employed = employed;
    p.stats.unemployed = unemployed;
    p.stats.homeless = homeless;
    p.stats.free_jobs = free_jobs;
    p.flags.idle = p.cache.profiles.is_empty()
        && p.population.is_empty()
        && !p.constructing
        && p.terraform.is_none()
        && p.siege == 0.0
        && !p.flags.damaged
        && p.events.is_empty();
}

/// Births and starvation per species. Fractional amounts are rounded at random (with a
/// deterministic roll), so small planets still grow at the right average rate.
fn grow_population(p: &mut Planet, pid: PlanetId, ctx: &PlanetCtx, housing: f64) {
    let defs = ctx.defs;
    let owner_id = ctx.owner.map(|(id, _)| id);
    let mult = ctx.owner.map_or(1.0, |(_, e)| e.modifiers.pop_growth_mult);
    let total = p.pop() as f64;
    let had_pops = total > 0.0;
    let mut room = (housing - total).floor().max(0.0) as u64;
    let mut starving = false;
    let mut changes: Vec<(SpeciesId, i64)> = Vec::new();
    for &(s, n) in &p.population {
        let d = defs.species_def(s);
        let roll = hash01(ctx.seed, &[ctx.day, pid.0 as u64, s.0 as u64]);
        let food = d.starvation_resource.map_or(1.0, |r| ctx.sat[r.idx()]);
        if food < 0.999 {
            starving = true;
            let lost = stochastic_round(n as f64 * d.starvation_rate * (1.0 - food), roll);
            changes.push((s, -(lost.min(n) as i64)));
        } else if total < housing {
            let g = (d.growth_rate * n as f64 * (1.0 - total / housing)).max(d.min_growth);
            let born = stochastic_round(g * mult, roll).min(room);
            room -= born;
            changes.push((s, born as i64));
        }
    }
    for (s, c) in changes {
        if c > 0 {
            p.add_pops(s, c as u64);
        } else if c < 0 {
            p.remove_species(s, (-c) as u64);
        }
    }
    if starving {
        // Announce famines once, not every time supplies flicker.
        let recent = p.flags.famine_announced.is_some_and(|d| ctx.day < d + 60);
        if !p.flags.starving && !recent {
            p.flags.famine_announced = Some(ctx.day);
            p.events.push(
                Event::new(
                    ctx.day,
                    EventKind::Starvation,
                    format!("Famine strikes {}", p.display_name),
                )
                .empire(owner_id)
                .planet(pid),
            );
        }
    }
    p.flags.starving = starving;
    if had_pops && p.pop() == 0 {
        p.events.push(
            Event::new(
                ctx.day,
                EventKind::Abandoned,
                format!("The last inhabitants of {} have perished", p.display_name),
            )
            .empire(owner_id)
            .planet(pid),
        );
    }
}

/// Ticks down the days of tile projects and terraforming, and completes the ones with
/// nothing left (their resources are drawn from storage during settlement).
fn advance_projects(p: &mut Planet, pid: PlanetId, ctx: &PlanetCtx) {
    let owner_id = ctx.owner.map(|(id, _)| id);
    if p.constructing || p.dirty {
        let mut any = false;
        for tile in &mut p.tiles {
            let Some(c) = &mut tile.construction else {
                continue;
            };
            any = true;
            c.remaining.days = c.remaining.days.saturating_sub(1);
            if !c.remaining.is_done() {
                continue;
            }
            let c = tile.construction.take().unwrap();
            let builder = c.empire.or(owner_id);
            let msg = match c.kind {
                ProjectKind::Building(def) => {
                    tile.building = Some(BuildingInstance::new(def));
                    if let Some(e) = c.empire.filter(|e| Some(*e) != owner_id) {
                        // Expansion: finishing a building on a planet you don't own claims it.
                        p.claims.push(e);
                    }
                    format!(
                        "{} completed on {}",
                        ctx.defs.building(def).display_name,
                        p.display_name
                    )
                }
                ProjectKind::Upgrade(upgrade) => {
                    let Some(b) = &mut tile.building else {
                        continue;
                    };
                    b.add_upgrade(upgrade);
                    format!(
                        "{} upgraded with {} on {}",
                        ctx.defs.building(b.def).display_name,
                        ctx.defs.upgrade(upgrade).display_name,
                        p.display_name
                    )
                }
                ProjectKind::Remove => {
                    let Some(b) = tile.building.take() else {
                        continue;
                    };
                    let d = ctx.defs.building(b.def);
                    let verb = if d.blocker { "cleared" } else { "demolished" };
                    format!("{} {verb} on {}", d.display_name, p.display_name)
                }
            };
            p.dirty = true;
            p.events.push(
                Event::new(ctx.day, EventKind::ConstructionFinished, msg)
                    .empire(builder)
                    .planet(pid),
            );
        }
        p.constructing = any;
    }
    if let Some(t) = &mut p.terraform {
        t.remaining.days = t.remaining.days.saturating_sub(1);
        if t.remaining.is_done() {
            p.terraform = None;
            p.habitable = true;
            p.dirty = true;
            p.events.push(
                Event::new(
                    ctx.day,
                    EventKind::Terraformed,
                    format!(
                        "{} has been terraformed and is now habitable",
                        p.display_name
                    ),
                )
                .empire(owner_id)
                .planet(pid),
            );
        }
    }
}

fn scale(list: &[ResourceAmount], mult: f64) -> Vec<(ResourceId, f64)> {
    list.iter()
        .map(|a| (a.resource, a.count * mult))
        .filter(|(_, c)| *c != 0.0)
        .collect()
}

/// Computes the effective profile of one building on a planet. Additions from every
/// applicable upgrade, adjacency bonus and technology are summed first; the product of all
/// multipliers is applied to the totals.
pub fn building_profile(
    defs: &Defs,
    planet: &Planet,
    tile: usize,
    owner: Option<&Empire>,
) -> Option<Profile> {
    let inst = planet.tiles[tile].building.as_ref()?;
    let def = defs.building(inst.def);
    let mut m = Modifier::default();
    for u in &inst.upgrades {
        m.combine(&defs.upgrade(*u).modifier);
    }
    let shape = planet.size.shape();
    for adj in &def.adjacency {
        for &n in shape.neighbors(tile) {
            if let Some(nb) = &planet.tiles[n as usize].building
                && defs.targets(&adj.targets, nb.def)
            {
                m.combine(&adj.modifier);
            }
        }
    }
    if let Some(e) = owner {
        for tm in &e.modifiers.buildings {
            if defs.targets_or_all(&tm.targets, inst.def) {
                m.combine(&tm.modifier);
            }
        }
    }
    let mut upkeep = def.upkeep.clone();
    crate::defs::add_amounts(&mut upkeep, &m.upkeep_add);
    let mut inputs = def.inputs.clone();
    crate::defs::add_amounts(&mut inputs, &m.input_add);
    let mut outputs: Yield = def.outputs.clone();
    outputs.add(&m.output_add);
    let mut passive: Yield = def.passive_outputs.clone();
    passive.add(&m.passive_add);

    let upkeep = scale(&upkeep, m.upkeep_mult);
    let inputs = scale(&inputs, m.input_mult);
    let t = &planet.tiles[tile];
    let with_tile = |list: &[ResourceAmount], mult: f64| -> Vec<(ResourceId, f64)> {
        list.iter()
            .map(|a| (a.resource, a.count * mult * t.mult(a.resource)))
            .filter(|(_, c)| *c != 0.0)
            .collect()
    };
    let max_workers = def.max_workers.map_or(0, |w| {
        ((w as f64 + m.max_workers_add) * m.max_workers_mult)
            .round()
            .max(0.0) as u32
    });
    let job = m.job.or(def.job);
    let mut consumes: Vec<ResourceId> = upkeep.iter().chain(&inputs).map(|(r, _)| *r).collect();
    consumes.sort_unstable();
    consumes.dedup();
    let appeal = job.map_or(0, |j| defs.job(j).appeal);
    Some(Profile {
        tile: tile as u8,
        max_workers,
        appeal,
        appeal_now: appeal,
        job,
        pinned: inst.pinned && inst.job_cap.is_some(),
        upkeep,
        inputs,
        outputs: with_tile(&outputs.resources, m.output_mult),
        amenities: outputs.amenities * m.output_mult,
        passive: with_tile(&passive.resources, m.passive_mult),
        passive_amenities: passive.amenities * m.passive_mult,
        consumes,
        housing: (def.housing + m.housing_add) * m.housing_mult,
    })
}

fn rebuild_profiles(p: &mut Planet, ctx: &PlanetCtx) {
    let owner = ctx.owner.map(|(_, e)| e);
    let mut profiles = std::mem::take(&mut p.cache.profiles);
    profiles.clear();
    for (i, t) in p.tiles.iter().enumerate() {
        match &t.building {
            Some(b) if b.enabled => {}
            _ => continue,
        }
        if let Some(prof) = building_profile(ctx.defs, p, i, owner) {
            // Inert buildings (most natural blockers) have nothing to simulate each tick.
            let inert = prof.max_workers == 0
                && prof.upkeep.is_empty()
                && prof.inputs.is_empty()
                && prof.outputs.is_empty()
                && prof.passive.is_empty()
                && prof.housing == 0.0
                && prof.amenities == 0.0
                && prof.passive_amenities == 0.0;
            if !inert {
                profiles.push(prof);
            }
        }
    }
    for (i, t) in p.tiles.iter_mut().enumerate() {
        if let Some(b) = &mut t.building
            && (!b.enabled || !profiles.iter().any(|pr| pr.tile as usize == i))
        {
            b.workers = 0;
        }
    }
    let mut order: Vec<u16> = (0..profiles.len() as u16)
        .filter(|&i| profiles[i as usize].max_workers > 0)
        .collect();
    order.sort_by_key(|&i| {
        let pr = &profiles[i as usize];
        (!pr.pinned, -pr.appeal, pr.tile)
    });
    p.cache.housing = profiles.iter().map(|pr| pr.housing).sum();
    p.cache.job_order = order;
    p.cache.profiles = profiles;
    p.constructing = p.tiles.iter().any(|t| t.construction.is_some());
    p.flags.damaged = p
        .tiles
        .iter()
        .any(|t| t.building.as_ref().is_some_and(|b| b.damage > 0.0));
}
