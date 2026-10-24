//! End-to-end tests against the shipped data files and small hand-made worlds.

use std::path::{Path, PathBuf};

use spacebox_core::defs::Kind;
use spacebox_core::sim::planet::building_profile;
use spacebox_core::{Command, EventKind, World, load_world, save};

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../data")
        .canonicalize()
        .unwrap()
}

/// A fresh temporary directory per test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spacebox-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes a scenario that imports the base data files under the usual aliases.
fn scenario(dir: &Path, body: &str) -> PathBuf {
    let d = data_dir();
    let text = format!(
        "[import]\nresources = \"{0}/resources.toml\"\nbuildings = \"{0}/buildings.toml\"\n\
         technologies = \"{0}/technologies.toml\"\nrules = \"{0}/rules.toml\"\n\n{body}",
        d.display()
    );
    let path = dir.join("scenario.toml");
    std::fs::write(&path, text).unwrap();
    path
}

fn sol() -> World {
    load_world(data_dir().join("scenarios/sol.toml")).unwrap().0
}

fn load_err(path: &Path) -> String {
    load_world(path).unwrap_err().to_string()
}

#[test]
fn loads_the_sol_scenario() {
    let w = sol();
    assert_eq!(w.empires.len(), 2);
    assert_eq!(w.systems.len(), 5);
    let earth = w.planet(w.planet_by_name("sol/sol-3").unwrap());
    assert_eq!(earth.display_name, "Earth");
    assert_eq!(earth.pop, 800);
    let forge = earth.tiles[0].building.as_ref().unwrap();
    assert_eq!(w.defs.building(forge.def).key.name, "alloy-forge");
    assert_eq!(w.defs.upgrade(forge.upgrades[0]).key.name, "faster-forge");
    assert_eq!(forge.workers, 100);
    assert_eq!(earth.tiles[0].resource_mults.len(), 2);
    // Links are symmetric even though only one side may list them.
    let sol = w.system_by_name("sol").unwrap();
    let ac = w.system_by_name("alpha-centauri").unwrap();
    assert!(w.system(ac).links.contains(&sol));
}

#[test]
fn imports_do_not_recurse() {
    let dir = temp_dir("imports");
    std::fs::write(
        dir.join("c.toml"),
        "[resource.ore]\ndisplay_name = \"Ore\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b.toml"),
        "[import]\nc = \"c.toml\"\n\n[job.digger]\ndisplay_name = \"Digger\"\nappeal = 1\n\
         upkeep = [{ resource = \"c.ore\", count = 1 }]\n",
    )
    .unwrap();
    // `a` imports `b` only, so it cannot see `c`'s resources even though they are loaded.
    std::fs::write(
        dir.join("a.toml"),
        "[import]\nb = \"b.toml\"\n\n[building.pit]\ndisplay_name = \"Pit\"\nmax_workers = 1\n\
         job = \"b.digger\"\noutput_resources = [{ resource = \"c.ore\", count = 1 }]\n",
    )
    .unwrap();
    let err = load_err(&dir.join("a.toml"));
    assert!(err.contains("unknown import alias 'c'"), "{err}");

    // Importing `c` directly fixes it.
    std::fs::write(
        dir.join("a.toml"),
        "[import]\nb = \"b.toml\"\nc = \"c.toml\"\n\n[building.pit]\ndisplay_name = \"Pit\"\n\
         max_workers = 1\njob = \"b.digger\"\noutput_resources = [{ resource = \"c.ore\", count = 1 }]\n",
    )
    .unwrap();
    let (w, _) = load_world(dir.join("a.toml")).unwrap();
    assert_eq!(
        w.defs.buildings[0].outputs[0].resource,
        w.defs.find_resource("ore").unwrap()
    );
}

#[test]
fn references_are_typed_by_slot() {
    let dir = temp_dir("typed");
    // `jobs.farmer` exists, but a `resource` slot only looks in [resource.*] tables.
    let d = data_dir();
    std::fs::write(
        dir.join("x.toml"),
        format!(
            "[import]\njobs = \"{}/jobs.toml\"\n\n[building.x]\ndisplay_name = \"X\"\n\
             output_resources = [{{ resource = \"jobs.farmer\", count = 1 }}]\n",
            d.display()
        ),
    )
    .unwrap();
    let err = load_err(&dir.join("x.toml"));
    assert!(err.contains("unknown resource 'jobs.farmer'"), "{err}");
}

#[test]
fn import_must_come_first_and_fields_are_checked() {
    let dir = temp_dir("strict");
    std::fs::write(
        dir.join("x.toml"),
        "[resource.a]\ndisplay_name = \"A\"\n\n[import]\nb = \"b.toml\"\n",
    )
    .unwrap();
    assert!(load_err(&dir.join("x.toml")).contains("[import] must be the first section"));

    std::fs::write(
        dir.join("y.toml"),
        "[resource.a]\ndisplay_name = \"A\"\ncolour = 1\n",
    )
    .unwrap();
    assert!(load_err(&dir.join("y.toml")).contains("colour"));

    std::fs::write(dir.join("z.toml"), "[resorce.a]\ndisplay_name = \"A\"\n").unwrap();
    assert!(load_err(&dir.join("z.toml")).contains("unknown top-level table [resorce]"));
}

#[test]
fn barren_buildings_cannot_have_workers() {
    let dir = temp_dir("barren");
    let d = data_dir();
    std::fs::write(
        dir.join("x.toml"),
        format!(
            "[import]\njobs = \"{}/jobs.toml\"\n\n[building.bad]\ndisplay_name = \"Bad\"\n\
             building_class = [\"barren\"]\nmax_workers = 10\njob = \"jobs.miner\"\n",
            d.display()
        ),
    )
    .unwrap();
    assert!(load_err(&dir.join("x.toml")).contains("max_workers = 0"));
}

