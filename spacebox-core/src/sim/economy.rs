//! Phases 2–4: system aggregation, empire pools, the galactic market, and final settlement
//! into storage.
//!
//! # Shortages
//!
//! Every building and pop reports both its *full* demand and its *actual* consumption, which is
//! the full demand throttled by the previous tick's satisfaction ratio for its pool. After
//! settlement each pool computes, per resource,
//!
//! ```text
//! sat = min(1, (storage + supply + market purchases) / full demand)
//! ```
//!
//! which the next tick's planets use to throttle consumers. This one-tick lag keeps planets
//! fully independent within a tick while still propagating shortages through production
//! chains. Any stockpiled resource with `sat < 1` (storage is empty and income cannot cover
//! demand) puts the empire into a crisis for that resource.
//!
//! # Projects
//!
//! Constructions, removals, terraforming, research and shipbuilding all have a list of
//! remaining resources (and days). After settlement each empire's projects draw normal
//! resources from its stockpile, in a fixed order, as far as stock allows. Transient resources
//! (research, shipbuilding) are never stored: the day's production goes to the projects that
//! need it, and whatever is left over is wasted.

use crate::defs::{Cost, Defs, ResourceAmount, Ruleset, add_amounts};
use crate::ids::*;
use crate::sim::events::{Event, EventKind};
use crate::sim::migration::match_migrants;
use crate::world::{Crisis, Empire, Planet, Pool, ResearchProject, System, World};

/// Phase 3: sum each ownership group's planets and match migrants inside the group.
pub(crate) fn tick_system(sys: &mut System, planets: &[Planet]) {
    // Systems of idle planets have nothing to sum: their groups are already zero.
    let quiet = planets.iter().all(|p| p.flags.idle);
    if quiet && sys.quiet {
        return;
    }
    sys.quiet = quiet;
    for g in &mut sys.groups {
        g.flows.clear();
        g.emigrants.clear();
        g.vacancies.clear();
        g.moves.clear();
    }
    let first = sys.planet_start as usize;
    for (i, p) in planets.iter().enumerate() {
        let g = &mut sys.groups[p.group as usize];
        g.flows.add(&p.flows);
        if p.emigrants > 0 {
            g.emigrants.push(crate::sim::migration::Emigrants {
                planet: PlanetId::from_idx(first + i),
                count: p.emigrants,
            });
        }
        if let Some(v) = p.vacancy {
            g.vacancies.push(v);
        }
    }
    for g in &mut sys.groups {
        match_migrants(&mut g.emigrants, &mut g.vacancies, &mut g.moves);
    }
}

/// Phase 4a: combine system balances into the pool, losing a share of any surplus shipped
/// out of a non-hub system. Core pools also receive the empire's daily income and pay its
/// fleet upkeep. A humiliated empire gains no influence (the expansion resource).
pub(crate) fn aggregate_pool(
    pool: &mut Pool,
    systems: &[System],
    empires: &[Empire],
    rules: &Ruleset,
) {
    // A pool of quiet systems (empty, unowned rocks) stays at zero; core pools always run,
    // since they also carry empire income and fleet upkeep.
    let quiet = !pool.core && pool.members.iter().all(|(s, _)| systems[s.idx()].quiet);
    if quiet && pool.quiet {
        return;
    }
    pool.quiet = quiet;
    pool.net.fill(0.0);
    pool.demand.fill(0.0);
    pool.demand_full.fill(0.0);
    let shipping = pool.systems.len() > 1;
    for &(s, g) in &pool.members {
        let f = &systems[s.idx()].groups[g as usize].flows;
        let lossy = shipping && Some(s) != pool.hub;
        let (supply, demand, full) = (f.supply(), f.demand(), f.demand_full());
        for r in 0..pool.net.len() {
            let net = supply[r] - demand[r];
            pool.net[r] += if lossy && net > 0.0 {
                net * (1.0 - rules.logistics_loss)
            } else {
                net
            };
            pool.demand[r] += demand[r];
            pool.demand_full[r] += full[r];
        }
    }
    if let (true, Some(e)) = (pool.core, pool.empire) {
        let e = &empires[e.idx()];
        if let Some(r) = rules.expansion_resource {
            pool.net[r.idx()] += rules.expansion_resource_income;
        }
        for (r, &u) in e.fleet_upkeep.iter().enumerate() {
            if u > 0.0 {
                let paid = u * pool.sat[r];
                pool.demand_full[r] += u;
                pool.demand[r] += paid;
                pool.net[r] -= paid;
            }
        }
    }
    if let (Some(e), Some(r)) = (pool.empire, rules.expansion_resource)
        && empires[e.idx()].humiliated > 0
    {
        pool.net[r.idx()] = pool.net[r.idx()].min(0.0);
    }
}

