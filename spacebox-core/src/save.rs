//! Writes a [`World`] as a TOML save file in the same format the loader reads.
//!
//! The save imports every asset file it references (with paths relative to the save's own
//! location) and contains all game state inline: `[game]`, `[empire.*]`, `[system.*]`,
//! `[system.*.planets.*]` and the non-empty `[...tiles.N]` of each planet.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::defs::{DefKey, Defs};
use crate::error::{Error, Result};
use crate::ids::*;
use crate::world::{BuildingInstance, Construction, FleetOrder, World};

/// Writes the world to `path`.
pub fn save(world: &World, path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let text = save_to_string(world, path)?;
    fs::write(path, text).map_err(|e| Error::io(path, e))
}

/// Renders the save file that would be written to `path` (used for relative import paths).
pub fn save_to_string(world: &World, path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let dir = fs::canonicalize(dir).map_err(|e| Error::io(dir, e))?;
    let target = dir.join(path.file_name().unwrap_or_default());

    let mut w = Writer::new(&world.defs);
    w.collect(world);
    for f in &w.order {
        if world.defs.files[f.idx()].path == target {
            return Err(Error::invalid(
                &target,
                "refusing to overwrite a file whose asset definitions the save would import",
            ));
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "# Spacebox save, day {}", world.day);
    if !w.order.is_empty() {
        out.push_str("[import]\n");
        for f in &w.order {
            let rel = relative_path(&dir, &world.defs.files[f.idx()].path);
            let _ = writeln!(
                out,
                "{} = {}",
                w.aliases[f],
                s(&rel.to_string_lossy().replace('\\', "/"))
            );
        }
    }
    out.push('\n');
    w.write_state(world, &mut out);
    Ok(out)
}

struct Writer<'a> {
    defs: &'a Defs,
    aliases: HashMap<FileId, String>,
    order: Vec<FileId>,
}

/// TOML-escaped string literal.
fn s(v: &str) -> String {
    toml::Value::String(v.to_string()).to_string()
}

/// TOML float literal (always with a decimal point or exponent, never NaN/inf).
fn f(v: f64) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    // Round away float noise so saves stay readable and diff-friendly.
    let v = (v * 1e6).round() / 1e6;
    format!("{v:?}")
}

fn key(name: &str) -> String {
    if !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        name.to_string()
    } else {
        s(name)
    }
}

impl<'a> Writer<'a> {
    fn new(defs: &'a Defs) -> Self {
        Writer {
            defs,
            aliases: HashMap::new(),
            order: Vec::new(),
        }
    }

