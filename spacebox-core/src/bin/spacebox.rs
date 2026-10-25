//! Command-line driver for spacebox-core.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use spacebox_core::generate::{GalaxyParams, generate};
use spacebox_core::geometry::PlanetSize;
use spacebox_core::world::ProjectKind;
use spacebox_core::{EmpireId, PlanetId, World, load_assets, load_world, save};

const USAGE: &str = "\
usage:
  spacebox check <file.toml>
      Load a save or asset file and report problems.
  spacebox run <save.toml> [--days N] [--out FILE] [--report N] [--quiet]
      Simulate N days (default 360), printing events and a summary every N days.
  spacebox generate <assets.toml> --out FILE [--systems N] [--empires N] [--seed S] [--no-ai]
      Create a random galaxy save using the given asset index file.
  spacebox info <save.toml> [--planet SYSTEM/PLANET]
      Show empires, or one planet's tiles in detail.
  spacebox tiles <small|large>
      Print the tile numbering and adjacency of a planet shape.";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(raw: Vec<String>) -> Self {
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut it = raw.into_iter().peekable();
        while let Some(a) = it.next() {
            if let Some(name) = a.strip_prefix("--") {
                let value = match it.peek() {
                    Some(v) if !v.starts_with("--") && !matches!(name, "quiet" | "no-ai") => {
                        it.next()
                    }
                    _ => None,
                };
                flags.push((name.to_string(), value));
            } else {
                positional.push(a);
            }
        }
        Args { positional, flags }
    }

    fn flag(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.as_deref())
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == name)
    }

    fn num<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T, String> {
        match self.flag(name) {
            None => Ok(default),
            Some(v) => v
                .parse()
                .map_err(|_| format!("--{name}: invalid number '{v}'")),
        }
    }
}

