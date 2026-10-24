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

use crate::defs::{Defs, Ruleset};
use crate::ids::*;
use crate::sim::events::{Event, EventKind};
use crate::sim::migration::match_migrants;
use crate::world::{Crisis, Empire, Planet, Pool, System, World};

/// Phase 3: sum each ownership group's planets and match migrants inside the group.
pub(crate) fn tick_system(sys: &mut System, planets: &[Planet]) {
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
/// fleet upkeep.
pub(crate) fn aggregate_pool(
    pool: &mut Pool,
    systems: &[System],
    empires: &[Empire],
    rules: &Ruleset,
) {
    pool.net.fill(0.0);
    pool.demand.fill(0.0);
    pool.demand_full.fill(0.0);
    let shipping = pool.systems.len() > 1;
    for &(s, g) in &pool.members {
        let f = &systems[s.idx()].groups[g as usize].flows;
        let lossy = shipping && Some(s) != pool.hub;
        for r in 0..pool.net.len() {
            let net = f.supply[r] - f.demand[r];
            pool.net[r] += if lossy && net > 0.0 {
                net * (1.0 - rules.logistics_loss)
            } else {
                net
            };
            pool.demand[r] += f.demand[r];
            pool.demand_full[r] += f.demand_full[r];
        }
    }
    if let (true, Some(e)) = (pool.core, pool.empire) {
        let e = &empires[e.idx()];
        for a in &rules.empire_income {
            pool.net[a.resource.idx()] += a.count;
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
        d.tradeable && d.stockpile && r != currency.idx()
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
            let base = defs.resources[r].base_value;
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

/// Phase 5a: settle an empire's core pool into storage, update crises and research.
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
    for p in owned {
        let p = &planets[p.idx()];
        stats.pop += p.pop;
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
        if Some(ResourceId::from_idx(r)) == rules.research_resource || !res.stockpile {
            e.income[r] = pool.net[r];
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
        if let Some(cap) = res.storage_cap {
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

    if let Some(rr) = rules.research_resource {
        let points = pool.net[rr.idx()].max(0.0);
        stats.research_income = points;
        research(e, id, defs, points, day);
    }
    build_ships(e, pool, defs);
    e.stats = stats;
}

/// Feeds on-demand production (shipbuilding) into the ship queue, oldest order first.
fn build_ships(e: &mut Empire, pool: &Pool, defs: &Defs) {
    for (r, res) in defs.resources.iter().enumerate() {
        if !res.on_demand {
            continue;
        }
        let mut points = pool.net[r].max(0.0);
        for order in &mut e.ship_queue {
            if points <= 0.0 {
                break;
            }
            for (res, left) in &mut order.remaining {
                if res.idx() == r && *left > 0.0 {
                    let used = left.min(points);
                    *left -= used;
                    points -= used;
                }
            }
        }
    }
    let (done, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut e.ship_queue)
        .into_iter()
        .partition(|o| o.remaining.iter().all(|(_, left)| *left <= 1e-9));
    e.ship_queue = waiting;
    e.completed_ships.extend(done);
}

fn research(e: &mut Empire, id: EmpireId, defs: &Defs, points: f64, day: u64) {
    if e.researching.is_some_and(|t| e.knows(t)) {
        e.researching = None;
    }
    if e.researching.is_none() {
        e.researching = next_research(e, defs);
    }
    let Some(t) = e.researching else { return };
    e.research_progress += points;
    let cost = defs.tech(t).cost;
    if e.research_progress >= cost {
        e.research_progress -= cost;
        e.techs[t.idx()] = true;
        e.researching = next_research(e, defs);
        e.rebuild_modifiers(defs);
        e.events.push(
            Event::new(
                day,
                EventKind::ResearchFinished,
                format!(
                    "{} has researched {}",
                    e.display_name,
                    defs.tech(t).display_name
                ),
            )
            .empire(Some(id)),
        );
    }
}

/// The cheapest technology whose prerequisites are all known.
pub fn next_research(e: &Empire, defs: &Defs) -> Option<TechId> {
    (0..defs.technologies.len())
        .map(TechId::from_idx)
        .filter(|t| defs.tech_available(*t, &e.techs))
        .min_by(|a, b| {
            defs.tech(*a)
                .cost
                .total_cmp(&defs.tech(*b).cost)
                .then(a.cmp(b))
        })
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
        pool.scarcity[r] = now.max(pool.scarcity[r] * 0.95);
    }
}
