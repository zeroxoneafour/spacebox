//! Procedural galaxy generation.
//!
//! Systems are scattered over a disc and connected by hyperlanes (a nearest-neighbour spanning
//! tree plus some extra short links). Following the design document, a system has on average
//! 0–2 habitable planets (most have none), 1–3 barren large planets and 2–4 small planets, some
//! of which orbit a gas giant. Empires start in widely spread home systems with a developed
//! capital world.
//!
//! The generator looks up the definitions it places by bare name (`capital-complex`, `farm`,
//! `housing`, ...) and silently skips any that the loaded assets do not define, so it works with
//! modded asset sets too.

use std::collections::HashSet;
use std::sync::Arc;

use crate::defs::{Defs, Kind};
use crate::error::{Error, Result};
use crate::geometry::PlanetSize;
use crate::ids::*;
use crate::rng::Rng;
use crate::world::{AiPersonality, BuildingInstance, Empire, Fleet, Planet, Ship, System, World};

#[derive(Clone, Debug)]
pub struct GalaxyParams {
    pub seed: u64,
    pub systems: usize,
    pub empires: usize,
    /// Whether generated empires are run by the built-in governor.
    pub ai: bool,
}

impl Default for GalaxyParams {
    fn default() -> Self {
        GalaxyParams {
            seed: 1,
            systems: 100,
            empires: 4,
            ai: true,
        }
    }
}

const SYLLABLES: &[&str] = &[
    "al", "an", "ar", "bel", "cor", "da", "del", "dra", "el", "en", "fa", "gal", "gor", "ha", "il",
    "is", "ka", "kel", "kor", "la", "lo", "ma", "mir", "na", "nor", "o", "or", "pa", "qua", "ra",
    "ri", "sa", "sel", "sol", "ta", "tar", "th", "ul", "va", "vel", "xa", "yr", "za", "zen",
];
const STARS: &[&str] = &[
    "yellow-small",
    "yellow",
    "orange",
    "red-dwarf",
    "red-dwarf",
    "red-dwarf",
    "white",
    "blue-giant",
    "neutron",
];
const ADJECTIVES: &[&str] = &[
    "United",
    "Free",
    "Sacred",
    "Grand",
    "Stellar",
    "Sovereign",
    "Radiant",
    "Eternal",
    "Iron",
    "Verdant",
    "Hollow",
    "Crimson",
];
const NOUNS: &[&str] = &[
    "Federation",
    "Hegemony",
    "Collective",
    "Directorate",
    "Commonwealth",
    "Syndicate",
    "Imperium",
    "Union",
    "Concord",
    "Dominion",
];
const COLORS: &[&str] = &[
    "#4e79a7", "#f28e2b", "#e15759", "#76b7b2", "#59a14f", "#edc948", "#b07aa1", "#ff9da7",
    "#9c755f", "#bab0ac",
];
const NUMERALS: &[&str] = &[
    "I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X", "XI", "XII",
];

/// A random pronounceable name not already in `used` (which it is added to).
pub fn proper_name(rng: &mut Rng, used: &mut HashSet<String>) -> String {
    loop {
        let n = rng.range(2, 3);
        let mut s = String::new();
        for _ in 0..n {
            s.push_str(rng.pick(SYLLABLES));
        }
        let mut c = s.chars();
        let name = match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => continue,
        };
        if used.insert(name.clone()) {
            return name;
        }
    }
}