fn main() -> ExitCode {
    let mut raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    }
    let cmd = raw.remove(0);
    let args = Args::parse(raw);
    let result = match cmd.as_str() {
        "check" => check(&args),
        "run" => run(&args),
        "generate" => generate_cmd(&args),
        "info" => info(&args),
        "tiles" => tiles(&args),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command '{other}'\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn path_arg(args: &Args, i: usize) -> Result<PathBuf, String> {
    args.positional
        .get(i)
        .map(PathBuf::from)
        .ok_or_else(|| USAGE.to_string())
}

fn load(path: &PathBuf) -> Result<World, String> {
    let (world, warnings) = load_world(path).map_err(|e| e.to_string())?;
    for w in warnings {
        eprintln!("warning: {w}");
    }
    Ok(world)
}

fn check(args: &Args) -> Result<(), String> {
    let path = path_arg(args, 0)?;
    let world = load(&path)?;
    let d = &world.defs;
    println!(
        "ok: {} files, {} resources, {} jobs, {} buildings, {} upgrades, {} technologies, \
         {} ships, {} weapons, {} outfits, {} rulesets",
        d.files.len(),
        d.resources.len(),
        d.jobs.len(),
        d.buildings.len(),
        d.upgrades.len(),
        d.technologies.len(),
        d.ships.len(),
        d.weapons.len(),
        d.outfits.len(),
        d.rulesets.len()
    );
    println!(
        "state: day {}, {} empires, {} systems, {} planets, {} fleets",
        world.day,
        world.empires.len(),
        world.systems.len(),
        world.planets.len(),
        world.fleets.len()
    );
    Ok(())
}

fn run(args: &Args) -> Result<(), String> {
    let path = path_arg(args, 0)?;
    let days: u64 = args.num("days", 360)?;
    let report: u64 = args.num("report", 90)?;
    let quiet = args.has("quiet");
    let mut world = load(&path)?;
    world.refresh_topology();
    let start = Instant::now();
    for _ in 0..days {
        world.tick();
        for ev in world.drain_events() {
            if !quiet {
                println!("{ev}");
            }
        }
        if report > 0 && world.day % report == 0 {
            print_summary(&world);
        }
    }
    let elapsed = start.elapsed();
    println!(
        "simulated {days} days of {} planets in {:.2?} ({:.2?}/day)",
        world.planets.len(),
        elapsed,
        elapsed / days.max(1) as u32
    );
    if let Some(out) = args.flag("out") {
        save::save(&world, out).map_err(|e| e.to_string())?;
        println!("saved to {out}");
    }
    Ok(())
}

fn generate_cmd(args: &Args) -> Result<(), String> {
    let assets = path_arg(args, 0)?;
    let out = args.flag("out").ok_or("generate needs --out FILE")?;
    let (defs, warnings) = load_assets(&assets).map_err(|e| e.to_string())?;
    for w in warnings {
        eprintln!("warning: {w}");
    }
    let params = GalaxyParams {
        seed: args.num("seed", 1)?,
        systems: args.num("systems", 100)?,
        empires: args.num("empires", 4)?,
        ai: !args.has("no-ai"),
    };
    let world = generate(defs, &params).map_err(|e| e.to_string())?;
    save::save(&world, out).map_err(|e| e.to_string())?;
    println!(
        "generated {} systems, {} planets, {} empires -> {out}",
        world.systems.len(),
        world.planets.len(),
        world.empires.len()
    );
    Ok(())
}

fn info(args: &Args) -> Result<(), String> {
    let path = path_arg(args, 0)?;
    let mut world = load(&path)?;
    world.refresh_topology();
    if let Some(name) = args.flag("planet") {
        let pid = world
            .planet_by_name(name)
            .ok_or_else(|| format!("no planet '{name}'"))?;
        // Derived stats only exist after a tick; simulate one in memory (nothing is written).
        world.tick();
        print_planet(&world, pid);
    } else {
        print_summary(&world);
    }
    Ok(())
}

fn tiles(args: &Args) -> Result<(), String> {
    let size = match args.positional.first().map(String::as_str) {
        Some("small") => PlanetSize::Small,
        Some("large") => PlanetSize::Large,
        _ => return Err("tiles needs 'small' or 'large'".into()),
    };
    let shape = size.shape();
    for t in 0..shape.tile_count() {
        let (lat, lon) = shape.lat_lon(t);
        println!(
            "tile {t:>2}  {}-gon  lat {lat:>6.1}  lon {lon:>6.1}  neighbours {:?}",
            shape.sides[t], shape.neighbors[t]
        );
    }
    Ok(())
}

fn short(world: &World, r: usize) -> String {
    world.defs.resources[r].display_name.clone()
}

fn print_summary(world: &World) {
    println!("=== day {} ===", world.day);
    for (i, e) in world.empires.iter().enumerate() {
        let eid = EmpireId::from_idx(i);
        println!(
            "{} [{}]: {} systems, {} planets, {} pops{}",
            e.display_name,
            e.name,
            world.empire_systems(eid).len(),
            e.stats.planets,
            e.stats.pop,
            e.dominant_species
                .map(|s| format!(" (mostly {})", world.defs.species_def(s).display_name))
                .unwrap_or_default()
        );
        let mut line = String::from("   ");
        let mut transient = String::from("   ");
        for (r, v) in e.storage.iter().enumerate() {
            if world.defs.resources[r].transient {
                transient.push_str(&format!(
                    " {} {:.1}/day ({:.1} wasted)",
                    short(world, r),
                    e.income[r],
                    e.wasted[r]
                ));
                continue;
            }
            line.push_str(&format!(
                " {} {:.0} ({:+.1})",
                short(world, r),
                v,
                e.income[r]
            ));
        }
        println!("{line}");
        println!("{transient}");
        if !e.research.is_empty() {
            let r: Vec<String> = e
                .research
                .iter()
                .map(|p| {
                    let total: f64 = world.defs.tech(p.tech).cost.iter().map(|a| a.count).sum();
                    let left: f64 = p.remaining.resources.iter().map(|a| a.count).sum();
                    format!(
                        "{} ({:.0}%)",
                        world.defs.tech(p.tech).display_name,
                        100.0 * (1.0 - left / total.max(1e-9))
                    )
                })
                .collect();
            println!("    researching: {}", r.join(", "));
        }
        if !e.crises.is_empty() {
            let c: Vec<String> = e
                .crises
                .iter()
                .map(|c| {
                    format!(
                        "{} {:.0}%",
                        short(world, c.resource.idx()),
                        c.severity * 100.0
                    )
                })
                .collect();
            println!("    CRISIS: {}", c.join(", "));
        }
        let known = e.techs.iter().filter(|k| **k).count();
        println!(
            "    {known}/{} technologies known",
            world.defs.technologies.len()
        );
        let fleets: Vec<String> = world
            .fleets
            .iter()
            .filter(|f| f.owner == eid)
            .map(|f| {
                format!(
                    "{} ({} ships @ {})",
                    f.display_name,
                    f.ships.len(),
                    world.system(f.system).display_name
                )
            })
            .collect();
        if !fleets.is_empty() {
            println!("    fleets: {}", fleets.join(", "));
        }
        if !e.shipbuilding.is_empty() {
            let q: Vec<&str> = e
                .shipbuilding
                .iter()
                .map(|o| world.defs.ship(o.ship).display_name.as_str())
                .collect();
            println!("    building: {}", q.join(", "));
        }
        if !e.wars.is_empty() {
            let w: Vec<String> = e
                .wars
                .iter()
                .map(|w| {
                    format!(
                        "{} (since day {})",
                        world.empire(w.enemy).display_name,
                        w.since_day
                    )
                })
                .collect();
            println!("    AT WAR with {}", w.join(", "));
        }
        if let Some(ai) = &e.ai {
            println!(
                "    AI: militant {:.2}, expansionist {:.2}, scientific {:.2}",
                ai.militant, ai.expansionist, ai.scientific
            );
        }
    }
    let traded: Vec<String> = world
        .market
        .iter()
        .enumerate()
        .filter(|(_, m)| m.volume > 0.0)
        .map(|(r, m)| format!("{} {:.1} @ {:.2}", short(world, r), m.volume, m.price))
        .collect();
    if !traded.is_empty() {
        println!("market: {}", traded.join(", "));
    }
}

fn print_planet(world: &World, pid: PlanetId) {
    let p = world.planet(pid);
    let sys = world.system(p.system);
    let species: Vec<String> = p
        .population
        .iter()
        .map(|(s, n)| format!("{n} {}", world.defs.species_def(*s).display_name))
        .collect();
    println!(
        "{} ({}) in {} - {} {}, {} pops{}",
        p.display_name,
        p.name,
        sys.display_name,
        p.size.as_str(),
        if p.habitable { "habitable" } else { "barren" },
        p.pop(),
        if species.is_empty() {
            String::new()
        } else {
            format!(" ({})", species.join(", "))
        }
    );
    let s = &p.stats;
    println!(
        "  housing {:.0}, jobs {} ({} filled, {} free), unemployed {}, homeless {}",
        s.housing, s.jobs, s.employed, s.free_jobs, s.unemployed, s.homeless
    );
    println!(
        "  amenities {:.0}/{:.0}, stability {:.1}",
        s.amenities, s.amenities_needed, s.stability
    );
    let mut flows = Vec::new();
    for r in 0..world.defs.resource_count() {
        let net = p.flows.supply[r] - p.flows.demand[r];
        if net.abs() > 0.005 {
            flows.push(format!("{} {net:+.1}", short(world, r)));
        }
    }
    println!("  net/day: {}", flows.join(", "));
    let shape = p.size.shape();
    for (i, t) in p.tiles.iter().enumerate() {
        let mut desc = match &t.building {
            Some(b) => {
                let d = world.defs.building(b.def);
                let mut s = d.display_name.clone();
                if let Some(max) = d.max_workers {
                    s.push_str(&format!(" [{}/{max} workers]", b.workers));
                }
                for u in &b.upgrades {
                    s.push_str(&format!(" +{}", world.defs.upgrade(*u).display_name));
                }
                s
            }
            None => "(empty)".into(),
        };
        if let Some(c) = &t.construction {
            let what = match c.kind {
                ProjectKind::Building(b) => {
                    format!("building {}", world.defs.building(b).display_name)
                }
                ProjectKind::Upgrade(u) => {
                    format!("installing {}", world.defs.upgrade(u).display_name)
                }
                ProjectKind::Remove => "removing".to_string(),
            };
            desc.push_str(&format!(
                " <{what}, {} left>",
                spacebox_core::sim::commands::describe_cost(world, &c.remaining)
            ));
        }
        for (r, m) in &t.resource_mults {
            desc.push_str(&format!(" {}×{m}", world.defs.resource(*r).display_name));
        }
        let terrain = if t.water { "~" } else { " " };
        println!(
            "  tile {i:>2} {terrain} {desc}  (adjacent {:?})",
            shape.neighbors(i)
        );
    }
}
