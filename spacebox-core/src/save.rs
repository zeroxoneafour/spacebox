//! Writes a [`World`] as a TOML save file in the same format the loader reads.
//!
//! The save imports every asset file it references (with paths relative to the save's own
//! location) and contains all game state inline, following `design/data/savegame.toml`:
//! `[game]`, `[empire.*]` (with `.fleets.*` and `.ai`), `[system.*]`, `[system.*.planets.*]`
//! and the non-empty `[...tile-N]` of each planet.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use crate::defs::{Cost, DefKey, Defs, ResourceAmount};
use crate::error::{Error, Result};
use crate::ids::*;
use crate::world::{BuildingInstance, FleetOrder, ProjectKind, World};

/// Writes the world to `path`: streamed to a temporary file next to it, which then replaces
/// `path`, so a failed save never leaves a half-written file behind.
pub fn save(world: &World, path: impl AsRef<Path>) -> Result<()> {
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
    let tmp = target.with_extension("toml.tmp");
    let write = |out: &mut String| -> io::Result<()> {
        let mut sink = io::BufWriter::new(fs::File::create(&tmp)?);
        w.write_state(world, out, &mut sink)?;
        sink.flush()
    };
    if let Err(e) = write(&mut out) {
        let _ = fs::remove_file(&tmp);
        return Err(Error::io(&tmp, e));
    }
    fs::rename(&tmp, &target).map_err(|e| Error::io(&target, e))
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
            if let Some(s) = e.dominant_species {
                self.use_file(d.species_def(s).key.file);
            }
            for p in &e.research {
                self.use_file(d.tech(p.tech).key.file);
                self.use_cost(&p.remaining);
                self.use_amounts(&p.paid);
            }
            for p in &e.shipbuilding {
                self.use_file(d.ship(p.ship).key.file);
                self.use_cost(&p.remaining);
                self.use_amounts(&p.paid);
            }
        }
        for f in &world.fleets {
            for s in &f.ships {
                self.use_file(d.ship(s.def).key.file);
            }
        }
        for p in &world.planets {
            for (s, _) in &p.population {
                self.use_file(d.species_def(*s).key.file);
            }
            if let Some(t) = &p.terraform {
                self.use_cost(&t.remaining);
                self.use_amounts(&t.paid);
            }
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
                if let Some(c) = &t.construction {
                    match c.kind {
                        ProjectKind::Building(b) => self.use_file(d.building(b).key.file),
                        ProjectKind::Upgrade(u) => self.use_file(d.upgrade(u).key.file),
                        ProjectKind::Remove => {}
                    }
                    self.use_cost(&c.remaining);
                    self.use_amounts(&c.paid);
                }
            }
        }
    }

    fn use_amounts(&mut self, list: &[ResourceAmount]) {
        for a in list {
            self.use_file(self.defs.resource(a.resource).key.file);
        }
    }

    fn use_cost(&mut self, c: &Cost) {
        for a in &c.resources {
            self.use_file(self.defs.resource(a.resource).key.file);
        }
    }

    fn r(&self, k: &DefKey) -> String {
        s(&format!("{}.{}", self.aliases[&k.file], k.name))
    }

    /// `[{ resource = ..., count = ... }, ..., { days = N }]`
    fn cost(&self, c: &Cost) -> String {
        let mut parts: Vec<String> = c
            .resources
            .iter()
            .filter(|a| a.count > 0.0)
            .map(|a| self.amount(&a.resource, a.count))
            .collect();
        if c.days > 0 {
            parts.push(format!("{{ days = {} }}", c.days));
        }
        format!("[{}]", parts.join(", "))
    }

    /// `[{ resource = ..., count = ... }, ...]`
    fn amounts(&self, list: &[ResourceAmount]) -> String {
        let parts: Vec<String> = list
            .iter()
            .filter(|a| a.count > 0.0)
            .map(|a| self.amount(&a.resource, a.count))
            .collect();
        format!("[{}]", parts.join(", "))
    }

    fn amount(&self, r: &ResourceId, count: f64) -> String {
        format!(
            "{{ resource = {}, count = {} }}",
            self.r(&self.defs.resource(*r).key),
            f(count)
        )
    }

    fn write_state(
        &self,
        world: &World,
        out: &mut String,
        sink: &mut impl Write,
    ) -> io::Result<()> {
        let d = self.defs;
        out.push_str("[game]\n");
        let _ = writeln!(out, "day = {}", world.day);
        let _ = writeln!(out, "seed = {}", world.seed);
        let rules = d.ruleset(world.ruleset);
        if !rules.builtin {
            let _ = writeln!(out, "ruleset = {}", self.r(&rules.key));
        }
        out.push('\n');
        for w in &world.wars {
            let _ = writeln!(out, "[game.wars.{}]", key(&w.name));
            let _ = writeln!(out, "display-name = {}", s(&w.display_name));
            for (field, goal) in ["offensive-goal", "defensive-goal"].iter().zip(w.goals) {
                if let Some(g) = goal {
                    let _ = writeln!(out, "{field} = {}", s(g.as_str()));
                }
            }
            let _ = writeln!(out, "starting-day = {}", w.starting_day);
            let _ = writeln!(out, "offensive-exhaustion = {}", w.exhaustion[0]);
            let _ = writeln!(out, "defensive-exhaustion = {}", w.exhaustion[1]);
            if let Some(v) = w.victory {
                let _ = writeln!(out, "victory = {}", s(v.as_str()));
            }
            out.push('\n');
        }
        for a in &world.alliances {
            let _ = writeln!(out, "[game.alliances.{}]", key(&a.name));
            let _ = writeln!(out, "display-name = {}\n", s(&a.display_name));
        }
        // Expired truces are dropped.
        for t in world.truces.iter().filter(|t| t.valid_until > world.day) {
            let _ = writeln!(out, "[game.truces.{}]", key(&t.name));
            let _ = writeln!(out, "display-name = {}", s(&t.display_name));
            let _ = writeln!(out, "valid-until = {}\n", t.valid_until);
        }

        for e in &world.empires {
            let ek = key(&e.name);
            let _ = writeln!(out, "[empire.{ek}]");
            let _ = writeln!(out, "display-name = {}", s(&e.display_name));
            if let Some(c) = &e.color {
                let _ = writeln!(out, "color = {}", s(c));
            }
            if let Some(c) = e.capital {
                let _ = writeln!(out, "capital = {}", s(&world.system(c).name));
            }
            if let Some(sp) = e.dominant_species {
                let _ = writeln!(out, "dominant-species = {}", self.r(&d.species_def(sp).key));
            }
            let stores: Vec<String> = e
                .storage
                .iter()
                .enumerate()
                .filter(|(_, v)| **v != 0.0)
                .map(|(r, v)| format!("    {},\n", self.amount(&ResourceId::from_idx(r), *v)))
                .collect();
            if !stores.is_empty() {
                let _ = write!(out, "resource-stores = [\n{}]\n", stores.concat());
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
            if !e.research.is_empty() {
                out.push_str("current-research = [\n");
                for p in &e.research {
                    // `paid` defaults to [] and is usually empty (research is mostly paid in
                    // transient resources), so it is only written when there is something in it.
                    let paid = if p.paid.is_empty() {
                        String::new()
                    } else {
                        format!(", paid = {}", self.amounts(&p.paid))
                    };
                    let _ = writeln!(
                        out,
                        "    {{ technology = {}{paid}, remaining = {} }},",
                        self.r(&d.tech(p.tech).key),
                        self.cost(&p.remaining)
                    );
                }
                out.push_str("]\n");
            }
            if !e.shipbuilding.is_empty() {
                out.push_str("current-shipbuilding = [\n");
                for p in &e.shipbuilding {
                    let planet = world.planet(p.planet);
                    let _ = writeln!(
                        out,
                        "    {{ ship = {}, system = {}, planet = {}, paid = {}, remaining = {} }},",
                        self.r(&d.ship(p.ship).key),
                        s(&world.system(planet.system).name),
                        s(&planet.name),
                        self.amounts(&p.paid),
                        self.cost(&p.remaining)
                    );
                }
                out.push_str("]\n");
            }
            out.push('\n');

            let _ = writeln!(out, "[empire.{ek}.policy]");
            let _ = writeln!(out, "accepts-migrants = {}", e.accepts_migrants);
            let _ = writeln!(out, "market-access = {}", e.market_access);
            let _ = writeln!(out, "total-war = {}\n", e.total_war);

            let _ = writeln!(out, "[empire.{ek}.diplomacy]");
            if e.humiliated > 0 {
                let _ = writeln!(out, "humiliated = {}", e.humiliated);
            }
            let list = |names: Vec<&str>| -> String {
                names.into_iter().map(s).collect::<Vec<_>>().join(", ")
            };
            let _ = writeln!(
                out,
                "claims = [{}]",
                list(
                    e.claims
                        .iter()
                        .map(|c| world.system(*c).name.as_str())
                        .collect()
                )
            );
            out.push_str("wars = [\n");
            for (w, side) in &e.wars {
                let _ = writeln!(
                    out,
                    "    {{ war = {}, side = {} }},",
                    s(&world.war(*w).name),
                    s(side.as_str())
                );
            }
            out.push_str("]\n");
            let _ = writeln!(
                out,
                "truces = [{}]",
                list(
                    e.truces
                        .iter()
                        .map(|t| &world.truces[t.idx()])
                        .filter(|t| t.valid_until > world.day)
                        .map(|t| t.name.as_str())
                        .collect()
                )
            );
            let _ = writeln!(
                out,
                "alliances = [{}]\n",
                list(
                    e.alliances
                        .iter()
                        .map(|a| world.alliances[a.idx()].name.as_str())
                        .collect()
                )
            );

            let eid = world.empire_by_name(&e.name);
            for fl in world.fleets.iter().filter(|fl| Some(fl.owner) == eid) {
                let _ = writeln!(out, "[empire.{ek}.fleets.{}]", key(&fl.name));
                let _ = writeln!(out, "display-name = {}", s(&fl.display_name));
                let _ = writeln!(out, "system = {}", s(&world.system(fl.system).name));
                out.push_str("ships = [\n");
                for (i, sh) in fl.ships.iter().enumerate() {
                    let _ = writeln!(
                        out,
                        "    {{ ship = {}, hull = {}, shield = {}, coords = {}, jump-charge = {} }},",
                        self.r(&d.ship(sh.def).key),
                        f(sh.hull),
                        f(sh.shield),
                        coords(fl.ship_coordinates(i)),
                        f(sh.jump_charge)
                    );
                }
                out.push_str("]\norders = [\n");
                for o in &fl.orders {
                    let _ = match o {
                        FleetOrder::Move { coordinates } => writeln!(
                            out,
                            "    {{ order = \"move\", coords = {} }},",
                            coords(*coordinates)
                        ),
                        FleetOrder::Jump { system } => writeln!(
                            out,
                            "    {{ order = \"jump\", system = {} }},",
                            s(&world.system(*system).name)
                        ),
                        FleetOrder::Bombard { planet, tile } => {
                            let t = tile.map(|t| format!(", tile = {t}")).unwrap_or_default();
                            writeln!(
                                out,
                                "    {{ order = \"bombard\", planet = {}{t} }},",
                                s(&world.planet(*planet).name)
                            )
                        }
                    };
                }
                out.push_str("]\n");
                let _ = writeln!(out, "in-combat = {}\n", fl.in_combat);
            }

            if let Some(ai) = &e.ai {
                let _ = writeln!(out, "[empire.{ek}.ai]");
                let _ = writeln!(out, "militant = {}", f(ai.militant));
                let _ = writeln!(out, "expansionist = {}", f(ai.expansionist));
                let _ = writeln!(out, "scientific = {}\n", f(ai.scientific));
            }
        }

        for sys in &world.systems {
            let sk = key(&sys.name);
            let _ = writeln!(out, "[system.{sk}]");
            let _ = writeln!(out, "display-name = {}", s(&sys.display_name));
            let _ = writeln!(out, "star-type = {}", s(&sys.star_type));
            let _ = writeln!(out, "star-count = {}", sys.star_count);
            if let Some(o) = sys.owner {
                let _ = writeln!(out, "owner = {}", s(&world.empire(o).name));
            }
            if let Some(o) = sys.occupier {
                let _ = writeln!(out, "occupier = {}", s(&world.empire(o).name));
            }
            let links: Vec<String> = sys
                .links
                .iter()
                .map(|l| s(&world.system(*l).name))
                .collect();
            let _ = writeln!(out, "linked-systems = [{}]", links.join(", "));
            let _ = writeln!(out, "coords = {}", coords(sys.coordinates));
            out.push('\n');

            for pid in sys.planet_ids() {
                let p = world.planet(pid);
                let pk = format!("system.{sk}.planets.{}", key(&p.name));
                let _ = writeln!(out, "[{pk}]");
                let _ = writeln!(out, "display-name = {}", s(&p.display_name));
                let _ = writeln!(out, "size = {}", s(p.size.as_str()));
                let _ = writeln!(out, "habitable = {}", p.habitable);
                let _ = writeln!(out, "coords = {}", coords(p.coordinates));
                if let Some(o) = p.owner {
                    let _ = writeln!(out, "owner = {}", s(&world.empire(o).name));
                }
                if let Some(o) = p.occupier {
                    let _ = writeln!(out, "occupier = {}", s(&world.empire(o).name));
                }
                if !p.population.is_empty() {
                    let pops: Vec<String> = p
                        .population
                        .iter()
                        .map(|(sp, n)| {
                            format!(
                                "{{ species = {}, count = {n} }}",
                                self.r(&d.species_def(*sp).key)
                            )
                        })
                        .collect();
                    let _ = writeln!(out, "population = [{}]", pops.join(", "));
                }
                if p.siege > 0.0 {
                    let _ = writeln!(out, "siege = {}", f(p.siege));
                }
                if let Some(t) = &p.terraform {
                    let _ = writeln!(
                        out,
                        "terraform = {{ paid = {}, remaining = {} }}",
                        self.amounts(&t.paid),
                        self.cost(&t.remaining)
                    );
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
                    let _ = writeln!(out, "[{pk}.tile-{i}]");
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
                        let _ = writeln!(out, "resource-multipliers = [{}]", mults.join(", "));
                    }
                    if t.water {
                        out.push_str("water = true\n");
                    }
                    if let Some(b) = &t.building {
                        let _ = writeln!(out, "building = {}", self.building(b));
                    }
                    if let Some(c) = &t.construction {
                        let what = match c.kind {
                            ProjectKind::Building(b) => {
                                format!("building = {}", self.r(&d.building(b).key))
                            }
                            ProjectKind::Upgrade(u) => {
                                format!("upgrade = {}", self.r(&d.upgrade(u).key))
                            }
                            ProjectKind::Remove => "remove = true".to_string(),
                        };
                        let who = c
                            .empire
                            .map(|e| format!(", empire = {}", s(&world.empire(e).name)))
                            .unwrap_or_default();
                        let _ = writeln!(
                            out,
                            "construction = {{ {what}, paid = {}, remaining = {}{who} }}",
                            self.amounts(&c.paid),
                            self.cost(&c.remaining)
                        );
                    }
                    out.push('\n');
                }
            }
            // Written out system by system, so the whole save never sits in memory.
            sink.write_all(out.as_bytes())?;
            out.clear();
        }
        sink.write_all(out.as_bytes())
    }

    /// A bare building reference, or an inline table for a building with state.
    fn building(&self, b: &BuildingInstance) -> String {
        let r = self.r(&self.defs.building(b.def).key);
        let simple = b.upgrades.is_empty()
            && b.job_cap.is_none()
            && b.enabled
            && b.workers == 0
            && b.damage == 0.0;
        if simple {
            return r;
        }
        let mut parts = vec![format!("building = {r}")];
        if !b.upgrades.is_empty() {
            let ups: Vec<String> = b
                .upgrades
                .iter()
                .map(|u| self.r(&self.defs.upgrade(*u).key))
                .collect();
            parts.push(format!("upgrades = [{}]", ups.join(", ")));
        }
        if b.workers > 0 {
            parts.push(format!("workers = {}", b.workers));
        }
        if let Some(c) = b.job_cap {
            parts.push(format!("job-cap = {c}"));
        }
        if b.pinned {
            parts.push("pinned = true".into());
        }
        if !b.enabled {
            parts.push("enabled = false".into());
        }
        if b.damage > 0.0 {
            parts.push(format!("damage = {}", f(b.damage)));
        }
        format!("{{ {} }}", parts.join(", "))
    }
}

fn coords(c: [f64; 2]) -> String {
    format!("[{}, {}]", f(c[0]), f(c[1]))
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
