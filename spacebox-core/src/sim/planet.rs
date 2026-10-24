//! Phase 1: per-planet economy and population. Runs in parallel; each planet only reads
//! immutable data (definitions, its pool's shortage ratios from the previous tick, and its
//! owner's technology modifiers).

use crate::defs::{Defs, Modifier, ResourceAmount, Ruleset};
use crate::ids::*;
use crate::sim::events::{Event, EventKind};
use crate::sim::migration::Vacancy;
use crate::world::{BuildingInstance, Construction, Empire, Planet};

/// A building's effective daily numbers at full employment, after upgrades, adjacency bonuses,
/// technology modifiers and tile multipliers.
#[derive(Clone, Debug)]
pub struct Profile {
    pub tile: u8,
    pub max_workers: u32,
    /// Base appeal of the building's job.
    pub appeal: i32,
    /// Appeal this tick, including the scarcity bonus.
    pub appeal_now: i32,
    pub job: Option<JobId>,
    pub pinned: bool,
    pub upkeep: Vec<(ResourceId, f64)>,
    pub inputs: Vec<(ResourceId, f64)>,
    pub outputs: Vec<(ResourceId, f64)>,
    /// Union of upkeep and input resources: a shortage of any throttles the building.
    pub consumes: Vec<ResourceId>,
    pub housing: f64,
    pub amenities: f64,
    /// Produces an on-demand resource (e.g. a shipyard): idle unless the owner needs it.
    pub on_demand: bool,
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
    /// Whether the owner has ships queued, which is what puts shipyards to work.
    pub shipyards_needed: bool,
    pub day: u64,
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
    p.events.clear();
    p.flows.clear();
    p.emigrants = 0;
    p.vacancy = None;
    let owner_id = ctx.owner.map(|(id, _)| id);

    advance_construction(p, pid, ctx);

    let key = (owner_id, ctx.owner.map_or(0, |(_, e)| e.modifier_version));
    if p.dirty || !p.cache.valid || p.cache.key != key {
        rebuild_profiles(p, ctx);
        p.cache.key = key;
        p.cache.valid = true;
        p.dirty = false;
    }

    let rules = ctx.rules;
    let sat = ctx.sat;
    let housing = rules.base_housing(p.size, p.habitable) + p.cache.housing;

