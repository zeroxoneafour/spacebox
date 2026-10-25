//! End-to-end tests against the shipped data files, the spec's example files and small
//! hand-made worlds.

use std::path::{Path, PathBuf};

use spacebox_core::defs::Kind;
use spacebox_core::sim::planet::building_profile;
use spacebox_core::world::{FleetOrder, ProjectKind};
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
         technologies = \"{0}/technologies.toml\"\nrules = \"{0}/rules.toml\"\n\
         species = \"{0}/species.toml\"\n\n{body}",
        d.display()
    );
    let path = dir.join("scenario.toml");
    std::fs::write(&path, text).unwrap();
    path
}

fn sol() -> World {
    load_world(data_dir().join("scenarios/sol.toml")).unwrap().0
}

/// Sol without the AI governors.
fn quiet_sol() -> World {
    let mut w = sol();
    w.empires.iter_mut().for_each(|e| e.ai = None);
    w
}

fn load_err(path: &Path) -> String {
    load_world(path).unwrap_err().to_string()
}

fn res(w: &World, name: &str) -> spacebox_core::ResourceId {
    w.defs.find_resource(name).unwrap()
}

/// Sets an empire's stock of a resource.
fn set_stock(w: &mut World, empire: spacebox_core::EmpireId, name: &str, count: f64) {
    let r = res(w, name);
    w.empires[empire.idx()].storage[r.idx()] = count;
}

#[test]
fn loads_the_sol_scenario() {
    let w = sol();
    assert_eq!(w.empires.len(), 2);
    assert_eq!(w.systems.len(), 5);
    let earth = w.planet(w.planet_by_name("sol/sol-3").unwrap());
    assert_eq!(earth.display_name, "Earth");
    assert_eq!(earth.pop(), 800);
    let human = w.defs.find_species("human").unwrap();
    assert_eq!(earth.species_count(human), 800);
    let forge = earth.tiles[0].building.as_ref().unwrap();
    assert_eq!(w.defs.building(forge.def).key.name, "alloy-forge");
    assert_eq!(w.defs.upgrade(forge.upgrades[0]).key.name, "faster-forge");
    assert_eq!(forge.workers, 100);
    assert_eq!(earth.tiles[0].resource_mults.len(), 2);
    let terran = w.empire(w.empire_by_name("empire-1").unwrap());
    assert_eq!(terran.dominant_species, Some(human));
    // Links are symmetric even though only one side may list them.
    let sol = w.system_by_name("sol").unwrap();
    let ac = w.system_by_name("alpha-centauri").unwrap();
    assert!(w.system(ac).links.contains(&sol));
}

/// The example save in `design/data/` loads as written, runs, and survives a save round trip.
#[test]
fn loads_the_spec_examples() {
    let spec = Path::new(env!("CARGO_MANIFEST_DIR")).join("../design/data/savegame.toml");
    let (mut w, warnings) = load_world(&spec).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(w.day, 10);
    assert_eq!(w.seed, 8902341);
    let e = &w.empires[0];
    assert_eq!(e.research.len(), 1);
    assert_eq!(e.shipbuilding.len(), 1);
    assert_eq!(e.shipbuilding[0].paid[0].count, 155.0);
    assert!(e.dominant_species.is_some());
    assert!(e.accepts_migrants && e.market_access);
    let fleet = &w.fleets[0];
    assert_eq!(fleet.ships.len(), 1);
    assert_eq!(fleet.orders.len(), 2);
    assert!(!fleet.in_combat);
    let planet = w.planet(w.planet_by_name("example-system/example-planet").unwrap());
    assert_eq!(planet.pop(), 1000);
    let c = planet.tiles[2].construction.as_ref().unwrap();
    assert!(matches!(c.kind, ProjectKind::Upgrade(_)));
    assert_eq!(c.remaining.days, 50);
    assert_eq!(c.paid[0].count, 750.0);
    let removal = planet.tiles[3].construction.as_ref().unwrap();
    assert_eq!(removal.kind, ProjectKind::Remove);
    assert!(removal.paid.is_empty());
    let tile1 = planet.tiles[1].building.as_ref().unwrap();
    assert!(planet.tiles[1].water && tile1.pinned && tile1.damage == 0.3);
    // The water building and its amenities, the blocker with a remove cost.
    let water = w.defs.find_building("example-water-building").unwrap();
    assert_eq!(w.defs.building(water).outputs.amenities, 100.0);
    assert!(w.defs.building(water).water && !w.defs.building(water).land);
    let barren = w.defs.find_building("example-barren-building").unwrap();
    assert!(w.defs.building(barren).barren && w.defs.building(barren).land);
    let blocker = w.defs.find_building("example-blocker-building").unwrap();
    assert_eq!(
        w.defs.building(blocker).remove_cost.as_ref().unwrap().days,
        150
    );
    // The upgrade applies to its building and, through the tag, the barren building.
    let up = w.defs.find_upgrade("example-building-upgrade").unwrap();
    assert!(w.defs.building(barren).available_upgrades.contains(&up));
    // Technology effects.
    let t2 = w
        .defs
        .tech(w.defs.find_tech("example-technology-2").unwrap());
    assert_eq!(t2.fleet_size_add, 10);
    assert_eq!(t2.pop_growth_mult, 1.1);
    assert_eq!(t2.ships.fire_rate_mult, 1.1);
    assert_eq!(t2.buildings[0].targets.buildings.len(), 1);
    assert_eq!(t2.buildings[0].targets.tags.len(), 1);
    w.run(30);
    let dir = temp_dir("spec");
    save::save(&w, dir.join("out.toml")).unwrap();
    let (back, warnings) = load_world(dir.join("out.toml")).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(back.day, 40);
}