#[test]
fn adjacency_and_upgrades_shape_profiles() {
    let w = sol();
    let earth = w.planet(w.planet_by_name("sol/sol-3").unwrap());
    let owner = w
        .planet_owner(w.planet_by_name("sol/sol-3").unwrap())
        .map(|e| w.empire(e));
    let minerals = w.defs.find_resource("minerals").unwrap();
    let alloy = w.defs.find_resource("alloy").unwrap();
    let electricity = w.defs.find_resource("energy").unwrap();
    let forge = building_profile(&w.defs, earth, 0, owner).unwrap();
    // Inputs: base 20 minerals, ×1.25 (faster-forge), ×0.95 (the mine on adjacent tile 2).
    let input = forge.inputs.iter().find(|(r, _)| *r == minerals).unwrap().1;
    assert!((input - 20.0 * 1.25 * 0.95).abs() < 1e-9, "{input}");
    // Output ×1.25 (upgrade) ×1.1 (tile multiplier).
    let output = forge.outputs.iter().find(|(r, _)| *r == alloy).unwrap().1;
    assert!((output - 6.0 * 1.25 * 1.1).abs() < 1e-9, "{output}");
    // Upkeep ×1.1 (upgrade) ×0.9 (adjacent power plant on tile 5).
    let maint = forge
        .upkeep
        .iter()
        .find(|(r, _)| *r == electricity)
        .unwrap()
        .1;
    assert!((maint - 5.0 * 1.1 * 0.9).abs() < 1e-9, "{maint}");
}

#[test]
fn upgrade_dependencies_are_enforced() {
    let mut w = sol();
    let earth = w.planet_by_name("sol/sol-3").unwrap();
    let ceti = w.planet_by_name("tau-ceti/tau-ceti-e").unwrap();
    let superconducting = w.defs.find_upgrade("superconducting-forge").unwrap();
    // Tau Ceti's forge lacks faster-forge (and the technology).
    let err = w
        .apply_now(Command::Upgrade {
            planet: ceti,
            tile: 10,
            upgrade: superconducting,
        })
        .unwrap_err();
    assert!(err.contains("first needs"), "{err}");
    // Earth's forge has faster-forge but the empire lacks Advanced Alloys.
    let err = w
        .apply_now(Command::Upgrade {
            planet: earth,
            tile: 0,
            upgrade: superconducting,
        })
        .unwrap_err();
    assert!(err.contains("requires Advanced Alloys"), "{err}");
    // Upgrades are only available where apply_to matches.
    let drills = w.defs.find_upgrade("deep-core-drills").unwrap();
    let err = w
        .apply_now(Command::Upgrade {
            planet: earth,
            tile: 0,
            upgrade: drills,
        })
        .unwrap_err();
    assert!(err.contains("cannot be applied"), "{err}");
}

#[test]
fn barren_planets_only_accept_barren_buildings() {
    let mut w = sol();
    let mars = w.planet_by_name("sol/sol-4").unwrap();
    let farm = w.defs.find_building("farm").unwrap();
    let outpost = w.defs.find_building("mining-outpost").unwrap();
    assert!(
        w.apply_now(Command::Build {
            planet: mars,
            tile: 10,
            building: farm,
            empire: None,
        })
        .unwrap_err()
        .contains("barren")
    );
    w.apply_now(Command::Build {
        planet: mars,
        tile: 10,
        building: outpost,
        empire: None,
    })
    .unwrap();
    assert!(w.planet(mars).tiles[10].construction.is_some());
}

#[test]
fn terraforming_requires_technology_then_makes_planets_habitable() {
    let mut w = sol();
    let venus = w.planet_by_name("sol/sol-1").unwrap();
    let err = w
        .apply_now(Command::Terraform { planet: venus })
        .unwrap_err();
    assert!(err.contains("Atmospheric Engineering"), "{err}");

    let terran = w.empire_by_name("empire-1").unwrap();
    let tech = w.defs.find_tech("atmospheric-engineering").unwrap();
    w.empires[terran.idx()].techs[tech.idx()] = true;
    for (name, n) in [
        ("minerals", 5000.0),
        ("alloy", 5000.0),
        ("exotic-gases", 500.0),
    ] {
        let r = w.defs.find_resource(name).unwrap();
        w.apply_now(Command::GrantResources {
            empire: terran,
            resource: r,
            count: n,
        })
        .unwrap();
    }
    w.apply_now(Command::Terraform { planet: venus }).unwrap();
    w.empires.iter_mut().for_each(|e| e.ai = None);
    w.run(360);
    assert!(w.planet(venus).habitable);
    assert!(
        w.drain_events()
            .iter()
            .any(|e| e.kind == EventKind::Terraformed)
    );
}

#[test]
fn save_round_trip_preserves_state() {
    let mut w = sol();
    w.run(120);
    let dir = temp_dir("save");
    let path = dir.join("out.toml");
    save::save(&w, &path).unwrap();
    let (back, warnings) = load_world(&path).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(back.day, w.day);
    assert_eq!(back.planets.len(), w.planets.len());
    for (a, b) in w.planets.iter().zip(&back.planets) {
        assert_eq!(a.pop, b.pop, "{}", a.name);
        assert_eq!(a.habitable, b.habitable);
        for (ta, tb) in a.tiles.iter().zip(&b.tiles) {
            assert_eq!(
                ta.building.as_ref().map(|x| (x.def, x.upgrades.clone())),
                tb.building.as_ref().map(|x| (x.def, x.upgrades.clone()))
            );
            assert_eq!(ta.construction.is_some(), tb.construction.is_some());
            assert_eq!(ta.water, tb.water);
        }
    }
    for (a, b) in w.empires.iter().zip(&back.empires) {
        assert_eq!(a.techs, b.techs);
        for (x, y) in a.storage.iter().zip(&b.storage) {
            assert!((x - y).abs() < 1e-3);
        }
    }
    // The mod's building and the cross-file upgrade survive.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("orbital-agriculture.orbital-farm"));
}