/// Lower-case, dash-separated identifier for use as a TOML key.
pub fn slug(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn dist2(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

#[derive(Clone, Copy, PartialEq)]
enum Body {
    Habitable,
    BarrenLarge,
    Small,
}

/// Tile-boostable resources: stockpiled, tradeable and producible by some building.
fn boostable_resources(defs: &Defs) -> Vec<ResourceId> {
    (0..defs.resource_count())
        .map(ResourceId::from_idx)
        .filter(|r| {
            let d = defs.resource(*r);
            d.stockpile
                && d.tradeable
                && defs
                    .buildings
                    .iter()
                    .any(|b| b.outputs.iter().any(|o| o.resource == *r))
        })
        .collect()
}

/// Natural terrain for a fresh planet, per the design document: habitable worlds get seas over
/// roughly a third of their surface and blockers on roughly a third of their tiles; barren
/// worlds are covered in blockers except for 0-2 tiles. Blockers are chosen by class:
/// `barren` ones for barren worlds, `water` ones for water tiles, and the rest for land.
/// Tiles that already hold something are left alone.
pub fn generate_terrain(defs: &Defs, rng: &mut Rng, p: &mut Planet) {
    let n = p.tiles.len();
    let shape = p.size.shape();
    let blockers = |f: &dyn Fn(&crate::defs::BuildingDef) -> bool| -> Vec<BuildingId> {
        defs.buildings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.blocker && f(b))
            .map(|(i, _)| BuildingId::from_idx(i))
            .collect()
    };
    let barren = blockers(&|b| b.barren);
    let wet = blockers(&|b| b.water && !b.barren);
    let dry = blockers(&|b| !b.water && !b.barren);
    let place = |rng: &mut Rng, t: &mut crate::world::Tile, from: &[BuildingId]| {
        if t.is_free() && !from.is_empty() {
            t.building = Some(BuildingInstance::new(*rng.pick(from)));
        }
    };

    let mut order: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        order.swap(i, rng.range(0, i as u64) as usize);
    }

    if !p.habitable {
        let open = rng.range(0, 2) as usize;
        for &t in &order[open..] {
            place(rng, &mut p.tiles[t], &barren);
        }
        return;
    }

    // Seas grow outwards from a few seeds so water forms connected bodies.
    let target = (n as f64 * rng.range_f(0.25, 0.4)).round() as usize;
    let mut frontier: Vec<usize> = Vec::new();
    let mut wet_count = 0;
    while wet_count < target {
        let next = if frontier.is_empty() || rng.chance(0.12) {
            order.iter().copied().find(|t| !p.tiles[*t].water)
        } else {
            Some(frontier.swap_remove(rng.range(0, frontier.len() as u64 - 1) as usize))
        };
        let Some(t) = next else { break };
        if p.tiles[t].water {
            continue;
        }
        p.tiles[t].water = true;
        wet_count += 1;
        frontier.extend(
            shape
                .neighbors(t)
                .iter()
                .map(|&x| x as usize)
                .filter(|x| !p.tiles[*x].water),
        );
        // Reshuffle where new seeds come from so separate seas appear in different places.
        order.rotate_left(1);
    }

    let blocked = (n as f64 * rng.range_f(0.25, 0.4)).round() as usize;
    let mut picks: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        picks.swap(i, rng.range(0, i as u64) as usize);
    }
    for &t in picks.iter().take(blocked) {
        let from = if p.tiles[t].water { &wet } else { &dry };
        place(rng, &mut p.tiles[t], from);
    }
}

/// Generates a random unowned system at `pos` following the design distribution. `used`
/// holds display names already taken and is updated with the new one.
pub fn random_system(
    defs: &Defs,
    rng: &mut Rng,
    used: &mut HashSet<String>,
    pos: [f64; 2],
) -> (System, Vec<Planet>) {
    let n_res = defs.resource_count();
    let boostable = boostable_resources(defs);
    let name = proper_name(rng, used);
    let mut sys = System::new(slug(&name), name.clone());
    sys.star_type = rng.pick(STARS).to_string();
    sys.star_count = if rng.chance(0.15) { 2 } else { 1 };
    sys.coordinates = [
        (pos[0] * 100.0).round() / 100.0,
        (pos[1] * 100.0).round() / 100.0,
    ];

    let roll = rng.f64();
    let habitable = if roll < 0.6 {
        0
    } else if roll < 0.9 {
        1
    } else {
        2
    };
    let mut bodies = vec![Body::Habitable; habitable];
    bodies.extend(std::iter::repeat_n(
        Body::BarrenLarge,
        rng.range(1, 3) as usize,
    ));
    bodies.extend(std::iter::repeat_n(Body::Small, rng.range(2, 4) as usize));
    for i in (1..bodies.len()).rev() {
        bodies.swap(i, rng.range(0, i as u64) as usize);
    }
    let giant = format!("{name} Major");
    // Planets sit on fixed, spread-out orbits under 100 units from the star; moons cluster
    // around their (unrepresented) gas giant.
    let orbit_gap = 80.0 / bodies.len() as f64;
    let giant_r = 20.0 + rng.f64() * 60.0;
    let giant_a = rng.f64() * std::f64::consts::TAU;
    let giant_at = [giant_r * giant_a.cos(), giant_r * giant_a.sin()];
    let mut planets = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        let (size, hab) = match body {
            Body::Habitable => (PlanetSize::Large, true),
            Body::BarrenLarge => (PlanetSize::Large, false),
            Body::Small => (PlanetSize::Small, false),
        };
        let mut p = Planet::new(
            format!("{}-{}", slug(&name), i + 1),
            format!("{name} {}", NUMERALS[i.min(NUMERALS.len() - 1)]),
            size,
            hab,
            n_res,
        );
        let round = |v: f64| (v * 10.0).round() / 10.0;
        if *body == Body::Small && rng.chance(0.4) {
            p.moon_of = Some(giant.clone());
            let a = rng.f64() * std::f64::consts::TAU;
            p.coordinates = [
                round(giant_at[0] + 6.0 * a.cos()),
                round(giant_at[1] + 6.0 * a.sin()),
            ];
        } else {
            let r = 12.0 + i as f64 * orbit_gap + rng.f64() * orbit_gap * 0.5;
            let a = rng.f64() * std::f64::consts::TAU;
            p.coordinates = [round(r * a.cos()), round(r * a.sin())];
        }
        for t in &mut p.tiles {
            if !boostable.is_empty() && rng.chance(0.12) {
                let r = *rng.pick(&boostable);
                let m = if rng.chance(0.8) {
                    rng.range_f(1.1, 1.5)
                } else {
                    rng.range_f(0.6, 0.9)
                };
                t.resource_mults.push((r, (m * 100.0).round() / 100.0));
            }
        }
        generate_terrain(defs, rng, &mut p);
        planets.push(p);
    }
    (sys, planets)
}