/// Phase 4: the galactic market.
///
/// Every empire with market access offers stock above the ruleset's reserve and bids for any
/// amount its storage cannot cover this tick. Prices scale with the bid/offer ratio (0.5× to 2×
/// base value); bids are scaled down to what the buyer can afford in the currency resource.
pub(crate) fn market(world: &mut World) {
    let defs = world.defs.clone();
    let rules = defs.ruleset(world.ruleset);
    let n_res = defs.resource_count();
    for e in &mut world.empires {
        e.trade.fill(0.0);
    }
    for m in &mut world.market {
        m.volume = 0.0;
    }
    let Some(currency) = rules.currency else {
        return;
    };
    let n_emp = world.empires.len();
    let mut offer = vec![0.0; n_emp * n_res];
    let mut bid = vec![0.0; n_emp * n_res];
    let tradeable = |r: usize| {
        let d = &defs.resources[r];
        d.tradeable && !d.transient && r != currency.idx()
    };

    for (ei, e) in world.empires.iter().enumerate() {
        let Some(pool) = e.core_pool.filter(|_| e.market_access) else {
            continue;
        };
        let pool = &world.pools[pool.idx()];
        for r in (0..n_res).filter(|&r| tradeable(r)) {
            let projected = e.storage[r] + pool.net[r];
            offer[ei * n_res + r] = (projected - rules.market_reserve).max(0.0);
            bid[ei * n_res + r] = (-projected).max(0.0);
        }
    }

    let totals = |v: &[f64], r: usize| (0..n_emp).map(|e| v[e * n_res + r]).sum::<f64>();
    let price: Vec<f64> = (0..n_res)
        .map(|r| {
            let (o, b) = (totals(&offer, r), totals(&bid, r));
            // Values are relative to the currency's own value.
            let base =
                defs.resources[r].base_value / defs.resources[currency.idx()].base_value.max(1e-9);
            if o > 0.0 && b > 0.0 {
                base * (b / o).clamp(0.5, 2.0)
            } else {
                base
            }
        })
        .collect();

    // Budget constraint: a buyer can only spend the currency it will have this tick.
    for (ei, e) in world.empires.iter().enumerate() {
        let Some(pool) = e.core_pool else { continue };
        let budget =
            (e.storage[currency.idx()] + world.pools[pool.idx()].net[currency.idx()]).max(0.0);
        let cost: f64 = (0..n_res)
            .map(|r| bid[ei * n_res + r] * price[r] * (1.0 + rules.market_fee))
            .sum();
        if cost > budget && cost > 0.0 {
            let scale = budget / cost;
            for r in 0..n_res {
                bid[ei * n_res + r] *= scale;
            }
        }
    }

    for r in (0..n_res).filter(|&r| tradeable(r)) {
        let (o, b) = (totals(&offer, r), totals(&bid, r));
        world.market[r].price = price[r];
        if o <= 0.0 || b <= 0.0 {
            continue;
        }
        let traded = o.min(b);
        world.market[r].volume = traded;
        for (ei, e) in world.empires.iter_mut().enumerate() {
            let bought = bid[ei * n_res + r] * traded / b;
            let sold = offer[ei * n_res + r] * traded / o;
            if bought == 0.0 && sold == 0.0 {
                continue;
            }
            e.trade[r] += bought - sold;
            e.trade[currency.idx()] +=
                sold * price[r] - bought * price[r] * (1.0 + rules.market_fee);
        }
    }
}