#[test]
fn simulation_is_deterministic() {
    let run = || {
        let mut w = sol();
        w.run(300);
        let pops: Vec<u64> = w.planets.iter().map(|p| p.pop).collect();
        let stores: Vec<Vec<f64>> = w.empires.iter().map(|e| e.storage.clone()).collect();
        (pops, stores, w.drain_events().len())
    };
    assert_eq!(run(), run());
}

#[test]
fn famine_kills_pops_and_starts_a_crisis() {
    let dir = temp_dir("famine");
    let path = scenario(
        &dir,
        r#"
[empire.e]
display_name = "Hungry"
resource_stores = [{ resource = "resources.food", count = 10 }]

[system.s]
display_name = "S"
coordinates = { x = 10, y = 0 }
owned_by = "e"
[system.s.planets.p]
display_name = "P"
total_pop = 300
"#,
    );
    let (mut w, _) = load_world(&path).unwrap();
    w.run(60);
    let events = w.drain_events();
    assert!(
        events.iter().any(|e| e.kind == EventKind::CrisisStarted),
        "{events:?}"
    );
    assert!(events.iter().any(|e| e.kind == EventKind::Starvation));
    assert!(w.planets[0].pop < 300);
}

#[test]
fn unemployed_pops_migrate_to_open_jobs() {
    let dir = temp_dir("migrate");
    let path = scenario(
        &dir,
        r#"
[empire.e]
display_name = "Movers"
resource_stores = [
    { resource = "resources.food", count = 100000 },
    { resource = "resources.consumer-goods", count = 100000 },
    { resource = "resources.energy", count = 100000 },
]

[system.a]
display_name = "A"
coordinates = { x = 20, y = 0 }
owned_by = "e"
linked_systems = ["b"]
[system.a.planets.crowded]
display_name = "Crowded"
total_pop = 150

[system.b]
display_name = "B"
coordinates = { x = 30, y = 0 }
owned_by = "e"
[system.b.planets.frontier]
display_name = "Frontier"
[system.b.planets.frontier.tiles.0]
building = "buildings.farm"
"#,
    );
    let (mut w, _) = load_world(&path).unwrap();
    w.run(90);
    let frontier = w.planet(w.planet_by_name("frontier").unwrap());
    assert!(frontier.pop >= 90, "frontier pop {}", frontier.pop);
    assert!(
        w.drain_events()
            .iter()
            .any(|e| e.kind == EventKind::Settled)
    );
}

#[test]
fn pinned_workers_override_appeal() {
    let mut w = sol();
    w.empires.iter_mut().for_each(|e| e.ai = None);
    let earth = w.planet_by_name("sol/sol-3").unwrap();
    // The administrative office (clerks, lowest appeal) gets exactly 50 workers when pinned.
    w.queue(Command::AssignWorkers {
        planet: earth,
        tile: 13,
        workers: Some(50),
    });
    w.tick();
    assert_eq!(
        w.planet(earth).tiles[13].building.as_ref().unwrap().workers,
        50
    );
    // Restricting to 10 jobs caps it.
    w.queue(Command::SetJobCap {
        planet: earth,
        tile: 13,
        cap: Some(10),
    });
    w.tick();
    assert!(w.planet(earth).tiles[13].building.as_ref().unwrap().workers <= 10);
}

#[test]
fn galactic_market_moves_goods_between_empires() {
    let dir = temp_dir("market");
    let path = scenario(
        &dir,
        r#"
[empire.rich]
display_name = "Rich"
resource_stores = [
    { resource = "resources.food", count = 50000 },
    { resource = "resources.consumer-goods", count = 50000 },
    { resource = "resources.energy", count = 1000 },
]
[empire.poor]
display_name = "Poor"
resource_stores = [
    { resource = "resources.consumer-goods", count = 50000 },
    { resource = "resources.energy", count = 50000 },
]

[system.r]
display_name = "R"
coordinates = { x = 40, y = 0 }
owned_by = "rich"
[system.r.planets.r1]
display_name = "R1"
total_pop = 10

[system.p]
display_name = "P"
coordinates = { x = 50, y = 0 }
owned_by = "poor"
[system.p.planets.p1]
display_name = "P1"
total_pop = 200
"#,
    );
    let (mut w, _) = load_world(&path).unwrap();
    w.run(5);
    let food = w.defs.find_resource("food").unwrap();
    let poor = w.empire(w.empire_by_name("poor").unwrap());
    assert!(
        poor.trade[food.idx()] > 0.0,
        "poor empire should be buying food"
    );
    assert!(
        poor.crises.is_empty(),
        "market purchases should prevent a famine"
    );
    assert!(w.market[food.idx()].volume > 0.0);
}

#[test]
fn disconnected_systems_lose_access_to_storage() {
    let mut w = sol();
    w.empires.iter_mut().for_each(|e| e.ai = None);
    w.tick();
    let terran = w.empire_by_name("empire-1").unwrap();
    let core = w.empire(terran).core_pool.unwrap();
    let ac = w.system_by_name("alpha-centauri").unwrap();
    let proxima = w.planet_by_name("alpha-centauri/proxima-b").unwrap();
    assert_eq!(w.planet(proxima).pool, core);
    // Cutting the only hyperlane isolates Alpha Centauri into its own pool.
    let sol = w.system_by_name("sol").unwrap();
    w.queue(Command::Unlink { a: sol, b: ac });
    w.tick();
    let core = w.empire(terran).core_pool.unwrap();
    assert_ne!(w.planet(proxima).pool, core);
    assert!(!w.pools[w.planet(proxima).pool.idx()].core);
}

