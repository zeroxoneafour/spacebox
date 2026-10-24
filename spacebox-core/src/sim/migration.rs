//! Hierarchical pop migration.
//!
//! Each planet offers emigrants (unemployed or homeless pops, rate-limited) and vacancies (open
//! jobs with free housing). These are matched within the system first, leftovers within the
//! owning empire, and the remainder across the whole galaxy. Moves are committed at the end of
//! the tick, so every level only ever reads state from the level below it.

use crate::ids::*;
use crate::sim::events::{Event, EventKind};
use crate::world::{Empire, System, World};

#[derive(Clone, Copy, Debug)]
pub struct Emigrants {
    pub planet: PlanetId,
    pub count: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Vacancy {
    pub planet: PlanetId,
    pub count: u64,
    /// Appeal of the best open job; migrants go to the most appealing vacancies first.
    pub appeal: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct Move {
    pub from: PlanetId,
    pub to: PlanetId,
    pub count: u64,
}

/// Greedily matches emigrants to vacancies (best appeal first), leaving unmatched leftovers.
pub(crate) fn match_migrants(
    em: &mut Vec<Emigrants>,
    vac: &mut Vec<Vacancy>,
    moves: &mut Vec<Move>,
) {
    if em.is_empty() || vac.is_empty() {
        return;
    }
    vac.sort_by(|a, b| b.appeal.cmp(&a.appeal).then(a.planet.cmp(&b.planet)));
    // `first` skips vacancies that are already full; `j` scans forward from there.
    let mut first = 0;
    for e in em.iter_mut() {
        let mut j = first;
        while e.count > 0 && j < vac.len() {
            let v = &mut vac[j];
            if v.count == 0 || v.planet == e.planet {
                if v.count == 0 && j == first {
                    first += 1;
                }
                j += 1;
                continue;
            }
            let n = e.count.min(v.count);
            moves.push(Move {
                from: e.planet,
                to: v.planet,
                count: n,
            });
            e.count -= n;
            v.count -= n;
        }
    }
    em.retain(|e| e.count > 0);
    vac.retain(|v| v.count > 0);
}

/// Empire level: pools the leftovers of the empire's planets in every system it is present in.
pub(crate) fn empire_level(
    empire: &mut Empire,
    id: EmpireId,
    present: &[SystemId],
    systems: &[System],
) {
    empire.emigrants.clear();
    empire.vacancies.clear();
    empire.moves.clear();
    for s in present {
        for g in systems[s.idx()]
            .groups
            .iter()
            .filter(|g| g.owner == Some(id))
        {
            empire.emigrants.extend_from_slice(&g.emigrants);
            empire.vacancies.extend_from_slice(&g.vacancies);
        }
    }
    match_migrants(
        &mut empire.emigrants,
        &mut empire.vacancies,
        &mut empire.moves,
    );
}

/// Galaxy level: everything left over, restricted to destinations that accept migrants.
pub(crate) fn galaxy_level(world: &mut World) {
    let mut em = Vec::new();
    let mut vac = Vec::new();
    for e in &world.empires {
        em.extend_from_slice(&e.emigrants);
        if e.accepts_migrants {
            vac.extend_from_slice(&e.vacancies);
        }
    }
    for g in world
        .systems
        .iter()
        .flat_map(|s| &s.groups)
        .filter(|g| g.owner.is_none())
    {
        em.extend_from_slice(&g.emigrants);
        vac.extend_from_slice(&g.vacancies);
    }
    world.galaxy_moves.clear();
    match_migrants(&mut em, &mut vac, &mut world.galaxy_moves);
}

/// Applies every move from all levels in a fixed order.
pub(crate) fn commit(world: &mut World) {
    let day = world.day;
    let World {
        systems,
        empires,
        planets,
        galaxy_moves,
        events,
        ..
    } = world;
    let all = systems
        .iter()
        .flat_map(|s| s.groups.iter().flat_map(|g| g.moves.iter()))
        .chain(empires.iter().flat_map(|e| e.moves.iter()))
        .chain(galaxy_moves.iter());
    for m in all {
        let n = m.count.min(planets[m.from.idx()].pop);
        if n == 0 {
            continue;
        }
        planets[m.from.idx()].pop -= n;
        let was_empty = planets[m.to.idx()].pop == 0;
        planets[m.to.idx()].pop += n;
        if was_empty {
            let p = &planets[m.to.idx()];
            let owner = p.owner;
            events.push(
                Event::new(
                    day,
                    EventKind::Settled,
                    format!(
                        "{n} {} from {} {} on {}",
                        if n == 1 { "settler" } else { "settlers" },
                        planets[m.from.idx()].display_name,
                        if n == 1 { "arrives" } else { "arrive" },
                        p.display_name
                    ),
                )
                .empire(owner)
                .planet(m.to),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_best_appeal_first() {
        let mut em = vec![Emigrants {
            planet: PlanetId(0),
            count: 10,
        }];
        let mut vac = vec![
            Vacancy {
                planet: PlanetId(1),
                count: 4,
                appeal: 10,
            },
            Vacancy {
                planet: PlanetId(2),
                count: 4,
                appeal: 50,
            },
        ];
        let mut moves = Vec::new();
        match_migrants(&mut em, &mut vac, &mut moves);
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[0].to, PlanetId(2));
        assert_eq!(em[0].count, 2);
        assert!(vac.is_empty());
    }
}