/// Phase 5a: settle an empire's core pool into storage and update crises. Transient
/// production from all of the empire's planets becomes this tick's transient income.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize_empire(
    e: &mut Empire,
    id: EmpireId,
    pools: &[Pool],
    systems: &[System],
    planets: &[Planet],
    owned: &[PlanetId],
    defs: &Defs,
    rules: &Ruleset,
    day: u64,
) {
    e.events.clear();
    e.income.fill(0.0);
    let mut stats = crate::world::EmpireStats {
        planets: owned.len() as u32,
        ..Default::default()
    };
    let mut last_system = None;
    let transient: Vec<usize> = (0..defs.resource_count())
        .filter(|&r| defs.resources[r].transient)
        .collect();
    for &r in &transient {
        e.income[r] = 0.0;
    }
    for p in owned {
        let p = &planets[p.idx()];
        stats.pop += p.pop();
        for &r in &transient {
            e.income[r] += p.flows.net(r).max(0.0);
        }
        if last_system != Some(p.system) {
            last_system = Some(p.system);
            // Planets are stored per system, so this counts systems with a planet we own.
            if systems[p.system.idx()].owner == Some(id) {
                stats.systems += 1;
            }
        }
    }

    let Some(pool_id) = e.core_pool else {
        e.stats = stats;
        return;
    };
    let pool = &pools[pool_id.idx()];
    let mut short = Vec::new();
    let mut sat = vec![1.0; defs.resource_count()];
    let mut scarcity = vec![0.0; defs.resource_count()];
    for (r, res) in defs.resources.iter().enumerate() {
        let full = pool.demand_full[r];
        let supply = pool.demand[r] + pool.net[r];
        if res.transient {
            if full > 1e-9 {
                sat[r] = (supply.max(0.0) / full).clamp(0.0, 1.0);
            }
            continue;
        }
        let available = e.storage[r] + supply + e.trade[r];
        if full > 1e-9 {
            sat[r] = (available / full).clamp(0.0, 1.0);
        }
        let before = e.storage[r];
        let mut after = (available - pool.demand[r]).max(0.0);
        if let Some(cap) = rules.storage_cap(ResourceId::from_idx(r)) {
            after = after.min(cap);
        }
        e.storage[r] = after;
        e.income[r] = after - before;
        let target = full * rules.scarcity_target_days;
        if sat[r] < 0.999 {
            short.push((ResourceId::from_idx(r), 1.0 - sat[r]));
            scarcity[r] = 2.0 - sat[r];
        } else if target > 1e-9 {
            scarcity[r] = (1.0 - after / target).clamp(0.0, 1.0);
        }
    }
    e.sat = sat;
    e.scarcity = scarcity;

    // Crises begin when a stockpiled resource runs dry and end when it recovers.
    for &(r, severity) in &short {
        match e.crises.iter_mut().find(|c| c.resource == r) {
            Some(c) => c.severity = severity,
            None => {
                e.crises.push(Crisis {
                    resource: r,
                    since_day: day,
                    severity,
                });
                e.events.push(
                    Event::new(
                        day,
                        EventKind::CrisisStarted,
                        format!(
                            "{} has run out of {}: a crisis begins",
                            e.display_name,
                            defs.resource(r).display_name
                        ),
                    )
                    .empire(Some(id)),
                );
            }
        }
    }
    let mut resolved = Vec::new();
    e.crises.retain(|c| {
        let still = short.iter().any(|(r, _)| *r == c.resource);
        if !still {
            resolved.push((c.resource, day.saturating_sub(c.since_day)));
        }
        still
    });
    for (r, days) in resolved {
        e.events.push(
            Event::new(
                day,
                EventKind::CrisisResolved,
                format!(
                    "{}'s {} crisis is over after {days} days",
                    e.display_name,
                    defs.resource(r).display_name
                ),
            )
            .empire(Some(id)),
        );
    }
    e.crisis_penalty = (e.crises.iter().map(|c| c.severity).sum::<f64>()
        * rules.crisis_stability_penalty)
        .min(40.0);

    e.stats = stats;
}