#[test]
fn generated_galaxies_follow_the_design_distribution() {
    let (defs, _) = spacebox_core::load_assets(data_dir().join("base.toml")).unwrap();
    let params = spacebox_core::generate::GalaxyParams {
        seed: 9,
        systems: 300,
        empires: 5,
        ai: true,
    };
    let mut w = spacebox_core::generate::generate(defs, &params).unwrap();
    assert_eq!(w.systems.len(), 300);
    let per_system = w.planets.len() as f64 / 300.0;
    assert!((3.0..=9.0).contains(&per_system), "{per_system}");
    let without_habitable = w
        .systems
        .iter()
        .filter(|s| s.planet_ids().all(|p| !w.planet(p).habitable))
        .count();
    assert!(
        without_habitable > 150,
        "most systems should have no habitable planet"
    );
    assert!(w.defs.find(Kind::Ruleset, "default").is_some());
    w.run(30);
    assert!(w.empires.iter().all(|e| e.stats.pop > 0));
}

#[test]
fn structural_edits_keep_ids_consistent() {
    let mut w = sol();
    w.run(5);
    let sol_id = w.system_by_name("sol").unwrap();
    let ac = w.system_by_name("alpha-centauri").unwrap();
    let tau = w.system_by_name("tau-ceti").unwrap();
    let ceti_prime = w
        .planet(w.planet_by_name("tau-ceti/tau-ceti-e").unwrap())
        .pop;

    // Removing Alpha Centauri shifts later ids down and drops its hyperlanes.
    w.remove_system(ac).unwrap();
    assert_eq!(w.systems.len(), 4);
    assert!(w.system_by_name("alpha-centauri").is_none());
    let tau_now = w.system_by_name("tau-ceti").unwrap();
    assert_eq!(tau_now.0, tau.0 - 1);
    let tau_capital = w.empire(w.empire_by_name("empire-2").unwrap()).capital;
    assert_eq!(tau_capital, Some(tau_now));
    assert!(
        w.systems
            .iter()
            .all(|s| s.links.iter().all(|l| l.idx() < 4))
    );
    assert!(
        !w.system(sol_id)
            .links
            .iter()
            .any(|l| w.system(*l).name == "alpha-centauri")
    );
    for (i, s) in w.systems.iter().enumerate() {
        for p in s.planet_ids() {
            assert_eq!(w.planet(p).system.idx(), i);
        }
    }
    assert_eq!(
        w.planet(w.planet_by_name("tau-ceti/tau-ceti-e").unwrap())
            .pop,
        ceti_prime
    );

    // Adding and removing planets mid-list keeps the layout contiguous.
    let pid = w
        .add_planet(
            sol_id,
            spacebox_core::world::Planet::new(
                "sol-9",
                "Planet Nine",
                spacebox_core::geometry::PlanetSize::Small,
                false,
                w.defs.resource_count(),
            ),
        )
        .unwrap();
    assert_eq!(w.planet(pid).display_name, "Planet Nine");
    assert_eq!(w.planet(pid).system, sol_id);
    w.remove_planet(w.planet_by_name("sol/sol-4").unwrap())
        .unwrap();
    assert!(w.planet_by_name("sol/sol-4").is_none());
    w.run(30);
    let earth = w.planet(w.planet_by_name("sol/sol-3").unwrap());
    assert!(earth.pop > 0 && earth.tiles[0].building.is_some());
}

// ---------------------------------------------------------------------------------------------
// Expansion, ships and fleets
// ---------------------------------------------------------------------------------------------

fn quiet_sol() -> World {
    let mut w = sol();
    w.empires.iter_mut().for_each(|e| e.ai = None);
    w
}

#[test]
fn expansion_costs_influence_and_time_by_distance() {
    let w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let influence = w.defs.find_resource("influence").unwrap();
    let outpost = w.defs.find_building("solar-array").unwrap();
    let base_days = w.defs.building(outpost).build_days;
    // Barnard's Star is adjacent to Terran space (n = 0): 100 influence, +30 days.
    let barnard = w.planet_by_name("barnards-star/barnard-b").unwrap();
    let plan = w.build_plan(terran, barnard, 0, outpost).unwrap();
    assert_eq!(plan.expansion_jumps, Some(1));
    let cost = |plan: &spacebox_core::sim::commands::BuildPlan| {
        plan.cost
            .iter()
            .find(|a| a.resource == influence)
            .map(|a| a.count)
            .unwrap()
    };
    assert_eq!(cost(&plan), 100.0);
    assert_eq!(plan.days, base_days + 30);
    // Owned systems cost no influence; a system owned by a rival cannot be built in at all.
    let mars = w.planet_by_name("sol/sol-4").unwrap();
    assert!(
        w.build_plan(terran, mars, 10, outpost)
            .unwrap()
            .expansion_jumps
            .is_none()
    );
    let ceti_f = w.planet_by_name("tau-ceti/tau-ceti-f").unwrap();
    let err = w.build_plan(terran, ceti_f, 5, outpost).unwrap_err();
    assert!(
        err.contains("you cannot build in a system you do not own"),
        "{err}"
    );
    // Each extra jump doubles the influence and multiplies days by √2.
    let rules = w.rules();
    assert_eq!(rules.expansion(2), (200.0, 42));
    assert_eq!(rules.expansion(3), (400.0, 60));
}