    fn use_file(&mut self, file: FileId) {
        if self.aliases.contains_key(&file) || file.0 == u32::MAX {
            return;
        }
        let stem = self.defs.files[file.idx()]
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "assets".into());
        let base: String = stem
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let mut alias = base.clone();
        let mut n = 2;
        while self.aliases.values().any(|a| *a == alias) {
            alias = format!("{base}-{n}");
            n += 1;
        }
        self.aliases.insert(file, alias);
        self.order.push(file);
    }

    fn collect(&mut self, world: &World) {
        let d = self.defs;
        let rules = d.ruleset(world.ruleset);
        if !rules.builtin {
            self.use_file(rules.key.file);
        }
        for e in &world.empires {
            for (r, v) in e.storage.iter().enumerate() {
                if *v != 0.0 {
                    self.use_file(d.resources[r].key.file);
                }
            }
            for (t, known) in e.techs.iter().enumerate() {
                if *known {
                    self.use_file(d.technologies[t].key.file);
                }
            }
            if let Some(t) = e.researching {
                self.use_file(d.tech(t).key.file);
            }
            for c in &e.crises {
                self.use_file(d.resource(c.resource).key.file);
            }
            for o in &e.ship_queue {
                self.use_file(d.ship(o.ship).key.file);
                for (r, _) in &o.remaining {
                    self.use_file(d.resource(*r).key.file);
                }
            }
        }
        for f in &world.fleets {
            for s in &f.ships {
                self.use_file(d.ship(s.def).key.file);
            }
        }
        for p in &world.planets {
            for t in &p.tiles {
                for (r, _) in &t.resource_mults {
                    self.use_file(d.resource(*r).key.file);
                }
                if let Some(b) = &t.building {
                    self.use_file(d.building(b.def).key.file);
                    for u in &b.upgrades {
                        self.use_file(d.upgrade(*u).key.file);
                    }
                }
                match &t.construction {
                    Some(Construction::Building { def, paid, .. }) => {
                        self.use_file(d.building(*def).key.file);
                        for a in paid {
                            self.use_file(d.resource(a.resource).key.file);
                        }
                    }
                    Some(Construction::Upgrade { upgrade, .. }) => {
                        self.use_file(d.upgrade(*upgrade).key.file);
                    }
                    None => {}
                }
            }
        }
    }

    fn r(&self, k: &DefKey) -> String {
        s(&format!("{}.{}", self.aliases[&k.file], k.name))
    }

    fn write_state(&self, world: &World, out: &mut String) {
        let d = self.defs;
        out.push_str("[game]\n");
        let _ = writeln!(out, "day = {}", world.day);
        let _ = writeln!(out, "seed = {}", world.seed);
        let rules = d.ruleset(world.ruleset);
        if !rules.builtin {
            let _ = writeln!(out, "ruleset = {}", self.r(&rules.key));
        }
        out.push('\n');

        for e in &world.empires {
            let _ = writeln!(out, "[empire.{}]", key(&e.name));
            let _ = writeln!(out, "display_name = {}", s(&e.display_name));
            if let Some(c) = &e.color {
                let _ = writeln!(out, "color = {}", s(c));
            }
            if let Some(c) = e.capital {
                let _ = writeln!(out, "capital = {}", s(&world.system(c).name));
            }
            if !e.accepts_migrants {
                out.push_str("accepts_migrants = false\n");
            }
            if !e.market_access {
                out.push_str("market_access = false\n");
            }
            let stores: Vec<String> = e
                .storage
                .iter()
                .enumerate()
                .filter(|(_, v)| **v != 0.0)
                .map(|(r, v)| {
                    format!(
                        "    {{ resource = {}, count = {} }},\n",
                        self.r(&d.resources[r].key),
                        f(*v)
                    )
                })
                .collect();
            if !stores.is_empty() {
                let _ = write!(out, "resource_stores = [\n{}]\n", stores.concat());
            }
            let techs: Vec<String> = e
                .techs
                .iter()
                .enumerate()
                .filter(|(_, k)| **k)
                .map(|(t, _)| format!("    {},\n", self.r(&d.technologies[t].key)))
                .collect();
            if !techs.is_empty() {
                let _ = write!(out, "technologies = [\n{}]\n", techs.concat());
            }
            if let Some(t) = e.researching {
                let _ = writeln!(out, "researching = {}", self.r(&d.tech(t).key));
                let _ = writeln!(out, "research_progress = {}", f(e.research_progress));
            }
            if !e.crises.is_empty() {
                out.push_str("crises = [\n");
                for c in &e.crises {
                    let _ = writeln!(
                        out,
                        "    {{ resource = {}, since_day = {}, severity = {} }},",
                        self.r(&d.resource(c.resource).key),
                        c.since_day,
                        f(c.severity)
                    );
                }
                out.push_str("]\n");
            }
            if !e.wars.is_empty() {
                out.push_str("wars = [\n");
                for w in &e.wars {
                    let _ = writeln!(
                        out,
                        "    {{ empire = {}, since_day = {} }},",
                        s(&world.empire(w.enemy).name),
                        w.since_day
                    );
                }
                out.push_str("]\n");
            }
            let truces: Vec<_> = e
                .truces
                .iter()
                .filter(|t| t.until_day > world.day)
                .collect();
            if !truces.is_empty() {
                out.push_str("truces = [\n");
                for t in truces {
                    let _ = writeln!(
                        out,
                        "    {{ empire = {}, until_day = {} }},",
                        s(&world.empire(t.with).name),
                        t.until_day
                    );
                }
                out.push_str("]\n");
            }
            if !e.ship_queue.is_empty() {
                out.push_str("ship_queue = [\n");
                for o in &e.ship_queue {
                    let remaining: Vec<String> = o
                        .remaining
                        .iter()
                        .map(|(r, c)| {
                            format!(
                                "{{ resource = {}, count = {} }}",
                                self.r(&d.resource(*r).key),
                                f(*c)
                            )
                        })
                        .collect();
                    let fleet = o
                        .fleet
                        .and_then(|id| world.fleet(id))
                        .map(|fl| format!(", fleet = {}", s(&fl.name)))
                        .unwrap_or_default();
                    let _ = writeln!(
                        out,
                        "    {{ ship = {}, planet = {}, remaining = [{}]{fleet} }},",
                        self.r(&d.ship(o.ship).key),
                        s(&planet_path(world, o.planet)),
                        remaining.join(", ")
                    );
                }
                out.push_str("]\n");
            }
            out.push('\n');

            if let Some(ai) = &e.ai {
                let _ = writeln!(out, "[empire.{}.ai]", key(&e.name));
                let _ = writeln!(out, "militant = {}", f(ai.militant));
                let _ = writeln!(out, "expansionist = {}", f(ai.expansionist));
                let _ = writeln!(out, "scientific = {}\n", f(ai.scientific));
            }

            let eid = world.empire_by_name(&e.name);
            for fl in world.fleets.iter().filter(|fl| Some(fl.owner) == eid) {
                let _ = writeln!(out, "[empire.{}.fleets.{}]", key(&e.name), key(&fl.name));
                let _ = writeln!(out, "display_name = {}", s(&fl.display_name));
                let _ = writeln!(out, "system = {}", s(&world.system(fl.system).name));
                let _ = writeln!(out, "coordinates = {}", coords(fl.coordinates));
                if fl.jump_charge > 0.0 {
                    let _ = writeln!(out, "jump_charge = {}", f(fl.jump_charge));
                }
                match &fl.order {
                    FleetOrder::Idle => {}
                    FleetOrder::Move {
                        system,
                        coordinates,
                    } => {
                        let c = coordinates
                            .map(|c| format!(", coordinates = {}", coords(c)))
                            .unwrap_or_default();
                        let _ = writeln!(
                            out,
                            "order = {{ type = \"move\", system = {}{c} }}",
                            s(&world.system(*system).name)
                        );
                    }
                    FleetOrder::Bombard { planet, tile } => {
                        let t = tile.map(|t| format!(", tile = {t}")).unwrap_or_default();
                        let _ = writeln!(
                            out,
                            "order = {{ type = \"bombard\", planet = {}{t} }}",
                            s(&planet_path(world, *planet))
                        );
                    }
                }
                out.push_str("ships = [\n");
                for sh in &fl.ships {
                    let _ = writeln!(
                        out,
                        "    {{ ship = {}, hull = {}, shield = {} }},",
                        self.r(&d.ship(sh.def).key),
                        f(sh.hull),
                        f(sh.shield)
                    );
                }
                out.push_str("]\n\n");
            }
        }

        for sys in &world.systems {
            let sk = key(&sys.name);
            let _ = writeln!(out, "[system.{sk}]");
            let _ = writeln!(out, "display_name = {}", s(&sys.display_name));
            let _ = writeln!(out, "star_type = {}", s(&sys.star_type));
            let _ = writeln!(out, "star_count = {}", sys.star_count);
            if let Some(o) = sys.owner {
                let _ = writeln!(out, "owned_by = {}", s(&world.empire(o).name));
            }
            if !sys.links.is_empty() {
                let links: Vec<String> = sys
                    .links
                    .iter()
                    .map(|l| s(&world.system(*l).name))
                    .collect();
                let _ = writeln!(out, "linked_systems = [{}]", links.join(", "));
            }
            let _ = writeln!(out, "coordinates = {}", coords(sys.coordinates));
            out.push('\n');

            for pid in sys.planet_ids() {
                let p = world.planet(pid);
                let pk = format!("system.{sk}.planets.{}", key(&p.name));
                let _ = writeln!(out, "[{pk}]");
                let _ = writeln!(out, "display_name = {}", s(&p.display_name));
                let _ = writeln!(out, "size = {}", s(p.size.as_str()));
                let _ = writeln!(out, "habitable = {}", p.habitable);
                let _ = writeln!(out, "total_pop = {}", p.pop);
                let _ = writeln!(out, "coordinates = {}", coords(p.coordinates));
                if let Some(o) = p.owner {
                    let _ = writeln!(out, "owned_by = {}", s(&world.empire(o).name));
                }
                if p.siege > 0.0 {
                    let _ = writeln!(out, "siege = {}", f(p.siege));
                }
                if p.growth_progress != 0.0 {
                    let _ = writeln!(out, "growth_progress = {}", f(p.growth_progress));
                }
                if let Some(m) = &p.moon_of {
                    let _ = writeln!(out, "moon_of = {}", s(m));
                }
                if let Some(t) = p.terraform_days_left {
                    let _ = writeln!(out, "terraform_days_left = {t}");
                }
                out.push('\n');

                for (i, t) in p.tiles.iter().enumerate() {
                    if t.resource_mults.is_empty()
                        && !t.water
                        && t.building.is_none()
                        && t.construction.is_none()
                    {
                        continue;
                    }
                    let _ = writeln!(out, "[{pk}.tiles.{i}]");
                    if !t.resource_mults.is_empty() {
                        let mults: Vec<String> = t
                            .resource_mults
                            .iter()
                            .map(|(r, m)| {
                                format!(
                                    "{{ resource = {}, mult = {} }}",
                                    self.r(&d.resource(*r).key),
                                    f(*m)
                                )
                            })
                            .collect();
                        let _ = writeln!(out, "resource_multipliers = [{}]", mults.join(", "));
                    }
                    if t.water {
                        out.push_str("water = true\n");
                    }
                    // Plain buildings are a bare reference; anything with state gets a
                    // `[...tiles.N.building]` table, written after the tile's own keys.
                    let detail = t.building.as_ref().and_then(|b| self.building_table(b));
                    if let (Some(b), None) = (&t.building, &detail) {
                        let _ = writeln!(out, "building = {}", self.r(&d.building(b.def).key));
                    }
                    match &t.construction {
                        Some(Construction::Building {
                            def,
                            days_left,
                            empire,
                            paid,
                        }) => {
                            let who = empire
                                .map(|e| format!(", empire = {}", s(&world.empire(e).name)))
                                .unwrap_or_default();
                            let paid: Vec<String> = paid
                                .iter()
                                .map(|a| {
                                    format!(
                                        "{{ resource = {}, count = {} }}",
                                        self.r(&d.resource(a.resource).key),
                                        f(a.count)
                                    )
                                })
                                .collect();
                            let paid = if paid.is_empty() {
                                String::new()
                            } else {
                                format!(", paid = [{}]", paid.join(", "))
                            };
                            let _ = writeln!(
                                out,
                                "construction = {{ building = {}, days_left = {days_left}{who}{paid} }}",
                                self.r(&d.building(*def).key)
                            );
                        }
                        Some(Construction::Upgrade { upgrade, days_left }) => {
                            let _ = writeln!(
                                out,
                                "construction = {{ upgrade = {}, days_left = {days_left} }}",
                                self.r(&d.upgrade(*upgrade).key)
                            );
                        }
                        None => {}
                    }
                    if let Some(table) = detail {
                        let _ = write!(out, "\n[{pk}.tiles.{i}.building]\n{table}");
                    }
                    out.push('\n');
                }
            }
        }
    }

    /// The `[...tiles.N.building]` table body for a building with state worth saving, or
    /// `None` if a bare reference is enough.
    fn building_table(&self, b: &BuildingInstance) -> Option<String> {
        let simple = b.upgrades.is_empty()
            && b.job_cap.is_none()
            && b.enabled
            && b.workers == 0
            && b.damage == 0.0;
        if simple {
            return None;
        }
        let mut out = format!("type = {}\n", self.r(&self.defs.building(b.def).key));
        if !b.upgrades.is_empty() {
            let ups: Vec<String> = b
                .upgrades
                .iter()
                .map(|u| self.r(&self.defs.upgrade(*u).key))
                .collect();
            let _ = writeln!(out, "upgrades = [{}]", ups.join(", "));
        }
        if b.workers > 0 {
            let _ = writeln!(out, "workers = {}", b.workers);
        }
        if let Some(c) = b.job_cap {
            let _ = writeln!(out, "job_cap = {c}");
        }
        if b.pinned {
            out.push_str("pinned = true\n");
        }
        if !b.enabled {
            out.push_str("enabled = false\n");
        }
        if b.damage > 0.0 {
            let _ = writeln!(out, "damage = {}", f(b.damage));
        }
        Some(out)
    }
}

fn coords(c: [f64; 2]) -> String {
    format!("{{ x = {}, y = {} }}", f(c[0]), f(c[1]))
}

fn planet_path(world: &World, p: PlanetId) -> String {
    let planet = world.planet(p);
    format!("{}/{}", world.system(planet.system).name, planet.name)
}

/// Relative path from directory `from` to file `to` (both absolute).
fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let a: Vec<Component> = from.components().collect();
    let b: Vec<Component> = to.components().collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..a.len() {
        out.push("..");
    }
    for c in &b[common..] {
        out.push(c.as_os_str());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative_path(Path::new("/a/b/saves"), Path::new("/a/b/resources.toml")),
            PathBuf::from("../resources.toml")
        );
        assert_eq!(
            relative_path(Path::new("/a/b"), Path::new("/a/b/x/y.toml")),
            PathBuf::from("x/y.toml")
        );
    }

    #[test]
    fn float_literals() {
        assert_eq!(f(1.0), "1.0");
        assert_eq!(f(f64::NAN), "0.0");
        assert_eq!(f(0.1 + 0.2), "0.3");
    }
}