/// Draws what a project still needs: normal resources from `storage` (recorded in `paid`),
/// transient ones from `transient` (stretched by `mult`), as far as they go.
fn draw(
    remaining: &mut Cost,
    paid: Option<&mut Vec<ResourceAmount>>,
    defs: &Defs,
    storage: &mut [f64],
    transient: &mut [f64],
    mult: f64,
) {
    let mut spent: Vec<ResourceAmount> = Vec::new();
    for a in &mut remaining.resources {
        if a.count <= 0.0 {
            continue;
        }
        let r = a.resource.idx();
        let take = if defs.resources[r].transient {
            let take = (transient[r] * mult).min(a.count);
            transient[r] -= take / mult;
            take
        } else {
            let take = storage[r].max(0.0).min(a.count);
            storage[r] -= take;
            if take > 0.0 {
                spent.push(ResourceAmount {
                    resource: a.resource,
                    count: take,
                });
            }
            take
        };
        a.count -= take;
        if a.count < 1e-9 {
            a.count = 0.0;
        }
    }
    if let Some(paid) = paid {
        add_amounts(paid, &spent);
    }
}

/// Phase 6b: funds every project from its empire's stockpile and transient production, in
/// order: tile constructions and terraforming (planet by planet), ships, then research.
/// Ships only get the transient output (shipbuilding) of the planet they are built at;
/// research gets what is left of the empire's. Finishes research and shipbuilding projects
/// with nothing left (tile projects finish during the planet phase). Unused transient
/// production is recorded as wasted.
pub(crate) fn fund_projects(world: &mut World) {
    let defs = world.defs.clone();
    let rules = defs.ruleset(world.ruleset);
    let day = world.day;
    let n_res = defs.resource_count();
    let World {
        empires,
        planets,
        busy,
        ..
    } = world;
    let mut transient: Vec<Vec<f64>> = empires
        .iter()
        .map(|e| {
            (0..n_res)
                .map(|r| {
                    if defs.resources[r].transient {
                        e.income[r]
                    } else {
                        0.0
                    }
                })
                .collect()
        })
        .collect();
    let before: Vec<Vec<f64>> = empires.iter().map(|e| e.storage.clone()).collect();

    for &i in busy.iter() {
        let p = &mut planets[i as usize];
        let owner = p.owner;
        if p.constructing {
            for t in &mut p.tiles {
                let Some(c) = &mut t.construction else {
                    continue;
                };
                if let Some(b) = c.empire.or(owner).filter(|b| b.idx() < empires.len()) {
                    let e = &mut empires[b.idx()];
                    draw(
                        &mut c.remaining,
                        Some(&mut c.paid),
                        &defs,
                        &mut e.storage,
                        &mut transient[b.idx()],
                        1.0,
                    );
                }
            }
        }
        if let (Some(t), Some(o)) = (&mut p.terraform, owner) {
            let e = &mut empires[o.idx()];
            draw(
                &mut t.remaining,
                Some(&mut t.paid),
                &defs,
                &mut e.storage,
                &mut transient[o.idx()],
                1.0,
            );
        }
    }

    for (i, e) in empires.iter_mut().enumerate() {
        let id = EmpireId::from_idx(i);
        let pool = &mut transient[i];

        // Ships: each planet's transient output only feeds the ships built there.
        let mut local: Vec<(PlanetId, Vec<f64>)> = Vec::new();
        for project in &mut e.shipbuilding {
            project.remaining.days = project.remaining.days.saturating_sub(1);
            let pid = project.planet;
            let slot = match local.iter().position(|(p, _)| *p == pid) {
                Some(k) => k,
                None => {
                    let p = &planets[pid.idx()];
                    let avail = (0..n_res)
                        .map(|r| {
                            if defs.resources[r].transient && p.owner == Some(id) {
                                p.flows.net(r).max(0.0)
                            } else {
                                0.0
                            }
                        })
                        .collect();
                    local.push((pid, avail));
                    local.len() - 1
                }
            };
            let avail = &mut local[slot].1;
            let start = avail.clone();
            draw(
                &mut project.remaining,
                Some(&mut project.paid),
                &defs,
                &mut e.storage,
                avail,
                1.0,
            );
            for r in 0..n_res {
                pool[r] = (pool[r] - (start[r] - avail[r])).max(0.0);
            }
        }
        let (done, active): (Vec<_>, Vec<_>) = std::mem::take(&mut e.shipbuilding)
            .into_iter()
            .partition(|p| p.remaining.is_done());
        e.shipbuilding = active;
        e.completed_ships.extend(done);

        for project in &mut e.research {
            // Waiting on a prerequisite that is still being researched.
            if defs
                .tech(project.tech)
                .prerequisites
                .iter()
                .any(|p| !e.techs[p.idx()])
            {
                continue;
            }
            project.remaining.days = project.remaining.days.saturating_sub(1);
            draw(
                &mut project.remaining,
                Some(&mut project.paid),
                &defs,
                &mut e.storage,
                pool,
                rules.research_speed_mult.max(1e-9),
            );
        }
        let (done, active): (Vec<ResearchProject>, Vec<ResearchProject>) =
            std::mem::take(&mut e.research)
                .into_iter()
                .partition(|p| p.remaining.is_done());
        e.research = active;
        for project in &done {
            e.techs[project.tech.idx()] = true;
            e.events.push(
                Event::new(
                    day,
                    EventKind::ResearchFinished,
                    format!(
                        "{} has researched {}",
                        e.display_name,
                        defs.tech(project.tech).display_name
                    ),
                )
                .empire(Some(id)),
            );
        }
        if !done.is_empty() {
            e.rebuild_modifiers(&defs);
        }

        for r in 0..n_res {
            if defs.resources[r].transient {
                e.wasted[r] = pool[r].max(0.0);
            } else {
                e.income[r] += e.storage[r] - before[i][r];
            }
        }
    }
}