#[test]
fn finishing_a_claim_takes_the_system_and_refunds_rivals() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let cetian = w.empire_by_name("empire-2").unwrap();
    let outpost = w.defs.find_building("solar-array").unwrap();
    let barnard_b = w.planet_by_name("barnards-star/barnard-b").unwrap();
    let barnard_c = w.planet_by_name("barnards-star/barnard-c").unwrap();
    let minerals = w.defs.find_resource("minerals").unwrap();
    w.apply_now(Command::Build {
        planet: barnard_b,
        tile: 0,
        building: outpost,
        empire: Some(terran),
    })
    .unwrap();
    w.run(5);
    let before = w.empire(cetian).storage[minerals.idx()];
    w.apply_now(Command::Build {
        planet: barnard_c,
        tile: 0,
        building: outpost,
        empire: Some(cetian),
    })
    .unwrap();
    let paid = before - w.empire(cetian).storage[minerals.idx()];
    assert!(paid > 0.0);
    w.run(120);
    let sys = w.system_by_name("barnards-star").unwrap();
    assert_eq!(w.system(sys).owner, Some(terran));
    assert!(
        w.system(sys)
            .planet_ids()
            .all(|p| w.planet(p).owner == Some(terran))
    );
    assert!(
        w.planet(barnard_c).tiles[0].construction.is_none(),
        "rival site cancelled"
    );
    let events = w.drain_events();
    assert!(events.iter().any(|e| e.kind == EventKind::SystemClaimed));
    assert!(events.iter().any(|e| e.kind == EventKind::ClaimLost));
}

#[test]
fn influence_accrues_daily_up_to_its_cap() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let influence = w.defs.find_resource("influence").unwrap();
    let start = w.empire(terran).storage[influence.idx()];
    w.run(10);
    assert!((w.empire(terran).storage[influence.idx()] - (start + 50.0)).abs() < 1e-6);
    w.run(400);
    assert_eq!(w.empire(terran).storage[influence.idx()], 1000.0);
}

#[test]
fn shipyards_build_ships_on_demand() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let earth = w.planet_by_name("sol/sol-3").unwrap();
    let corvette = w.defs.find_ship("corvette").unwrap();
    let ships_before: usize = w
        .fleets
        .iter()
        .filter(|f| f.owner == terran)
        .map(|f| f.ships.len())
        .sum();
    // Idle shipyards employ nobody.
    w.tick();
    assert_eq!(
        w.planet(earth).tiles[10].building.as_ref().unwrap().workers,
        0
    );
    w.apply_now(Command::BuildShip {
        planet: earth,
        ship: corvette,
        fleet: None,
    })
    .unwrap();
    // Mars has no shipyard.
    let mars = w.planet_by_name("sol/sol-4").unwrap();
    assert!(
        w.apply_now(Command::BuildShip {
            planet: mars,
            ship: corvette,
            fleet: None
        })
        .is_err()
    );
    w.run(2);
    assert!(w.planet(earth).tiles[10].building.as_ref().unwrap().workers > 0);
    w.run(30);
    let ships_after: usize = w
        .fleets
        .iter()
        .filter(|f| f.owner == terran)
        .map(|f| f.ships.len())
        .sum();
    assert_eq!(ships_after, ships_before + 1);
    assert!(w.empire(terran).ship_queue.is_empty());
    assert!(
        w.drain_events()
            .iter()
            .any(|e| e.kind == EventKind::ShipLaunched)
    );
}

#[test]
fn ship_stats_combine_hull_weapons_and_outfits() {
    let w = quiet_sol();
    let fighter = w.defs.ship(w.defs.find_ship("fighter").unwrap());
    // 350 shield + 50 from the booster; regen 10 x 1.5; speed 10 x 1.3 (afterburner).
    assert_eq!(fighter.max_shield, 400.0);
    assert_eq!(fighter.shield_regen, 15.0);
    assert!((fighter.speed - 13.0).abs() < 1e-9);
    assert_eq!(fighter.armament.len(), 2);
    let alloy = w.defs.find_resource("alloy").unwrap();
    let alloy_cost = fighter
        .build_cost
        .iter()
        .find(|a| a.resource == alloy)
        .unwrap()
        .count;
    assert_eq!(alloy_cost, 50.0 + 10.0 * 2.0 + 25.0 + 15.0);
}

#[test]
fn fleets_jump_fight_and_conquer() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let cetian = w.empire_by_name("empire-2").unwrap();
    let home = w.fleets.iter().find(|f| f.owner == terran).unwrap().id;
    let ceti_f = w.planet_by_name("tau-ceti/tau-ceti-f").unwrap();
    let ceti_prime = w.planet_by_name("tau-ceti/tau-ceti-e").unwrap();

    // Bombardment needs a war.
    assert!(
        w.apply_now(Command::Bombard {
            fleet: home,
            planet: ceti_f,
            tile: None
        })
        .is_err()
    );
    w.apply_now(Command::DeclareWar {
        empire: terran,
        target: cetian,
    })
    .unwrap();
    assert!(w.at_war(cetian, terran));
    w.apply_now(Command::Bombard {
        fleet: home,
        planet: ceti_f,
        tile: None,
    })
    .unwrap();

    // Sol -> Alpha Centauri -> Barnard's Star -> Tau Ceti, or via Sirius: two jumps either way.
    let mut fought = false;
    for _ in 0..80 {
        w.tick();
        let events = w.drain_events();
        fought |= events.iter().any(|e| e.kind == EventKind::Battle);
        if w.planet(ceti_f).owner == Some(terran) {
            break;
        }
    }
    assert!(fought, "the fleets should have met");
    assert!(
        w.fleets.iter().all(|f| f.owner != cetian),
        "the Cetian fleet should be destroyed"
    );
    // Ceti Forge is barren, so it surrendered as soon as it was bombarded.
    assert_eq!(w.planet(ceti_f).owner, Some(terran));
    // The system still belongs to the Concord while it holds Ceti Prime.
    let tau = w.system_by_name("tau-ceti").unwrap();
    assert_eq!(w.system(tau).owner, Some(cetian));

    // Bombarding the populated capital wrecks buildings and eventually forces a surrender.
    w.apply_now(Command::Bombard {
        fleet: home,
        planet: ceti_prime,
        tile: None,
    })
    .unwrap();
    let mut destroyed = false;
    for _ in 0..200 {
        w.tick();
        destroyed |= w
            .drain_events()
            .iter()
            .any(|e| e.kind == EventKind::BuildingDestroyed);
        if w.planet(ceti_prime).owner == Some(terran) {
            break;
        }
    }
    assert!(destroyed);
    assert_eq!(w.planet(ceti_prime).owner, Some(terran));
}