#[test]
fn imports_do_not_recurse() {
    let dir = temp_dir("imports");
    std::fs::write(
        dir.join("c.toml"),
        "[resource.ore]\ndisplay-name = \"Ore\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b.toml"),
        "[import]\nc = \"c.toml\"\n\n[job.digger]\ndisplay-name = \"Digger\"\nappeal = 1\n\
         associated-resource = [\"c.ore\"]\n",
    )
    .unwrap();
    // `a` imports `b` only, so it cannot see `c`'s resources even though they are loaded.
    std::fs::write(
        dir.join("a.toml"),
        "[import]\nb = \"b.toml\"\n\n[building.pit]\ndisplay-name = \"Pit\"\nmax-workers = 1\n\
         job = \"b.digger\"\noutputs = [{ resource = \"c.ore\", count = 1 }]\n",
    )
    .unwrap();
    let err = load_err(&dir.join("a.toml"));
    assert!(err.contains("unknown import alias 'c'"), "{err}");

    // Importing `c` directly fixes it.
    std::fs::write(
        dir.join("a.toml"),
        "[import]\nb = \"b.toml\"\nc = \"c.toml\"\n\n[building.pit]\ndisplay-name = \"Pit\"\n\
         max-workers = 1\njob = \"b.digger\"\noutputs = [{ resource = \"c.ore\", count = 1 }]\n",
    )
    .unwrap();
    let (w, _) = load_world(dir.join("a.toml")).unwrap();
    assert_eq!(
        w.defs.buildings[0].outputs.resources[0].resource,
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
            "[import]\njobs = \"{}/jobs.toml\"\n\n[building.x]\ndisplay-name = \"X\"\n\
             outputs = [{{ resource = \"jobs.farmer\", count = 1 }}]\n",
            d.display()
        ),
    )
    .unwrap();
    let err = load_err(&dir.join("x.toml"));
    assert!(err.contains("unknown resource 'jobs.farmer'"), "{err}");

    // Upgrades live in [building-upgrade.*], so a building slot doesn't find them.
    let path = scenario(
        &dir,
        "[empire.e]\ndisplay-name = \"E\"\n\n[system.s]\ndisplay-name = \"S\"\n\
         star-type = \"yellow\"\ncoords = [0, 0]\n[system.s.planets.p]\ndisplay-name = \"P\"\n\
         size = \"large\"\nhabitable = true\ncoords = [10, 0]\n\
         [system.s.planets.p.tile-0]\nbuilding = \"buildings.faster-forge\"\n",
    );
    let err = load_err(&path);
    assert!(
        err.contains("unknown building 'buildings.faster-forge'"),
        "{err}"
    );
}

#[test]
fn import_must_come_first_and_fields_are_checked() {
    let dir = temp_dir("strict");
    std::fs::write(
        dir.join("x.toml"),
        "[resource.a]\ndisplay-name = \"A\"\n\n[import]\nb = \"b.toml\"\n",
    )
    .unwrap();
    assert!(load_err(&dir.join("x.toml")).contains("[import] must be the first section"));

    std::fs::write(
        dir.join("y.toml"),
        "[resource.a]\ndisplay-name = \"A\"\ncolour = 1\n",
    )
    .unwrap();
    assert!(load_err(&dir.join("y.toml")).contains("colour"));

    // Keys use hyphens, never underscores.
    std::fs::write(dir.join("u.toml"), "[resource.a]\ndisplay_name = \"A\"\n").unwrap();
    assert!(load_err(&dir.join("u.toml")).contains("display_name"));

    std::fs::write(dir.join("z.toml"), "[resorce.a]\ndisplay-name = \"A\"\n").unwrap();
    assert!(load_err(&dir.join("z.toml")).contains("unknown top-level table [resorce]"));
}

#[test]
fn cost_and_output_lists_take_days_and_amenities() {
    let w = sol();
    let farm = w.defs.building(w.defs.find_building("farm").unwrap());
    assert_eq!(farm.build_cost.days, 60);
    assert_eq!(farm.build_cost.resources.len(), 1);
    let office = w
        .defs
        .building(w.defs.find_building("administrative-office").unwrap());
    assert_eq!(office.outputs.amenities, 40.0);
    // Land is implicit; water buildings only stand on water.
    assert!(farm.land && !farm.water);
    let aqua = w
        .defs
        .building(w.defs.find_building("aquaculture-farm").unwrap());
    assert!(aqua.water && !aqua.land);

    let dir = temp_dir("lists");
    std::fs::write(
        dir.join("x.toml"),
        "[resource.a]\ndisplay-name = \"A\"\n\n[building.b]\ndisplay-name = \"B\"\n\
         build-cost = [{ days = 1.5 }]\n",
    )
    .unwrap();
    assert!(load_err(&dir.join("x.toml")).contains("whole number"));
    std::fs::write(
        dir.join("x.toml"),
        "[resource.a]\ndisplay-name = \"A\"\n\n[building.b]\ndisplay-name = \"B\"\n\
         outputs = [{ days = 3 }]\n",
    )
    .unwrap();
    assert!(load_err(&dir.join("x.toml")).contains("amenities"));
}

#[test]
fn adjacency_and_upgrades_shape_profiles() {
    let w = sol();
    let earth = w.planet(w.planet_by_name("sol/sol-3").unwrap());
    let owner = w
        .planet_owner(w.planet_by_name("sol/sol-3").unwrap())
        .map(|e| w.empire(e));
    let minerals = res(&w, "minerals");
    let alloy = res(&w, "alloy");
    let energy = res(&w, "energy");
    let forge = building_profile(&w.defs, earth, 0, owner).unwrap();
    // Inputs: base 20 minerals, ×1.25 (faster-forge), ×0.95 (the mine on adjacent tile 2).
    let input = forge.inputs.iter().find(|(r, _)| *r == minerals).unwrap().1;
    assert!((input - 20.0 * 1.25 * 0.95).abs() < 1e-9, "{input}");
    // Output ×1.25 (upgrade) ×1.1 (tile multiplier).
    let output = forge.outputs.iter().find(|(r, _)| *r == alloy).unwrap().1;
    assert!((output - 6.0 * 1.25 * 1.1).abs() < 1e-9, "{output}");
    // Upkeep ×1.1 (upgrade) ×0.9 (adjacent power plant on tile 5).
    let upkeep = forge.upkeep.iter().find(|(r, _)| *r == energy).unwrap().1;
    assert!((upkeep - 5.0 * 1.1 * 0.9).abs() < 1e-9, "{upkeep}");
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
    // Upgrades are only available where applies-to matches.
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
    let tile = w.planet(mars).free_tiles().next().unwrap();
    assert!(
        w.apply_now(Command::Build {
            planet: mars,
            tile,
            building: farm,
            empire: None,
        })
        .unwrap_err()
        .contains("barren")
    );
    w.apply_now(Command::Build {
        planet: mars,
        tile,
        building: outpost,
        empire: None,
    })
    .unwrap();
    assert!(w.planet(mars).tiles[tile].construction.is_some());
}