/// Picks a technology to research for governors: the cheapest one not yet known or being
/// researched whose prerequisites are known or being researched, after dividing costs by
/// `weight`.
pub fn next_research(e: &Empire, defs: &Defs, weight: impl Fn(TechId) -> f64) -> Option<TechId> {
    let value = |t: TechId| -> f64 {
        defs.tech(t)
            .cost
            .iter()
            .map(|a| a.count * defs.resource(a.resource).base_value)
            .sum::<f64>()
            / weight(t).max(1e-9)
    };
    (0..defs.technologies.len())
        .map(TechId::from_idx)
        .filter(|t| {
            !e.techs[t.idx()]
                && !e.is_researching(*t)
                && defs
                    .tech(*t)
                    .prerequisites
                    .iter()
                    .all(|p| e.techs[p.idx()] || e.is_researching(*p))
        })
        .min_by(|a, b| value(*a).total_cmp(&value(*b)).then(a.cmp(b)))
}

/// Phase 5b: publish satisfaction ratios for next tick.
pub(crate) fn finalize_pool(pool: &mut Pool, empires: &[Empire]) {
    if pool.core
        && let Some(e) = pool.empire
    {
        pool.sat.copy_from_slice(&empires[e.idx()].sat);
        pool.scarcity.copy_from_slice(&empires[e.idx()].scarcity);
        return;
    }
    // A quiet pool settles at full satisfaction once its scarcity has decayed away.
    if pool.quiet && pool.scarcity.iter().all(|s| *s == 0.0) && pool.sat.iter().all(|s| *s == 1.0) {
        return;
    }
    // Isolated and unowned pools have no storage: only this tick's production counts.
    for r in 0..pool.sat.len() {
        let full = pool.demand_full[r];
        pool.sat[r] = if full > 1e-9 {
            ((pool.demand[r] + pool.net[r]).max(0.0) / full).clamp(0.0, 1.0)
        } else {
            1.0
        };
        // Without storage to buffer it, scarcity would flip jobs back and forth every tick, so
        // it decays gradually instead.
        let now = if pool.sat[r] < 0.999 {
            2.0 - pool.sat[r]
        } else {
            0.0
        };
        let decayed = pool.scarcity[r] * 0.95;
        pool.scarcity[r] = now.max(if decayed < 1e-3 { 0.0 } else { decayed });
    }
}