#[test]
fn peace_stops_bombardment_and_fleets_follow_routes() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let cetian = w.empire_by_name("empire-2").unwrap();
    let home = w.fleets.iter().find(|f| f.owner == terran).unwrap().id;
    let sirius = w.system_by_name("sirius").unwrap();
    w.apply_now(Command::MoveFleet {
        fleet: home,
        system: sirius,
        coordinates: Some([0.0, 0.0]),
    })
    .unwrap();
    w.run(40);
    let f = w.fleet(home).unwrap();
    assert_eq!(f.system, sirius);
    assert!(
        f.coordinates[0].hypot(f.coordinates[1]) < 1.0,
        "fleet should reach the star"
    );

    let ceti_prime = w.planet_by_name("tau-ceti/tau-ceti-e").unwrap();
    w.apply_now(Command::DeclareWar {
        empire: terran,
        target: cetian,
    })
    .unwrap();
    w.apply_now(Command::Bombard {
        fleet: home,
        planet: ceti_prime,
        tile: None,
    })
    .unwrap();
    w.apply_now(Command::MakePeace {
        empire: cetian,
        target: terran,
    })
    .unwrap();
    assert_eq!(
        w.fleet(home).unwrap().order,
        spacebox_core::world::FleetOrder::Idle
    );
}

#[test]
fn fleets_and_claims_survive_a_save_round_trip() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let cetian = w.empire_by_name("empire-2").unwrap();
    let outpost = w.defs.find_building("solar-array").unwrap();
    let barnard_b = w.planet_by_name("barnards-star/barnard-b").unwrap();
    let earth = w.planet_by_name("sol/sol-3").unwrap();
    let corvette = w.defs.find_ship("corvette").unwrap();
    w.apply_now(Command::Build {
        planet: barnard_b,
        tile: 0,
        building: outpost,
        empire: Some(terran),
    })
    .unwrap();
    w.apply_now(Command::BuildShip {
        planet: earth,
        ship: corvette,
        fleet: None,
    })
    .unwrap();
    w.apply_now(Command::DeclareWar {
        empire: terran,
        target: cetian,
    })
    .unwrap();
    let home = w.fleets.iter().find(|f| f.owner == terran).unwrap().id;
    let sirius = w.system_by_name("sirius").unwrap();
    w.apply_now(Command::MoveFleet {
        fleet: home,
        system: sirius,
        coordinates: None,
    })
    .unwrap();
    w.run(3);

    let dir = temp_dir("fleets");
    let path = dir.join("out.toml");
    save::save(&w, &path).unwrap();
    let (back, warnings) = load_world(&path).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(back.at_war(terran, cetian));
    assert_eq!(back.fleets.len(), w.fleets.len());
    for (a, b) in w.fleets.iter().zip(&back.fleets) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.system, b.system);
        assert_eq!(a.order, b.order);
        assert_eq!(a.ships.len(), b.ships.len());
        assert!((a.coordinates[0] - b.coordinates[0]).abs() < 1e-3);
    }
    assert_eq!(back.empire(terran).ship_queue.len(), 1);
    match &back.planet(barnard_b).tiles[0].construction {
        Some(spacebox_core::world::Construction::Building { empire, paid, .. }) => {
            assert_eq!(*empire, Some(terran));
            assert!(!paid.is_empty());
        }
        other => panic!("claim site lost: {other:?}"),
    }
    for (a, b) in w.planets.iter().zip(&back.planets) {
        assert_eq!(a.owner, b.owner);
        assert_eq!(a.coordinates, b.coordinates);
    }
}

// ---------------------------------------------------------------------------------------------
// Blockers and terrain
// ---------------------------------------------------------------------------------------------

/// First tile on a planet matching a predicate.
fn find_tile(
    w: &World,
    planet: spacebox_core::PlanetId,
    f: impl Fn(&spacebox_core::world::Tile) -> bool,
) -> usize {
    w.planet(planet)
        .tiles
        .iter()
        .position(f)
        .expect("no matching tile")
}

#[test]
fn blockers_must_be_cleared_at_their_remove_cost() {
    let mut w = quiet_sol();
    let earth = w.planet_by_name("sol/sol-3").unwrap();
    let terran = w.empire_by_name("empire-1").unwrap();
    let energy = w.defs.find_resource("energy").unwrap();
    let wetlands = w.defs.find_building("wetlands").unwrap();
    let farm = w.defs.find_building("farm").unwrap();
    assert!(w.defs.building(wetlands).blocker);
    assert!(
        !w.defs.building(wetlands).buildable,
        "blockers are never constructed"
    );

    let tile = find_tile(&w, earth, |t| {
        t.building.as_ref().is_some_and(|b| b.def == wetlands)
    });
    let err = w
        .apply_now(Command::Build {
            planet: earth,
            tile,
            building: farm,
            empire: None,
        })
        .unwrap_err();
    assert!(err.contains("blocked by Wetlands"), "{err}");

    // Clearing costs the wetlands' 1000 energy.
    let before = w.empire(terran).storage[energy.idx()];
    w.apply_now(Command::Demolish {
        planet: earth,
        tile,
    })
    .unwrap();
    assert_eq!(w.empire(terran).storage[energy.idx()], before - 1000.0);
    assert!(w.planet(earth).tiles[tile].building.is_none());
    w.apply_now(Command::Build {
        planet: earth,
        tile,
        building: farm,
        empire: None,
    })
    .unwrap();

    // Without the energy, the next blocker stays put.
    w.apply_now(Command::GrantResources {
        empire: terran,
        resource: energy,
        count: -1e9,
    })
    .unwrap();
    let other = find_tile(&w, earth, |t| {
        t.building
            .as_ref()
            .is_some_and(|b| w.defs.building(b.def).blocker)
    });
    let err = w
        .apply_now(Command::Demolish {
            planet: earth,
            tile: other,
        })
        .unwrap_err();
    assert!(err.contains("cannot afford"), "{err}");

    // Nobody can clear blockers in an unowned system.
    let barnard = w.planet_by_name("barnards-star/barnard-b").unwrap();
    let rock = find_tile(&w, barnard, |t| t.building.is_some());
    assert!(
        w.apply_now(Command::Demolish {
            planet: barnard,
            tile: rock
        })
        .unwrap_err()
        .contains("claim it")
    );
}