#[test]
fn terraforming_requires_technology_then_makes_planets_habitable() {
    let mut w = quiet_sol();
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
        set_stock(&mut w, terran, name, n);
    }
    w.apply_now(Command::Terraform { planet: venus }).unwrap();
    w.run(1);
    // Costs come out of the stockpile as the project runs.
    assert!(w.empire(terran).storage[res(&w, "exotic-gases").idx()] < 500.0);
    w.run(360);
    assert!(w.planet(venus).habitable);
    assert!(w.planet(venus).terraform.is_none());
    assert!(
        w.drain_events()
            .iter()
            .any(|e| e.kind == EventKind::Terraformed)
    );
}

#[test]
fn constructions_draw_from_the_stockpile_and_refund_when_cancelled() {
    let dir = temp_dir("funding");
    let path = scenario(
        &dir,
        r#"
[empire.e]
display-name = "Broke"

[system.s]
display-name = "S"
star-type = "yellow"
coords = [0, 0]
owner = "e"
[system.s.planets.p]
display-name = "P"
size = "large"
habitable = true
coords = [20, 0]
owner = "e"
"#,
    );
    let (mut w, _) = load_world(&path).unwrap();
    let e = spacebox_core::EmpireId(0);
    let p = spacebox_core::PlanetId(0);
    let farm = w.defs.find_building("farm").unwrap();
    let minerals = res(&w, "minerals");
    // Without minerals (and no income) the project starts but cannot finish.
    w.apply_now(Command::Build {
        planet: p,
        tile: 0,
        building: farm,
        empire: None,
    })
    .unwrap();
    w.run(70);
    let c = w.planet(p).tiles[0].construction.as_ref().unwrap();
    assert_eq!(c.remaining.days, 0);
    assert_eq!(c.remaining.get(minerals), 100.0);

    // 60 minerals go towards it; cancelling refunds them.
    set_stock(&mut w, e, "minerals", 60.0);
    w.tick();
    let c = w.planet(p).tiles[0].construction.as_ref().unwrap();
    assert!((c.remaining.get(minerals) - 40.0).abs() < 1e-9);
    assert_eq!(w.empire(e).storage[minerals.idx()], 0.0);
    w.apply_now(Command::Demolish { planet: p, tile: 0 })
        .unwrap();
    assert!(w.planet(p).tiles[0].construction.is_none());
    assert!((w.empire(e).storage[minerals.idx()] - 60.0).abs() < 1e-9);

    // Fully funded, it completes once its 60 days are up.
    set_stock(&mut w, e, "minerals", 1000.0);
    w.apply_now(Command::Build {
        planet: p,
        tile: 0,
        building: farm,
        empire: None,
    })
    .unwrap();
    w.run(59);
    assert!(w.planet(p).tiles[0].building.is_none());
    w.run(1);
    assert_eq!(w.planet(p).tiles[0].building.as_ref().unwrap().def, farm);
    assert_eq!(w.empire(e).storage[minerals.idx()], 900.0);
}

#[test]
fn transient_resources_fund_projects_or_go_to_waste() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let research = res(&w, "research");
    assert!(w.defs.resource(research).transient);
    w.tick();
    let e = w.empire(terran);
    assert!(e.income[research.idx()] > 0.0);
    assert_eq!(
        e.storage[research.idx()],
        0.0,
        "transient resources are never stored"
    );
    assert!(
        (e.wasted[research.idx()] - e.income[research.idx()]).abs() < 1e-9,
        "with nothing to research, all of it is wasted"
    );

    // Two projects at once: the first gets what it needs, the second the rest.
    let grid = w.defs.find_tech("grid-optimization").unwrap();
    let holo = w.defs.find_tech("holographic-entertainment").unwrap();
    w.apply_now(Command::Research {
        empire: terran,
        tech: grid,
    })
    .unwrap();
    w.apply_now(Command::Research {
        empire: terran,
        tech: holo,
    })
    .unwrap();
    assert!(
        w.apply_now(Command::Research {
            empire: terran,
            tech: grid
        })
        .unwrap_err()
        .contains("already being researched")
    );
    // Research can queue behind prerequisites that are being researched...
    let quantum = w.defs.find_tech("quantum-computing").unwrap();
    w.apply_now(Command::Research {
        empire: terran,
        tech: quantum,
    })
    .unwrap();
    // ...but not behind ones nobody is working on.
    let fusion = w.defs.find_tech("fusion-power").unwrap();
    assert!(
        w.apply_now(Command::Research {
            empire: terran,
            tech: fusion
        })
        .unwrap_err()
        .contains("first needs")
    );
    w.tick();
    let e = w.empire(terran);
    assert_eq!(e.wasted[research.idx()], 0.0);
    let grid_left = e.research[0].remaining.get(research);
    assert!(grid_left < 500.0);
    assert_eq!(e.research[1].remaining.get(research), 600.0);
    assert_eq!(
        e.research[2].remaining.get(research),
        1600.0,
        "waits for its prerequisite"
    );
    for _ in 0..400 {
        w.tick();
        if w.empire(terran).knows(holo) {
            break;
        }
    }
    let e = w.empire(terran);
    assert!(e.knows(grid) && e.knows(holo));
    // Quantum Computing only started once Grid Optimization was known.
    assert_eq!(e.research.len(), 1);
    assert_eq!(e.research[0].tech, quantum);
    assert!(e.research[0].remaining.get(research) < 1600.0);
    // Cancelling a prerequisite cancels what waits on it.
    let crops = w.defs.find_tech("gene-tailored-crops").unwrap();
    let therapy = w.defs.find_tech("gene-therapy").unwrap();
    for tech in [crops, therapy] {
        w.apply_now(Command::Research {
            empire: terran,
            tech,
        })
        .unwrap();
    }
    w.apply_now(Command::CancelResearch {
        empire: terran,
        tech: crops,
    })
    .unwrap();
    assert!(!w.empire(terran).is_researching(therapy));
    assert!(w.empire(terran).is_researching(quantum));
    let events = w.drain_events();
    assert!(events.iter().any(|e| e.kind == EventKind::ResearchFinished));
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
        assert_eq!(a.population, b.population, "{}", a.name);
        assert_eq!(a.habitable, b.habitable);
        for (ta, tb) in a.tiles.iter().zip(&b.tiles) {
            assert_eq!(
                ta.building.as_ref().map(|x| (x.def, x.upgrades.clone())),
                tb.building.as_ref().map(|x| (x.def, x.upgrades.clone()))
            );
            assert_eq!(
                ta.construction.as_ref().map(|c| (c.kind, c.remaining.days)),
                tb.construction.as_ref().map(|c| (c.kind, c.remaining.days))
            );
            assert_eq!(ta.water, tb.water);
        }
    }
    for (a, b) in w.empires.iter().zip(&back.empires) {
        assert_eq!(a.techs, b.techs);
        assert_eq!(a.research.len(), b.research.len());
        for (x, y) in a.storage.iter().zip(&b.storage) {
            assert!((x - y).abs() < 1e-3);
        }
    }
    // Spec format: kebab-case keys and tile-N tables. The mod's building survives.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(".tile-0]"));
    assert!(text.contains("display-name = "));
    assert!(!text.contains("display_name"));
    assert!(text.contains("orbital-agriculture.orbital-farm"));
}