    // Jobs: pinned allocations first, then the most appealing jobs. Appeal rises for jobs
    // whose outputs are scarce in the pool (or, for amenity jobs, on this planet), which is how
    // essential low-appeal jobs get staffed.
    let amenity_gap = if p.stats.amenities_needed > 0.0 {
        ((p.stats.amenities_needed - p.stats.amenities) / p.stats.amenities_needed * 2.0)
            .clamp(0.0, 1.0)
    } else {
        0.0
    };
    for &i in &p.cache.job_order {
        let prof = &mut p.cache.profiles[i as usize];
        let mut bonus = prof
            .outputs
            .iter()
            .map(|(r, _)| ctx.scarcity[r.idx()])
            .fold(0.0f64, f64::max);
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
    let mut remaining = p.pop;
    let mut jobs = 0u64;
    let mut free_jobs = 0u64;
    let mut best_free_appeal = i32::MIN;
    for &i in &p.cache.job_order {
        let prof = &p.cache.profiles[i as usize];
        let inst = p.tiles[prof.tile as usize]
            .building
            .as_mut()
            .expect("profiled tile has a building");
        let idle = (prof.on_demand && !ctx.shipyards_needed) || condition(inst, rules) == 0.0;
        let cap = if idle {
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
    let employed = p.pop - unemployed;

    let mut amenities = if p.pop > 0 { rules.base_amenities } else { 0.0 };
    for prof in &p.cache.profiles {
        let inst = p.tiles[prof.tile as usize].building.as_ref().unwrap();
        amenities += prof.amenities * prof.workforce(inst) * condition(inst, rules);
    }
    if p.flags.bombarded {
        amenities = (amenities - rules.bombardment_amenities_penalty).max(0.0);
    }
    let amenities_needed = p.pop as f64 * rules.amenities_per_pop;
    let homeless = (p.pop as f64 - housing.floor()).max(0.0) as u64;

    // Stability.
    let pop_f = p.pop.max(1) as f64;
    let mut stability = rules.base_stability;
    if let Some((_, e)) = ctx.owner {
        stability += e.modifiers.stability - e.crisis_penalty;
    }
    if amenities_needed > 0.0 {
        let ratio = (amenities - amenities_needed) / amenities_needed;
        stability += (ratio * 20.0).clamp(-30.0, 15.0);
    }
    stability -= unemployed as f64 / pop_f * rules.unemployment_stability_penalty;
    stability -= homeless as f64 / pop_f * rules.homelessness_stability_penalty;
    if let Some(c) = rules.comfort_resource {
        stability -= (1.0 - sat[c.idx()]) * rules.comfort_stability_penalty;
    }
    let stability = stability.clamp(0.0, 100.0);
    let output_mult = (1.0 + (stability - 50.0) * rules.stability_output_factor).max(0.0);

    // Production and consumption.
    let flows = &mut p.flows;
    for prof in &p.cache.profiles {
        let inst = p.tiles[prof.tile as usize].building.as_ref().unwrap();
        let idle = prof.on_demand && !ctx.shipyards_needed;
        let wf = if idle { 0.0 } else { prof.workforce(inst) };
        let cond = condition(inst, rules);
        let eff = prof
            .consumes
            .iter()
            .fold(1.0f64, |m, r| m.min(sat[r.idx()]));
        for &(r, c) in &prof.upkeep {
            flows.demand_full[r.idx()] += c;
            flows.demand[r.idx()] += c * eff;
        }
        for &(r, c) in &prof.inputs {
            flows.demand_full[r.idx()] += c * wf;
            flows.demand[r.idx()] += c * wf * eff;
        }
        for &(r, c) in &prof.outputs {
            flows.supply[r.idx()] += c * wf * eff * cond * output_mult;
        }
        if let Some(job) = prof.job {
            for a in &ctx.defs.job(job).upkeep {
                let need = a.count * inst.workers as f64;
                flows.demand_full[a.resource.idx()] += need;
                flows.demand[a.resource.idx()] += need * sat[a.resource.idx()];
            }
        }
    }
    for a in &rules.pop_upkeep {
        let need = a.count * p.pop as f64;
        flows.demand_full[a.resource.idx()] += need;
        flows.demand[a.resource.idx()] += need * sat[a.resource.idx()];
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
        for t in &mut p.tiles {
            if let Some(b) = t.building.as_mut().filter(|b| b.damage > 0.0) {
                b.damage = (b.damage - rules.building_repair_rate).max(0.0);
            }
        }
    }

    // Migration offers.
    let push = unemployed.max(homeless).min(p.pop);
    if push > 0 {
        p.emigrants = ((push as f64 * rules.migration_rate).ceil() as u64).clamp(1, push);
    }
    if unemployed == 0 && free_jobs > 0 {
        let free_housing = (housing - p.pop as f64).floor().max(0.0) as u64;
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
    if stability < 20.0 && p.pop > 0 && !p.flags.unrest {
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
}

fn grow_population(p: &mut Planet, pid: PlanetId, ctx: &PlanetCtx, housing: f64) {
    let rules = ctx.rules;
    let owner_id = ctx.owner.map(|(id, _)| id);
    let food = rules.starvation_resource.map_or(1.0, |r| ctx.sat[r.idx()]);
    if p.pop > 0 && food < 0.999 {
        p.growth_progress =
            p.growth_progress.min(0.0) - p.pop as f64 * rules.starvation_rate * (1.0 - food);
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
        p.flags.starving = true;
    } else {
        p.flags.starving = false;
        let pop = p.pop as f64;
        if p.pop > 0 && pop < housing {
            let mult = ctx.owner.map_or(1.0, |(_, e)| e.modifiers.pop_growth_mult);
            let g = (rules.pop_growth_rate * pop * (1.0 - pop / housing)).max(rules.min_pop_growth);
            p.growth_progress += g * mult;
        } else {
            p.growth_progress = p.growth_progress.min(0.0);
        }
    }
    while p.growth_progress >= 1.0 && (p.pop as f64) < housing {
        p.pop += 1;
        p.growth_progress -= 1.0;
    }
    let had_pops = p.pop > 0;
    while p.growth_progress <= -1.0 && p.pop > 0 {
        p.pop -= 1;
        p.growth_progress += 1.0;
    }
    if p.pop == 0 {
        p.growth_progress = 0.0;
        if had_pops {
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
}

fn advance_construction(p: &mut Planet, pid: PlanetId, ctx: &PlanetCtx) {
    let owner_id = ctx.owner.map(|(id, _)| id);
    for tile in &mut p.tiles {
        let Some(c) = &mut tile.construction else {
            continue;
        };
        let days = match c {
            Construction::Building { days_left, .. } | Construction::Upgrade { days_left, .. } => {
                days_left
            }
        };
        *days = days.saturating_sub(1);
        if *days > 0 {
            continue;
        }
        let mut builder = owner_id;
        let msg = match tile.construction.take().unwrap() {
            Construction::Building { def, empire, .. } => {
                tile.building = Some(BuildingInstance::new(def));
                if let Some(e) = empire.filter(|e| Some(*e) != owner_id) {
                    // Expansion: finishing a site on a planet you don't own claims it.
                    p.claims.push(e);
                    builder = Some(e);
                }
                format!(
                    "{} completed on {}",
                    ctx.defs.building(def).display_name,
                    p.display_name
                )
            }
            Construction::Upgrade { upgrade, .. } => {
                let Some(b) = &mut tile.building else {
                    continue;
                };
                b.upgrades.push(upgrade);
                format!(
                    "{} upgraded with {} on {}",
                    ctx.defs.building(b.def).display_name,
                    ctx.defs.upgrade(upgrade).display_name,
                    p.display_name
                )
            }
        };
        p.dirty = true;
        p.events.push(
            Event::new(ctx.day, EventKind::ConstructionFinished, msg)
                .empire(builder)
                .planet(pid),
        );
    }
    if let Some(days) = &mut p.terraform_days_left {
        *days = days.saturating_sub(1);
        if *days == 0 {
            p.terraform_days_left = None;
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

/// Accumulates resource lists by summing per resource, and multipliers by product.
struct Accum {
    m: Modifier,
}

fn add_amounts(list: &mut Vec<ResourceAmount>, items: &[ResourceAmount]) {
    for a in items {
        match list.iter_mut().find(|x| x.resource == a.resource) {
            Some(x) => x.count += a.count,
            None => list.push(*a),
        }
    }
}

impl Accum {
    fn apply(&mut self, m: &Modifier) {
        add_amounts(&mut self.m.inputs, &m.inputs);
        add_amounts(&mut self.m.outputs, &m.outputs);
        add_amounts(&mut self.m.upkeep, &m.upkeep);
        self.m.housing += m.housing;
        self.m.amenities += m.amenities;
        self.m.input_mult *= m.input_mult;
        self.m.output_mult *= m.output_mult;
        self.m.upkeep_mult *= m.upkeep_mult;
        self.m.housing_mult *= m.housing_mult;
        self.m.amenities_mult *= m.amenities_mult;
    }
}

/// Computes the effective profile of one building on a planet.
pub fn building_profile(
    defs: &Defs,
    planet: &Planet,
    tile: usize,
    owner: Option<&Empire>,
) -> Option<Profile> {
    let inst = planet.tiles[tile].building.as_ref()?;
    let def = defs.building(inst.def);
    let mut acc = Accum {
        m: Modifier {
            inputs: def.inputs.clone(),
            outputs: def.outputs.clone(),
            upkeep: def.upkeep.clone(),
            housing: def.housing,
            amenities: def.amenities,
            ..Modifier::default()
        },
    };
    for u in &inst.upgrades {
        acc.apply(&defs.upgrade(*u).modifier);
    }
    let shape = planet.size.shape();
    for adj in &def.adjacency {
        for &n in shape.neighbors(tile) {
            if let Some(nb) = &planet.tiles[n as usize].building
                && defs.target_matches(adj.target, nb.def)
            {
                acc.apply(&adj.modifier);
            }
        }
    }
    if let Some(e) = owner {
        for tm in &e.modifiers.buildings {
            if defs.target_matches(tm.target, inst.def) {
                acc.apply(&tm.modifier);
            }
        }
    }
    let m = acc.m;
    let scale = |list: &[ResourceAmount], mult: f64| -> Vec<(ResourceId, f64)> {
        list.iter().map(|a| (a.resource, a.count * mult)).collect()
    };
    let upkeep = scale(&m.upkeep, m.upkeep_mult);
    let inputs = scale(&m.inputs, m.input_mult);
    let t = &planet.tiles[tile];
    let outputs = m
        .outputs
        .iter()
        .map(|a| (a.resource, a.count * m.output_mult * t.mult(a.resource)))
        .collect();
    let mut consumes: Vec<ResourceId> = upkeep.iter().chain(&inputs).map(|(r, _)| *r).collect();
    consumes.sort_unstable();
    consumes.dedup();
    Some(Profile {
        tile: tile as u8,
        max_workers: def.max_workers,
        appeal: def.job.map_or(0, |j| defs.job(j).appeal),
        appeal_now: def.job.map_or(0, |j| defs.job(j).appeal),
        job: def.job,
        pinned: inst.pinned && inst.job_cap.is_some(),
        upkeep,
        inputs,
        outputs,
        consumes,
        on_demand: defs.produces_on_demand(inst.def),
        housing: m.housing * m.housing_mult,
        amenities: m.amenities * m.amenities_mult,
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
                && prof.housing == 0.0
                && prof.amenities == 0.0;
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
}