#[test]
fn land_and_water_buildings_need_matching_tiles() {
    let w = quiet_sol();
    let earth = w.planet_by_name("sol/sol-3").unwrap();
    let terran = w.empire_by_name("empire-1").unwrap();
    let farm = w.defs.find_building("farm").unwrap();
    let aqua = w.defs.find_building("aquaculture-farm").unwrap();
    let sea = find_tile(&w, earth, |t| t.water && t.is_free());
    let land = find_tile(&w, earth, |t| !t.water && t.is_free());
    assert!(
        w.build_plan(terran, earth, sea, farm)
            .unwrap_err()
            .contains("on land")
    );
    assert!(
        w.build_plan(terran, earth, land, aqua)
            .unwrap_err()
            .contains("water tile")
    );
    w.build_plan(terran, earth, sea, aqua).unwrap();
    w.build_plan(terran, earth, land, farm).unwrap();
}

#[test]
fn space_buildings_cannot_be_placed_yet() {
    let dir = temp_dir("space");
    let d = data_dir();
    std::fs::write(
        dir.join("mod.toml"),
        "[building.ring-station]\ndisplay_name = \"Ring Station\"\nbuilding_class = [\"space\"]\n",
    )
    .unwrap();
    let path = dir.join("scenario.toml");
    std::fs::write(
        &path,
        format!(
            "[import]\nrules = \"{0}/rules.toml\"\nstation = \"mod.toml\"\n\n\
             [empire.e]\ndisplay_name = \"E\"\n\n\
             [system.s]\ndisplay_name = \"S\"\ncoordinates = {{ x = 0, y = 0 }}\nowned_by = \"e\"\n\
             [system.s.planets.p]\ndisplay_name = \"P\"\n",
            d.display()
        ),
    )
    .unwrap();
    let (w, _) = load_world(&path).unwrap();
    let station = w.defs.find_building("ring-station").unwrap();
    let err = w
        .build_plan(
            spacebox_core::EmpireId(0),
            spacebox_core::PlanetId(0),
            0,
            station,
        )
        .unwrap_err();
    assert!(err.contains("orbital megastructure"), "{err}");
}

#[test]
fn generated_planets_have_seas_and_blockers() {
    let (defs, _) = spacebox_core::load_assets(data_dir().join("base.toml")).unwrap();
    let params = spacebox_core::generate::GalaxyParams {
        seed: 4,
        systems: 400,
        empires: 0,
        ai: false,
    };
    let w = spacebox_core::generate::generate(defs, &params).unwrap();
    let (mut tiles, mut water, mut blocked) = (0usize, 0usize, 0usize);
    for p in &w.planets {
        let is_blocker = |t: &spacebox_core::world::Tile| {
            t.building
                .as_ref()
                .is_some_and(|b| w.defs.building(b.def).blocker)
        };
        if p.habitable {
            tiles += p.tiles.len();
            water += p.tiles.iter().filter(|t| t.water).count();
            blocked += p.tiles.iter().filter(|t| is_blocker(t)).count();
            for t in p.tiles.iter().filter(|t| is_blocker(t)) {
                let def = w.defs.building(t.building.as_ref().unwrap().def);
                assert!(
                    !def.barren && def.water == t.water,
                    "{} on the wrong terrain",
                    def.display_name
                );
            }
        } else {
            assert!(p.tiles.iter().all(|t| !t.water));
            let open = p.tiles.iter().filter(|t| !is_blocker(t)).count();
            assert!(open <= 2, "{} has {open} open tiles", p.display_name);
            assert!(
                p.tiles
                    .iter()
                    .filter(|t| is_blocker(t))
                    .all(|t| w.defs.building(t.building.as_ref().unwrap().def).barren)
            );
        }
    }
    let (wf, bf) = (water as f64 / tiles as f64, blocked as f64 / tiles as f64);
    assert!((0.25..0.42).contains(&wf), "water fraction {wf}");
    assert!((0.25..0.42).contains(&bf), "blocker fraction {bf}");
}

#[test]
fn ai_clears_blockers_on_crowded_worlds() {
    let mut w = sol();
    w.run(720);
    let cleared = w
        .drain_events()
        .iter()
        .any(|e| e.message.contains(" clears the "));
    assert!(
        cleared,
        "AI empires should clear blockers once their worlds fill up"
    );
}

// ---------------------------------------------------------------------------------------------
// Format strictness and AI personalities
// ---------------------------------------------------------------------------------------------