#[test]
fn simulation_is_deterministic() {
    let run = || {
        let mut w = sol();
        w.run(300);
        let pops: Vec<_> = w.planets.iter().map(|p| p.population.clone()).collect();
        let stores: Vec<Vec<f64>> = w.empires.iter().map(|e| e.storage.clone()).collect();
        (pops, stores, w.drain_events().len())
    };
    assert_eq!(run(), run());
}

#[test]
fn species_eat_different_things() {
    let dir = temp_dir("species");
    let path = scenario(
        &dir,
        r#"
[empire.e]
display-name = "Mixed"
resource-stores = [
    { resource = "resources.minerals", count = 100000 },
    { resource = "resources.consumer-goods", count = 100000 },
    { resource = "resources.energy", count = 100000 },
]

[system.s]
display-name = "S"
star-type = "yellow"
coords = [10, 0]
owner = "e"
[system.s.planets.p]
display-name = "P"
size = "large"
habitable = true
coords = [20, 0]
owner = "e"
population = [
    { species = "species.human", count = 200 },
    { species = "species.lithoid", count = 200 },
]
"#,
    );
    let (mut w, _) = load_world(&path).unwrap();
    let human = w.defs.find_species("human").unwrap();
    let lithoid = w.defs.find_species("lithoid").unwrap();
    w.run(60);
    let p = w.planet(spacebox_core::PlanetId(0));
    // No food at all: humans starve, lithoids (who eat minerals) do not.
    assert!(p.species_count(human) < 200, "{:?}", p.population);
    assert!(p.species_count(lithoid) >= 200, "{:?}", p.population);
    let events = w.drain_events();
    assert!(events.iter().any(|e| e.kind == EventKind::Starvation));
    assert!(events.iter().any(|e| e.kind == EventKind::CrisisStarted));
}

#[test]
fn unemployed_pops_migrate_to_open_jobs() {
    let dir = temp_dir("migrate");
    let path = scenario(
        &dir,
        r#"
[empire.e]
display-name = "Movers"
resource-stores = [
    { resource = "resources.food", count = 100000 },
    { resource = "resources.consumer-goods", count = 100000 },
    { resource = "resources.energy", count = 100000 },
]

[system.a]
display-name = "A"
star-type = "yellow"
coords = [20, 0]
owner = "e"
linked-systems = ["b"]
[system.a.planets.crowded]
display-name = "Crowded"
size = "large"
habitable = true
coords = [10, 0]
owner = "e"
population = [ { species = "species.human", count = 100 }, { species = "species.hive", count = 50 } ]

[system.b]
display-name = "B"
star-type = "yellow"
coords = [30, 0]
owner = "e"
[system.b.planets.frontier]
display-name = "Frontier"
size = "large"
habitable = true
coords = [10, 0]
owner = "e"
[system.b.planets.frontier.tile-0]
building = "buildings.farm"
[system.b.planets.frontier.tile-1]
building = "buildings.housing"
"#,
    );
    let (mut w, _) = load_world(&path).unwrap();
    w.run(90);
    let frontier = w.planet(w.planet_by_name("frontier").unwrap());
    assert!(frontier.pop() >= 90, "frontier pop {}", frontier.pop());
    // Both species moved.
    assert_eq!(frontier.population.len(), 2, "{:?}", frontier.population);
    assert!(
        w.drain_events()
            .iter()
            .any(|e| e.kind == EventKind::Settled)
    );
}

#[test]
fn pinned_workers_override_appeal() {
    let mut w = quiet_sol();
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
display-name = "Rich"
resource-stores = [
    { resource = "resources.food", count = 50000 },
    { resource = "resources.consumer-goods", count = 50000 },
    { resource = "resources.energy", count = 1000 },
]
[empire.poor]
display-name = "Poor"
resource-stores = [
    { resource = "resources.consumer-goods", count = 50000 },
    { resource = "resources.energy", count = 50000 },
]

[system.r]
display-name = "R"
star-type = "yellow"
coords = [40, 0]
owner = "rich"
[system.r.planets.r1]
display-name = "R1"
size = "large"
habitable = true
coords = [10, 0]
owner = "rich"
population = [ { species = "species.human", count = 10 } ]

[system.p]
display-name = "P"
star-type = "yellow"
coords = [50, 0]
owner = "poor"
[system.p.planets.p1]
display-name = "P1"
size = "large"
habitable = true
coords = [10, 0]
owner = "poor"
population = [ { species = "species.human", count = 200 } ]
"#,
    );
    let (mut w, _) = load_world(&path).unwrap();
    w.run(5);
    let food = res(&w, "food");
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
    let mut w = quiet_sol();
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
    // Empires start with the ruleset's base stores and a dominant species.
    let minerals = w.defs.find_resource("minerals").unwrap();
    assert!(
        w.empires
            .iter()
            .all(|e| e.storage[minerals.idx()] == 1500.0)
    );
    assert!(w.empires.iter().all(|e| e.dominant_species.is_some()));
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
        .pop();

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
            .pop(),
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
    assert!(earth.pop() > 0 && earth.tiles[0].building.is_some());
}

// ---------------------------------------------------------------------------------------------
// Expansion, ships and fleets
// ---------------------------------------------------------------------------------------------