pub fn generate(defs: Arc<Defs>, params: &GalaxyParams) -> Result<World> {
    if params.systems == 0 {
        return Err(Error::Other("a galaxy needs at least one system".into()));
    }
    if params.empires > params.systems {
        return Err(Error::Other("more empires than systems".into()));
    }
    let ruleset = RulesetId(defs.find(Kind::Ruleset, "default").unwrap_or(0));
    if defs.rulesets.is_empty() {
        return Err(Error::Other("assets define no ruleset".into()));
    }
    let mut rng = Rng::new(params.seed);
    let mut world = World::new(defs.clone(), ruleset);
    world.seed = params.seed;
    let mut used = HashSet::new();

    // Positions, kept under 100 units from the galactic centre. Large galaxies get denser.
    let natural = (params.systems as f64).sqrt() * 8.0;
    let radius = natural.min(95.0);
    let scale = radius / natural;
    let min_sep2 = 9.0 * scale * scale;
    let mut positions: Vec<[f64; 2]> = Vec::with_capacity(params.systems);
    while positions.len() < params.systems {
        let mut best = None;
        for _ in 0..30 {
            let r = radius * rng.f64().sqrt();
            let a = rng.f64() * std::f64::consts::TAU;
            let p = [r * a.cos(), r * a.sin()];
            if positions.iter().all(|q| dist2(p, *q) > min_sep2) {
                best = Some(p);
                break;
            }
            best.get_or_insert(p);
        }
        positions.push(best.unwrap());
    }

    // Systems and planets.
    for pos in &positions {
        let (sys, planets) = random_system(&defs, &mut rng, &mut used, *pos);
        world.add_system(sys, planets);
    }

    // Hyperlanes: spanning tree to the nearest earlier system, plus extra short links.
    let n = positions.len();
    for i in 1..n {
        let j = (0..i)
            .min_by(|&a, &b| {
                dist2(positions[i], positions[a]).total_cmp(&dist2(positions[i], positions[b]))
            })
            .unwrap();
        world.link(SystemId::from_idx(i), SystemId::from_idx(j));
    }
    for i in 0..n {
        // The two nearest other systems, by linear scan.
        let mut near = [(f64::MAX, usize::MAX); 2];
        for j in (0..n).filter(|&j| j != i) {
            let d = dist2(positions[i], positions[j]);
            if d < near[0].0 {
                near = [(d, j), near[0]];
            } else if d < near[1].0 {
                near[1] = (d, j);
            }
        }
        for (d, j) in near {
            if j != usize::MAX && d < 225.0 * scale * scale && rng.chance(0.5) {
                world.link(SystemId::from_idx(i), SystemId::from_idx(j));
            }
        }
    }

    // Empires: farthest-point sampling of home systems.
    let mut homes: Vec<usize> = Vec::new();
    if params.empires > 0 {
        homes.push(rng.range(0, n as u64 - 1) as usize);
        while homes.len() < params.empires {
            let next = (0..n)
                .filter(|i| !homes.contains(i))
                .max_by(|&a, &b| {
                    let d = |x: usize| {
                        homes
                            .iter()
                            .map(|h| dist2(positions[x], positions[*h]))
                            .fold(f64::MAX, f64::min)
                    };
                    d(a).total_cmp(&d(b))
                })
                .unwrap();
            homes.push(next);
        }
    }

    let b = |name: &str| defs.find_building(name);
    let capital_layout: Vec<BuildingId> = [
        "capital-complex",
        "farm",
        "farm",
        "power-plant",
        "mine",
        "housing",
        "civilian-factory",
        "research-lab",
        "alloy-forge",
        "mine",
        "power-plant",
        "administrative-office",
        "shipyard",
    ]
    .iter()
    .filter_map(|n| b(n))
    .collect();
    let barren_layout: Vec<BuildingId> = ["mining-outpost", "solar-array"]
        .iter()
        .filter_map(|n| b(n))
        .collect();
    let starting_stock = [
        ("food", 800.0),
        ("minerals", 1500.0),
        ("energy", 1000.0),
        ("alloy", 300.0),
        ("consumer-goods", 400.0),
        ("influence", 200.0),
    ];
    let starting_fleet: Vec<ShipId> = ["corvette", "corvette", "corvette"]
        .iter()
        .filter_map(|n| defs.find_ship(n))
        .collect();

    for (k, &home) in homes.iter().enumerate() {
        let home_id = SystemId::from_idx(home);
        let sys_name = world.systems[home].display_name.clone();
        let display = format!(
            "{} {} of {}",
            rng.pick(ADJECTIVES),
            rng.pick(NOUNS),
            sys_name
        );
        let mut e = Empire::new(format!("empire-{}", k + 1), display, &defs);
        e.color = Some(COLORS[k % COLORS.len()].to_string());
        // Each AI empire gets its own temperament.
        e.ai = params.ai.then(|| {
            let mut t = || (rng.range_f(0.1, 0.95) * 100.0).round() / 100.0;
            AiPersonality {
                militant: t(),
                expansionist: t(),
                scientific: t(),
            }
        });
        e.capital = Some(home_id);
        for (r, amount) in starting_stock {
            if let Some(r) = defs.find_resource(r) {
                e.storage[r.idx()] = amount;
            }
        }
        let eid = world.add_empire(e);
        world.systems[home].owner = Some(eid);

        // Make sure the home system has a habitable large capital world.
        let range = world.systems[home].planet_range();
        let capital = range
            .clone()
            .find(|&p| world.planets[p].habitable)
            .or_else(|| {
                range
                    .clone()
                    .find(|&p| world.planets[p].size == PlanetSize::Large)
            })
            .unwrap_or(range.start);
        {
            let p = &mut world.planets[capital];
            if p.size == PlanetSize::Small {
                p.size = PlanetSize::Large;
                p.tiles
                    .resize(PlanetSize::Large.tile_count(), Default::default());
                p.moon_of = None;
            }
            if !p.habitable {
                // A barren world promoted to capital gets habitable terrain instead.
                p.habitable = true;
                for t in &mut p.tiles {
                    t.building = None;
                }
                generate_terrain(&defs, &mut rng, p);
            }
            p.pop = 400;
            // The developed core sits on land, clearing whatever blockers were there.
            let land: Vec<usize> = (0..p.tiles.len()).filter(|t| !p.tiles[*t].water).collect();
            for (t, def) in land.into_iter().zip(&capital_layout) {
                p.tiles[t].building = Some(BuildingInstance::new(*def));
            }
        }
        if let Some(barren) = range
            .clone()
            .find(|&p| p != capital && !world.planets[p].habitable)
        {
            // Outposts replace the blockers on the first tiles.
            for (t, def) in barren_layout.iter().enumerate() {
                world.planets[barren].tiles[t].building = Some(BuildingInstance::new(*def));
            }
        }

        // Claim up to two unowned neighbouring systems.
        let claims: Vec<SystemId> = world.systems[home]
            .links
            .iter()
            .copied()
            .filter(|s| world.systems[s.idx()].owner.is_none() && !homes.contains(&s.idx()))
            .take(2)
            .collect();
        for nb in claims.into_iter().chain([home_id]) {
            world.systems[nb.idx()].owner = Some(eid);
            for p in world.systems[nb.idx()].planet_range() {
                world.planets[p].owner = Some(eid);
            }
        }

        if !starting_fleet.is_empty() {
            let mut fleet =
                Fleet::new("home-guard", format!("{sys_name} Home Guard"), eid, home_id);
            fleet.coordinates = world.planets[capital].coordinates;
            fleet.ships = starting_fleet
                .iter()
                .map(|s| Ship::new(*s, &defs))
                .collect();
            world.add_fleet(fleet);
        }
    }
    world.topology_dirty = true;
    Ok(world)
}