#[test]
fn only_standard_toml_is_accepted() {
    // Multi-line inline tables are TOML 1.1, not TOML 1.0.
    let dir = temp_dir("strict-toml");
    std::fs::write(
        dir.join("x.toml"),
        "[resource.a]\ndisplay_name = \"A\"\n\n[building.b]\ndisplay_name = \"B\"\n\
         upkeep_resources = [\n    {\n        resource = \"a\",\n        count = 1,\n    },\n]\n",
    )
    .unwrap();
    assert!(load_err(&dir.join("x.toml")).contains("inline table"));
}

#[test]
fn ai_tables_set_personalities() {
    let w = sol();
    let terran = w.empire(w.empire_by_name("empire-1").unwrap()).ai.unwrap();
    assert_eq!(terran.militant, 0.6);
    assert_eq!(terran.expansionist, 0.7);

    let dir = temp_dir("ai-table");
    let body = |ai: &str| {
        format!(
            "[empire.e]\ndisplay_name = \"E\"\n{ai}\n[system.s]\ndisplay_name = \"S\"\n\
             coordinates = {{ x = 0, y = 0 }}\nowned_by = \"e\"\n"
        )
    };
    let (w, _) = load_world(scenario(&dir, &body(""))).unwrap();
    assert!(w.empires[0].ai.is_none(), "no table means a player empire");
    let (w, _) = load_world(scenario(&dir, &body("[empire.e.ai]\nmilitant = 0.9\n"))).unwrap();
    let ai = w.empires[0].ai.unwrap();
    assert_eq!(
        (ai.militant, ai.expansionist, ai.scientific),
        (0.9, 0.5, 0.5)
    );
    let err = load_err(&scenario(&dir, &body("[empire.e.ai]\nmilitant = 1.5\n")));
    assert!(err.contains("between 0 and 1"), "{err}");
}

fn warlike_galaxy(militant: f64) -> World {
    let (defs, _) = spacebox_core::load_assets(data_dir().join("base.toml")).unwrap();
    let params = spacebox_core::generate::GalaxyParams {
        seed: 11,
        systems: 60,
        empires: 4,
        ai: true,
    };
    let mut w = spacebox_core::generate::generate(defs, &params).unwrap();
    for e in &mut w.empires {
        e.ai.as_mut().unwrap().militant = militant;
    }
    w
}

#[test]
fn militant_ai_empires_go_to_war_and_fight() {
    let mut w = warlike_galaxy(0.9);
    let mut kinds = std::collections::HashSet::new();
    for _ in 0..900 {
        w.tick();
        kinds.extend(w.drain_events().into_iter().map(|e| e.kind));
    }
    for k in [
        EventKind::WarDeclared,
        EventKind::Battle,
        EventKind::Bombardment,
        EventKind::Surrendered,
    ] {
        assert!(kinds.contains(&k), "expected a {k:?} event; saw {kinds:?}");
    }
}

#[test]
fn peaceful_ai_empires_never_start_wars() {
    let mut w = warlike_galaxy(0.0);
    w.run(600);
    assert!(
        w.drain_events()
            .iter()
            .all(|e| e.kind != EventKind::WarDeclared)
    );
}

#[test]
fn peace_brings_a_truce() {
    let mut w = quiet_sol();
    let (a, b) = (
        w.empire_by_name("empire-1").unwrap(),
        w.empire_by_name("empire-2").unwrap(),
    );
    w.run(10);
    w.apply_now(Command::DeclareWar {
        empire: a,
        target: b,
    })
    .unwrap();
    assert_eq!(w.empire(a).wars[0].since_day, 10);
    w.apply_now(Command::MakePeace {
        empire: b,
        target: a,
    })
    .unwrap();
    let truce = w.rules().truce_days;
    assert!(w.empire(a).in_truce(b, 11) && w.empire(b).in_truce(a, 11));
    assert!(!w.empire(a).in_truce(b, 10 + truce));
}

#[test]
fn ai_fleets_finish_conquering_a_system() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let cetian = w.empire_by_name("empire-2").unwrap();
    // A peaceful AI so the governor itself doesn't redirect the fleet.
    w.empires[terran.idx()].ai = Some(spacebox_core::world::AiPersonality {
        militant: 0.0,
        expansionist: 0.0,
        scientific: 0.0,
    });
    let home = w.fleets.iter().find(|f| f.owner == terran).unwrap().id;
    let ceti_f = w.planet_by_name("tau-ceti/tau-ceti-f").unwrap();
    w.apply_now(Command::DeclareWar {
        empire: terran,
        target: cetian,
    })
    .unwrap();
    w.apply_now(Command::Bombard {
        fleet: home,
        planet: ceti_f,
        tile: None,
    })
    .unwrap();

    let tau = w.system_by_name("tau-ceti").unwrap();
    for _ in 0..400 {
        w.tick();
        if w.system(tau).owner == Some(terran) {
            break;
        }
    }
    assert_eq!(
        w.system(tau).owner,
        Some(terran),
        "the AI should take the whole system"
    );
    assert!(
        w.system(tau)
            .planet_ids()
            .all(|p| w.planet(p).owner == Some(terran))
    );
}

#[test]
fn player_fleets_stand_down_after_a_surrender() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let cetian = w.empire_by_name("empire-2").unwrap();
    let home = w.fleets.iter().find(|f| f.owner == terran).unwrap().id;
    let ceti_f = w.planet_by_name("tau-ceti/tau-ceti-f").unwrap();
    w.apply_now(Command::DeclareWar {
        empire: terran,
        target: cetian,
    })
    .unwrap();
    w.apply_now(Command::Bombard {
        fleet: home,
        planet: ceti_f,
        tile: None,
    })
    .unwrap();
    for _ in 0..80 {
        w.tick();
        if w.planet(ceti_f).owner == Some(terran) {
            break;
        }
    }
    assert_eq!(w.planet(ceti_f).owner, Some(terran));
    assert_eq!(
        w.fleet(home).unwrap().order,
        spacebox_core::world::FleetOrder::Idle
    );
}