#[test]
fn expansion_costs_influence_and_time_by_distance() {
    let w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let influence = res(&w, "influence");
    let outpost = w.defs.find_building("solar-array").unwrap();
    let base_days = w.defs.building(outpost).build_cost.days;
    // Barnard's Star is adjacent to Terran space (n = 0): 100 influence, +30 days.
    let barnard = w.planet_by_name("barnards-star/barnard-b").unwrap();
    let tile = w.planet(barnard).free_tiles().next().unwrap();
    let plan = w.build_plan(terran, barnard, tile, outpost).unwrap();
    assert_eq!(plan.expansion_jumps, Some(1));
    assert_eq!(plan.cost.get(influence), 100.0);
    assert_eq!(plan.cost.days, base_days + 30);
    // Owned systems cost no influence; a system owned by a rival cannot be built in at all.
    let mars = w.planet_by_name("sol/sol-4").unwrap();
    let mars_tile = w.planet(mars).free_tiles().next().unwrap();
    assert!(
        w.build_plan(terran, mars, mars_tile, outpost)
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
    let minerals = res(&w, "minerals");
    let tile_b = w.planet(barnard_b).free_tiles().next().unwrap();
    let tile_c = w.planet(barnard_c).free_tiles().next().unwrap();
    w.apply_now(Command::Build {
        planet: barnard_b,
        tile: tile_b,
        building: outpost,
        empire: Some(terran),
    })
    .unwrap();
    w.run(5);
    w.apply_now(Command::Build {
        planet: barnard_c,
        tile: tile_c,
        building: outpost,
        empire: Some(cetian),
    })
    .unwrap();
    let before = w.empire(cetian).storage[minerals.idx()];
    w.tick();
    // The rival's site draws its 100 minerals from the stockpile...
    let paid = before - w.empire(cetian).storage[minerals.idx()];
    assert!(paid > 0.0);
    // Run until Terra's claim completes, then check the rival's refund: the materials it paid,
    // but not its influence.
    let influence = res(&w, "influence");
    let mut refunded = false;
    for _ in 0..150 {
        let c = w.planet(barnard_c).tiles[tile_c].construction.clone();
        let (m0, i0) = (
            w.empire(cetian).storage[minerals.idx()],
            w.empire(cetian).storage[influence.idx()],
        );
        w.tick();
        if let Some(c) = c
            && w.planet(barnard_c).tiles[tile_c].construction.is_none()
        {
            let paid_minerals: f64 = c
                .paid
                .iter()
                .filter(|a| a.resource == minerals)
                .map(|a| a.count)
                .sum();
            let paid_influence: f64 = c
                .paid
                .iter()
                .filter(|a| a.resource == influence)
                .map(|a| a.count)
                .sum();
            assert!(paid_influence > 0.0);
            let gained = w.empire(cetian).storage[minerals.idx()] - m0;
            assert!(gained >= paid_minerals - 1.0, "{gained} < {paid_minerals}");
            assert!(w.empire(cetian).storage[influence.idx()] - i0 < paid_influence);
            refunded = true;
            break;
        }
    }
    assert!(refunded, "rival site cancelled");
    let sys = w.system_by_name("barnards-star").unwrap();
    assert_eq!(w.system(sys).owner, Some(terran));
    // Only the planet built on is claimed; the system's other planets stay unowned, and
    // nobody else can claim them now.
    assert_eq!(w.planet(barnard_b).owner, Some(terran));
    assert_eq!(w.planet(barnard_c).owner, None);
    let err = w
        .build_plan(cetian, barnard_c, tile_c, outpost)
        .unwrap_err();
    assert!(
        err.contains("you cannot build in a system you do not own"),
        "{err}"
    );
    w.build_plan(terran, barnard_c, tile_c, outpost).unwrap();
    let events = w.drain_events();
    assert!(events.iter().any(|e| e.kind == EventKind::SystemClaimed));
    assert!(events.iter().any(|e| e.kind == EventKind::ClaimLost));
}

#[test]
fn influence_accrues_daily_up_to_its_cap() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let influence = res(&w, "influence");
    let start = w.empire(terran).storage[influence.idx()];
    w.run(10);
    assert!((w.empire(terran).storage[influence.idx()] - (start + 50.0)).abs() < 1e-6);
    w.run(400);
    assert_eq!(w.empire(terran).storage[influence.idx()], 1000.0);
}

fn ship_count(w: &World, empire: spacebox_core::EmpireId) -> usize {
    w.fleets
        .iter()
        .filter(|f| f.owner == empire)
        .map(|f| f.ships.len())
        .sum()
}

#[test]
fn shipyards_feed_ships_under_construction() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let earth = w.planet_by_name("sol/sol-3").unwrap();
    let mars = w.planet_by_name("sol/sol-4").unwrap();
    let corvette = w.defs.find_ship("corvette").unwrap();
    let shipbuilding = res(&w, "shipbuilding");
    let before = ship_count(&w, terran);
    w.run(2);
    // Shipyards always work; with nothing to build their output is wasted.
    let e = w.empire(terran);
    assert!(e.income[shipbuilding.idx()] > 0.0);
    assert_eq!(e.wasted[shipbuilding.idx()], e.income[shipbuilding.idx()]);

    // Ships can be started at any owned planet, but only that planet's shipbuilding goes into
    // them: Mars has no shipyard, so its corvette pays its alloy and then waits.
    w.apply_now(Command::BuildShip {
        planet: mars,
        ship: corvette,
    })
    .unwrap();
    let barnard = w.planet_by_name("barnards-star/barnard-b").unwrap();
    assert!(
        w.apply_now(Command::BuildShip {
            planet: barnard,
            ship: corvette
        })
        .is_err(),
        "only at a planet of one's own"
    );
    let destroyer = w.defs.find_ship("destroyer").unwrap();
    assert!(
        w.apply_now(Command::BuildShip {
            planet: earth,
            ship: destroyer
        })
        .unwrap_err()
        .contains("Naval Doctrine")
    );
    w.apply_now(Command::BuildShip {
        planet: earth,
        ship: corvette,
    })
    .unwrap();
    w.tick();
    let e = w.empire(terran);
    assert_eq!(e.wasted[shipbuilding.idx()], 0.0);
    let alloy = res(&w, "alloy");
    assert_eq!(
        e.shipbuilding[0].paid[0].resource, alloy,
        "alloy paid up front"
    );
    w.run(30);
    assert_eq!(ship_count(&w, terran), before + 1);
    let e = w.empire(terran);
    assert_eq!(e.shipbuilding.len(), 1);
    assert_eq!(e.shipbuilding[0].planet, mars);
    assert!(e.shipbuilding[0].remaining.get(shipbuilding) > 0.0);
    let launched: Vec<_> = w
        .drain_events()
        .into_iter()
        .filter(|e| e.kind == EventKind::ShipLaunched)
        .collect();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].planet, Some(earth));
    // A new ship forms a fleet of its own.
    let sol = w.system_by_name("sol").unwrap();
    assert!(
        w.fleets
            .iter()
            .any(|f| f.owner == terran && f.system == sol && f.ships.len() == 1)
    );
    w.apply_now(Command::CancelShip {
        empire: terran,
        index: 0,
    })
    .unwrap();

    // Cancelling refunds the alloy already paid (but not the shipbuilding).
    set_stock(&mut w, terran, "alloy", 30.0);
    w.apply_now(Command::BuildShip {
        planet: earth,
        ship: corvette,
    })
    .unwrap();
    w.tick();
    assert_eq!(w.empire(terran).storage[alloy.idx()], 0.0);
    let spent = w.empire(terran).shipbuilding[0].paid[0].count;
    assert!(spent >= 30.0);
    w.apply_now(Command::CancelShip {
        empire: terran,
        index: 0,
    })
    .unwrap();
    assert!((w.empire(terran).storage[alloy.idx()] - spent).abs() < 1e-9);
}

#[test]
fn ship_stats_combine_hull_weapons_outfits_and_technology() {
    let mut w = quiet_sol();
    let fighter_id = w.defs.find_ship("fighter").unwrap();
    let fighter = w.defs.ship(fighter_id);
    let stats = &fighter.base_stats;
    // 350 shield + 50 from the booster; regen 10 x 1.5; speed 10 x 1.3 (afterburner).
    assert_eq!(stats.max_shield, 400.0);
    assert_eq!(stats.shield_regen, 15.0);
    assert!((stats.speed - 13.0).abs() < 1e-9);
    assert_eq!(stats.armament.len(), 2);
    let alloy = res(&w, "alloy");
    assert_eq!(
        fighter.build_cost.get(alloy),
        50.0 + 10.0 * 2.0 + 25.0 + 15.0
    );
    let cooldown = stats.armament[0].cooldown;

    // Plasma Weapons divides the delay between shots by 1.1 for its empire only.
    let terran = w.empire_by_name("empire-1").unwrap();
    let plasma = w.defs.find_tech("plasma-weapons").unwrap();
    w.empires[terran.idx()].techs[plasma.idx()] = true;
    let defs = w.defs.clone();
    w.empires[terran.idx()].rebuild_modifiers(&defs);
    let boosted = w.ship_stats(terran, fighter_id).armament[0].cooldown;
    assert!((boosted - cooldown / 1.1).abs() < 1e-9);
    let cetian = w.empire_by_name("empire-2").unwrap();
    assert_eq!(
        w.ship_stats(cetian, fighter_id).armament[0].cooldown,
        cooldown
    );
}

#[test]
fn fleets_have_a_size_limit() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let limit = w.empire(terran).max_fleet_size(w.rules());
    assert_eq!(limit, 20);
    let home = w.fleets.iter().find(|f| f.owner == terran).unwrap().id;
    w.apply_now(Command::SplitFleet {
        fleet: home,
        ships: vec![0, 1, 2],
    })
    .unwrap();
    let detachment = w
        .fleets
        .iter()
        .find(|f| f.owner == terran && f.id != home)
        .unwrap()
        .id;
    // Destroyers take two slots each: 1 + 9 × 2 = 19, plus the detachment's 3, is over 20.
    let destroyer = w.defs.find_ship("destroyer").unwrap();
    let ship = spacebox_core::world::Ship::new(destroyer, w.ship_stats(terran, destroyer));
    w.fleet_mut(home).unwrap().ships.extend(vec![ship; 9]);
    assert_eq!(w.fleet(home).unwrap().size(&w.defs), 19);
    let err = w
        .apply_now(Command::MergeFleets {
            into: home,
            from: detachment,
        })
        .unwrap_err();
    assert!(err.contains("at most 20 slots"), "{err}");
    // Naval Doctrine raises the limit by 5.
    let doctrine = w.defs.find_tech("naval-doctrine").unwrap();
    w.empires[terran.idx()].techs[doctrine.idx()] = true;
    let defs = w.defs.clone();
    w.empires[terran.idx()].rebuild_modifiers(&defs);
    w.apply_now(Command::MergeFleets {
        into: home,
        from: detachment,
    })
    .unwrap();
    assert_eq!(w.fleet(home).unwrap().size(&w.defs), 22);
}

#[test]
fn fleets_jump_from_the_edge_facing_their_destination() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let home = w.fleets.iter().find(|f| f.owner == terran).unwrap().id;
    let sol = w.system_by_name("sol").unwrap();
    let sirius = w.system_by_name("sirius").unwrap();
    w.apply_now(Command::MoveFleet {
        fleet: home,
        system: sirius,
        coordinates: Some([0.0, 0.0]),
    })
    .unwrap();
    let radius = w.rules().arrival_radius;
    let (a, b) = (w.system(sol).coordinates, w.system(sirius).coordinates);
    let len = (b[0] - a[0]).hypot(b[1] - a[1]);
    let edge = [(b[0] - a[0]) / len * radius, (b[1] - a[1]) / len * radius];
    let mut charged_at_edge = false;
    for _ in 0..60 {
        w.tick();
        let f = w.fleet(home).unwrap();
        if f.system == sol && f.jump_progress() > 0.0 {
            let d = (f.coordinates[0] - edge[0]).hypot(f.coordinates[1] - edge[1]);
            assert!(d < 1.0, "charging away from the edge ({d} units)");
            charged_at_edge = true;
        }
    }
    assert!(charged_at_edge);
    let f = w.fleet(home).unwrap();
    assert_eq!(f.system, sirius);
    assert!(
        f.coordinates[0].hypot(f.coordinates[1]) < 1.0,
        "fleet should reach the star"
    );
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

    let mut fought = false;
    for _ in 0..120 {
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
    for _ in 0..300 {
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
fn peace_stops_bombardment() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let cetian = w.empire_by_name("empire-2").unwrap();
    let home = w.fleets.iter().find(|f| f.owner == terran).unwrap().id;
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
    // The bombardment order is gone; the jumps towards Tau Ceti remain.
    let f = w.fleet(home).unwrap();
    assert!(!f.orders.is_empty());
    assert!(
        f.orders
            .iter()
            .all(|o| matches!(o, FleetOrder::Jump { .. }))
    );
}

#[test]
fn fleets_and_projects_survive_a_save_round_trip() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let cetian = w.empire_by_name("empire-2").unwrap();
    let outpost = w.defs.find_building("solar-array").unwrap();
    let barnard_b = w.planet_by_name("barnards-star/barnard-b").unwrap();
    let tile = w.planet(barnard_b).free_tiles().next().unwrap();
    let earth = w.planet_by_name("sol/sol-3").unwrap();
    // A destroyer takes a while to build.
    let destroyer = w.defs.find_ship("destroyer").unwrap();
    let doctrine = w.defs.find_tech("naval-doctrine").unwrap();
    w.empires[terran.idx()].techs[doctrine.idx()] = true;
    let defs = w.defs.clone();
    w.empires[terran.idx()].rebuild_modifiers(&defs);
    let grid = w.defs.find_tech("grid-optimization").unwrap();
    w.apply_now(Command::Build {
        planet: barnard_b,
        tile,
        building: outpost,
        empire: Some(terran),
    })
    .unwrap();
    w.apply_now(Command::BuildShip {
        planet: earth,
        ship: destroyer,
    })
    .unwrap();
    w.apply_now(Command::Research {
        empire: terran,
        tech: grid,
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
    for a in &w.fleets {
        let b = back
            .fleets
            .iter()
            .find(|b| b.owner == a.owner && b.name == a.name)
            .unwrap();
        assert_eq!(a.system, b.system);
        assert_eq!(a.orders, b.orders);
        assert_eq!(a.ships.len(), b.ships.len());
        for i in 0..a.ships.len() {
            let (pa, pb) = (a.ship_coordinates(i), b.ship_coordinates(i));
            assert!((pa[0] - pb[0]).abs() < 1e-3 && (pa[1] - pb[1]).abs() < 1e-3);
        }
    }
    let (ea, eb) = (w.empire(terran), back.empire(terran));
    assert_eq!(eb.shipbuilding.len(), 1);
    assert_eq!(eb.shipbuilding[0].planet, earth);
    let same = |a: &spacebox_core::defs::Cost, b: &spacebox_core::defs::Cost| {
        a.days == b.days
            && a.resources
                .iter()
                .chain(&b.resources)
                .all(|x| (a.get(x.resource) - b.get(x.resource)).abs() < 1e-3)
    };
    assert!(same(
        &eb.shipbuilding[0].remaining,
        &ea.shipbuilding[0].remaining
    ));
    assert!(same(&eb.research[0].remaining, &ea.research[0].remaining));
    let c = back.planet(barnard_b).tiles[tile]
        .construction
        .as_ref()
        .unwrap();
    assert_eq!(c.empire, Some(terran));
    assert_eq!(c.kind, ProjectKind::Building(outpost));
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
fn blockers_are_cleared_by_removal_projects() {
    let mut w = quiet_sol();
    let earth = w.planet_by_name("sol/sol-3").unwrap();
    let terran = w.empire_by_name("empire-1").unwrap();
    let energy = res(&w, "energy");
    let wetlands = w.defs.find_building("wetlands").unwrap();
    let farm = w.defs.find_building("farm").unwrap();
    assert!(w.defs.building(wetlands).blocker);

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
    assert!(
        w.apply_now(Command::Build {
            planet: earth,
            tile: find_tile(&w, earth, |t| t.is_free() && !t.water),
            building: wetlands,
            empire: None,
        })
        .unwrap_err()
        .contains("occurs naturally")
    );

    // Clearing the wetlands is a project: 1000 energy and 150 days.
    w.apply_now(Command::Demolish {
        planet: earth,
        tile,
    })
    .unwrap();
    let c = w.planet(earth).tiles[tile].construction.as_ref().unwrap();
    assert_eq!(c.kind, ProjectKind::Remove);
    assert_eq!(c.remaining.days, 150);
    let before = w.empire(terran).storage[energy.idx()];
    w.tick();
    assert!(w.empire(terran).storage[energy.idx()] < before);
    w.run(150);
    assert!(w.planet(earth).tiles[tile].building.is_none());
    assert!(w.planet(earth).tiles[tile].construction.is_none());
    w.apply_now(Command::Build {
        planet: earth,
        tile,
        building: farm,
        empire: None,
    })
    .unwrap();

    // Buildings without a remove-cost are removed at once, for free.
    let housing = find_tile(&w, earth, |t| {
        t.building
            .as_ref()
            .is_some_and(|b| w.defs.building(b.def).key.name == "housing")
    });
    w.apply_now(Command::Demolish {
        planet: earth,
        tile: housing,
    })
    .unwrap();
    assert!(w.planet(earth).tiles[housing].building.is_none());

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
            .contains("cannot be built on water")
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
        "[building.ring-station]\ndisplay-name = \"Ring Station\"\nbuilding-tags = [\"space\"]\n",
    )
    .unwrap();
    let path = dir.join("scenario.toml");
    std::fs::write(
        &path,
        format!(
            "[import]\nrules = \"{0}/rules.toml\"\nstation = \"mod.toml\"\n\n\
             [empire.e]\ndisplay-name = \"E\"\n\n\
             [system.s]\ndisplay-name = \"S\"\nstar-type = \"yellow\"\ncoords = [0, 0]\n\
             owner = \"e\"\n[system.s.planets.p]\ndisplay-name = \"P\"\nsize = \"large\"\n\
             habitable = true\ncoords = [10, 0]\nowner = \"e\"\n",
            d.display()
        ),
    )
    .unwrap();
    let (w, _) = load_world(&path).unwrap();
    let station = w.defs.find_building("ring-station").unwrap();
    assert!(
        !w.defs.building(station).land,
        "space buildings are not implicitly land"
    );
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
                    !def.barren && def.fits_terrain(t.water),
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
fn ai_clears_blockers_and_uses_its_transient_output() {
    let mut w = sol();
    w.run(720);
    let events = w.drain_events();
    assert!(
        events.iter().any(|e| e.message.contains(" cleared on ")),
        "AI empires should clear blockers once their worlds fill up"
    );
    assert!(events.iter().any(|e| e.kind == EventKind::ResearchFinished));
    assert!(events.iter().any(|e| e.kind == EventKind::ShipLaunched));
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
        "[resource.a]\ndisplay-name = \"A\"\n\n[building.b]\ndisplay-name = \"B\"\n\
         upkeep-cost = [\n    {\n        resource = \"a\",\n        count = 1,\n    },\n]\n",
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
            "[empire.e]\ndisplay-name = \"E\"\n{ai}\n[system.s]\ndisplay-name = \"S\"\n\
             star-type = \"yellow\"\ncoords = [0, 0]\nowner = \"e\"\n"
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
    for _ in 0..500 {
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
    for _ in 0..120 {
        w.tick();
        if w.planet(ceti_f).owner == Some(terran) {
            break;
        }
    }
    assert_eq!(w.planet(ceti_f).owner, Some(terran));
    assert!(w.fleet(home).unwrap().is_idle());
}

#[test]
fn pops_need_housing() {
    let mut w = quiet_sol();
    let earth = w.planet_by_name("sol/sol-3").unwrap();
    let proxima = w.planet_by_name("alpha-centauri/proxima-b").unwrap();
    // Proxima b has jobs but no housing: nobody can be moved there.
    assert_eq!(w.housing(proxima), 0.0);
    let err = w
        .apply_now(Command::MovePops {
            from: earth,
            to: proxima,
            count: 50,
        })
        .unwrap_err();
    assert!(err.contains("no free housing"), "{err}");
    // Placing housing (500) makes room for exactly that many.
    let housing = w.defs.find_building("housing").unwrap();
    let tile = w
        .planet(proxima)
        .free_tiles()
        .find(|t| !w.planet(proxima).tiles[*t].water)
        .unwrap();
    w.planet_mut(proxima).tiles[tile].building =
        Some(spacebox_core::world::BuildingInstance::new(housing));
    w.planet_mut(proxima).invalidate();
    w.apply_now(Command::MovePops {
        from: earth,
        to: proxima,
        count: 600,
    })
    .unwrap();
    assert_eq!(w.planet(proxima).pop(), 500);
}

#[test]
fn fleets_work_through_their_order_queue() {
    let mut w = quiet_sol();
    let terran = w.empire_by_name("empire-1").unwrap();
    let cetian = w.empire_by_name("empire-2").unwrap();
    let home = w.fleets.iter().find(|f| f.owner == terran).unwrap().id;
    let sol = w.system_by_name("sol").unwrap();
    let sirius = w.system_by_name("sirius").unwrap();
    let tau = w.system_by_name("tau-ceti").unwrap();
    // A jump must follow a hyperlane from where the fleet will be.
    let err = w
        .apply_now(Command::SetOrders {
            fleet: home,
            orders: vec![FleetOrder::Jump { system: tau }],
            append: false,
        })
        .unwrap_err();
    assert!(err.contains("no hyperlane"), "{err}");
    let orders = vec![
        FleetOrder::Move {
            coordinates: [0.0, 30.0],
        },
        FleetOrder::Jump { system: sirius },
        FleetOrder::Move {
            coordinates: [5.0, 5.0],
        },
    ];
    w.apply_now(Command::SetOrders {
        fleet: home,
        orders,
        append: false,
    })
    .unwrap();
    let mut moved_first = false;
    for _ in 0..80 {
        w.tick();
        let f = w.fleet(home).unwrap();
        if f.system == sol && f.orders.len() == 2 {
            moved_first = true;
        }
        if f.is_idle() {
            break;
        }
    }
    assert!(moved_first, "the in-system move came first");
    let f = w.fleet(home).unwrap();
    assert!(f.is_idle());
    assert_eq!(f.system, sirius);
    assert_eq!(f.coordinates, [5.0, 5.0]);
    assert!(
        f.ships.iter().all(|s| s.jump_charge == 0.0),
        "charge resets after a jump"
    );

    // Combat suspends orders: the Cetian fleet comes to Sirius while the Terran fleet has a
    // move order there, and the order waits until the battle is over.
    w.apply_now(Command::DeclareWar {
        empire: terran,
        target: cetian,
    })
    .unwrap();
    let vanguard = w.fleets.iter().find(|f| f.owner == cetian).unwrap().id;
    w.apply_now(Command::MoveFleet {
        fleet: vanguard,
        system: sirius,
        coordinates: None,
    })
    .unwrap();
    for _ in 0..80 {
        w.tick();
        if w.fleet(vanguard).is_some_and(|f| f.system == sirius) {
            break;
        }
    }
    // Arrivals happen at the end of the space phase; the battle starts the next day.
    w.tick();
    assert!(w.fleet(home).unwrap().in_combat);
    w.apply_now(Command::SetOrders {
        fleet: home,
        orders: vec![FleetOrder::Move {
            coordinates: [-60.0, 0.0],
        }],
        append: false,
    })
    .unwrap();
    for _ in 0..80 {
        w.tick();
        let f = w.fleet(home).unwrap();
        if f.in_combat {
            assert_eq!(f.orders.len(), 1, "orders wait during combat");
        }
        if w.fleet(vanguard).is_none() && f.is_idle() {
            break;
        }
    }
    // After the battle the fleet carried out its order.
    let f = w.fleet(home).unwrap();
    assert!(w.fleet(vanguard).is_none() && !f.in_combat);
    assert_eq!(f.coordinates, [-60.0, 0.0]);
}

#[test]
fn tags_are_scoped_to_their_file() {
    let w = sol();
    let defs = &w.defs;
    // The mod's orbital farm carries the base game's agriculture tag, so Gene-Tailored Crops
    // (which targets "buildings.agriculture") boosts it too.
    let orbital = defs.find_building("orbital-farm").unwrap();
    let farm = defs.find_building("farm").unwrap();
    let ag = defs.building(farm).tags[0];
    assert_eq!(defs.tag_name(ag), "agriculture");
    assert!(defs.building(orbital).has_tag(ag));
    let crops = defs.tech(defs.find_tech("gene-tailored-crops").unwrap());
    assert!(defs.targets(&crops.buildings[0].targets, orbital));
    // Reserved tags are global.
    let barren = defs.reserved_tag("barren").unwrap();
    assert!(defs.building(orbital).has_tag(barren));

    // A tag of the same name in another file is a different tag.
    let dir = temp_dir("tags");
    let d = data_dir();
    std::fs::write(
        dir.join("mod.toml"),
        format!(
            "[import]\nbase = \"{0}/buildings.toml\"\nresources = \"{0}/resources.toml\"\n\n\
             [building.greenhouse]\ndisplay-name = \"Greenhouse\"\nbuilding-tags = [\"agriculture\"]\n\n\
             [building-upgrade.misfiled]\ndisplay-name = \"Misfiled\"\nbuilding-tags = [\"forge\"]\n",
            d.display()
        ),
    )
    .unwrap();
    let (defs, warnings) = spacebox_core::load_assets(dir.join("mod.toml")).unwrap();
    let greenhouse = defs.find_building("greenhouse").unwrap();
    let farm = defs.find_building("farm").unwrap();
    assert_ne!(
        defs.building(greenhouse).tags[0],
        defs.building(farm).tags[0]
    );
    // `forge` here means this file's tag, which no building has.
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("no building has the tag 'forge'")),
        "{warnings:?}"
    );
}
