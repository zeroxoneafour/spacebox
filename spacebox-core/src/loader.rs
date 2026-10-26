//! TOML loading, import handling and reference resolution (see `design/data/readme.md`).
//!
//! # Files and imports
//!
//! A file may start with a single `[import]` table mapping aliases to paths relative to the
//! importing file. Imported files are loaded (each once, deduplicated by canonical path) so
//! their own references can be resolved, but imports do not recurse for *names*: a file can
//! only reference its own definitions (`"name"`) and those of files it imports directly
//! (`"alias.name"`).
//!
//! # Typed references
//!
//! The slot a reference appears in determines which table it is looked up in. For example
//! `outputs = [{ resource = "res.alloy", count = 1 }]` looks for `[resource.alloy]` in the
//! file imported as `res`, and `job = "jobs.metallurgist"` looks for `[job.metallurgist]`.
//!
//! Tags (on buildings, jobs and technologies) are scoped the same way: `"forge"` is the
//! `forge` tag of the current file and `"base.forge"` that of the file imported as `base`. The
//! reserved building tags (`barren`, `blocker`, `land`, `water`, `space`) are global.
//!
//! # Large files
//!
//! Saves of large galaxies are not parsed in one piece: the `[empire.*]` and `[system.*]`
//! entries are split out (see [`crate::split`]) and parsed separately, systems in parallel,
//! so memory use stays close to the size of the world rather than many times the file size.
//! Files the splitter can't handle unambiguously are parsed whole, and parse errors are always
//! reported against the whole file.
//!
//! # Assets vs. state
//!
//! Asset tables are collected from every loaded file. Game state (`game`, `empire`, `system`)
//! is only read from the root file, so a save can import a scenario file to reuse its assets
//! without duplicating the scenario's systems.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;
use toml::{Table, Value};

use crate::defs::*;
use crate::error::{Error, Result};
use crate::geometry::PlanetSize;
use crate::ids::*;
use crate::raw::*;
use crate::world::*;

const STATE_TABLES: [&str; 3] = ["game", "empire", "system"];

/// Top-level asset tables, in parse order. Buildings and upgrades share `[building.*]`.
const ASSET_TABLES: [&str; 10] = [
    "resource",
    "job",
    "species",
    "building",
    "building-upgrade",
    "technology",
    "ruleset",
    "ship-weapon",
    "ship-outfit",
    "ship",
];

/// Loads only the asset definitions reachable from `path`.
pub fn load_assets(path: impl AsRef<Path>) -> Result<(Arc<Defs>, Vec<String>)> {
    let mut loader = Loader::default();
    let root = loader.load_file(path.as_ref())?;
    let (mut defs, _state) = loader.build_defs(root)?;
    if defs.rulesets.is_empty() {
        defs.rulesets.push(Ruleset::builtin(defs.resource_count()));
    }
    Ok((Arc::new(defs), loader.warnings))
}

/// Loads a save/scenario file: all reachable assets plus the root file's game state.
pub fn load_world(path: impl AsRef<Path>) -> Result<(World, Vec<String>)> {
    let mut loader = Loader::default();
    let root = loader.load_file(path.as_ref())?;
    let (defs, state) = loader.build_defs(root)?;
    let groups = std::mem::take(&mut loader.groups[root.idx()]);
    let text = std::mem::take(&mut loader.root_text);
    let world = build_world(defs, root, state, groups, &text, &mut loader.warnings)?;
    Ok((world, loader.warnings))
}

#[derive(Default)]
struct Loader {
    files: Vec<SourceFile>,
    tables: Vec<Table>,
    /// Per file: the `[empire.*]` / `[system.*]` entries, still unparsed where possible.
    groups: Vec<Vec<Group>>,
    /// The root file's text, which unparsed groups refer to.
    root_text: String,
    by_path: HashMap<PathBuf, FileId>,
    warnings: Vec<String>,
}

/// One `[empire.x]` or `[system.x]` entry of a file.
struct Group {
    top: &'static str,
    name: String,
    src: GroupSrc,
}

enum GroupSrc {
    /// Where the entry's sections are in the file, to be parsed on their own (see
    /// [`crate::split`]).
    Ranges(Vec<std::ops::Range<usize>>),
    /// Already parsed, when the file had to be parsed as a whole.
    Value(Value),
}

impl Group {
    /// The entry's table. Parse errors are reported against the whole file, so that line
    /// numbers are right.
    fn parse(self, path: &Path, file_text: &str) -> Result<Value> {
        match self.src {
            GroupSrc::Value(v) => Ok(v),
            GroupSrc::Ranges(ranges) => {
                let piece = crate::split::Group {
                    top: self.top,
                    name: self.name.clone(),
                    ranges,
                };
                let parsed: std::result::Result<Table, _> = piece.text(file_text).parse();
                let mut t = parsed.map_err(|e| whole_file_error(path, e))?;
                match t.remove(self.top) {
                    Some(Value::Table(mut entries)) => entries
                        .remove(&self.name)
                        .ok_or_else(|| Error::invalid(path, format!("[{}] is missing", self.name))),
                    _ => Err(Error::invalid(path, format!("[{}] is missing", self.top))),
                }
            }
        }
    }
}

/// The error of parsing the whole file (with correct line numbers), falling back to a piece's
/// own error.
fn whole_file_error(path: &Path, piece: toml::de::Error) -> Error {
    let whole = fs::read_to_string(path)
        .ok()
        .and_then(|t| t.parse::<Table>().err());
    Error::Parse {
        path: path.to_path_buf(),
        message: whole.unwrap_or(piece).to_string(),
    }
}

impl Loader {
    fn load_file(&mut self, path: &Path) -> Result<FileId> {
        let canon = fs::canonicalize(path).map_err(|e| Error::io(path, e))?;
        if let Some(id) = self.by_path.get(&canon) {
            return Ok(*id);
        }
        let text = fs::read_to_string(&canon).map_err(|e| Error::io(&canon, e))?;
        check_import_header(&text, &canon)?;
        let parse_error = |e: toml::de::Error| Error::Parse {
            path: canon.clone(),
            message: e.to_string(),
        };
        // Large saves are parsed piecewise; anything unusual is parsed as a whole.
        let split = crate::split::split(&text).and_then(|sp| {
            let head: Table = sp.head.parse().ok()?;
            let grouped = crate::split::GROUPED.iter().any(|g| head.contains_key(*g));
            (!grouped).then_some((head, sp.groups))
        });
        let (mut table, groups) = match split {
            Some((head, groups)) => {
                let groups = groups
                    .into_iter()
                    .map(|g| Group {
                        top: g.top,
                        name: g.name,
                        src: GroupSrc::Ranges(g.ranges),
                    })
                    .collect();
                (head, groups)
            }
            None => {
                let mut table: Table = text.parse().map_err(parse_error)?;
                let mut groups = Vec::new();
                for top in crate::split::GROUPED {
                    match table.remove(top) {
                        None => {}
                        Some(Value::Table(entries)) => {
                            groups.extend(entries.into_iter().map(|(name, v)| Group {
                                top,
                                name,
                                src: GroupSrc::Value(v),
                            }))
                        }
                        Some(_) => {
                            return Err(Error::invalid(&canon, format!("[{top}] must be a table")));
                        }
                    }
                }
                (table, groups)
            }
        };
        // Only the root file's state is read; its unparsed groups point into its text.
        if self.files.is_empty() {
            self.root_text = text;
        }

        let id = FileId::from_idx(self.files.len());
        self.files.push(SourceFile {
            path: canon.clone(),
            imports: Vec::new(),
        });
        self.tables.push(Table::new());
        self.groups.push(groups);
        self.by_path.insert(canon.clone(), id);

        let imports = match table.remove("import") {
            None => Table::new(),
            Some(Value::Table(t)) => t,
            Some(_) => return Err(Error::invalid(&canon, "[import] must be a table")),
        };
        let dir = canon.parent().unwrap_or(Path::new(".")).to_path_buf();
        for (alias, target) in imports {
            if alias.is_empty() || alias.contains('.') {
                return Err(Error::invalid(
                    &canon,
                    format!("invalid import alias '{alias}' (must be non-empty with no '.')"),
                ));
            }
            let Value::String(rel) = target else {
                return Err(Error::invalid(
                    &canon,
                    format!("import '{alias}' must be a path string"),
                ));
            };
            let target_id = self.load_file(&dir.join(&rel)).map_err(|e| match e {
                Error::Io { path, source } => Error::invalid(
                    &canon,
                    format!(
                        "cannot import '{alias}' = \"{rel}\" ({}): {source}",
                        path.display()
                    ),
                ),
                other => other,
            })?;
            self.files[id.idx()].imports.push((alias, target_id));
        }

        for key in table.keys() {
            if !ASSET_TABLES.contains(&key.as_str()) && !STATE_TABLES.contains(&key.as_str()) {
                return Err(Error::invalid(
                    &canon,
                    format!("unknown top-level table [{key}]"),
                ));
            }
        }
        self.tables[id.idx()] = table;
        Ok(id)
    }

    /// Declares every definition name, then parses all definitions. Returns the defs and the
    /// root file's state tables.
    fn build_defs(&mut self, root: FileId) -> Result<(Defs, Table)> {
        let mut names: Vec<[HashMap<String, u32>; KIND_COUNT]> =
            (0..self.files.len()).map(|_| Default::default()).collect();
        let mut decls: [Vec<(FileId, String, Value)>; KIND_COUNT] = Default::default();
        let mut root_state = Table::new();

        for (fi, table) in self.tables.iter_mut().enumerate() {
            let file = FileId::from_idx(fi);
            let path = &self.files[fi].path;
            for top in ASSET_TABLES {
                let Some(value) = table.remove(top) else {
                    continue;
                };
                let Value::Table(entries) = value else {
                    return Err(Error::invalid(
                        path,
                        format!("[{top}] must be a table of definitions"),
                    ));
                };
                for (name, def) in entries {
                    validate_name(&name)
                        .map_err(|m| Error::invalid(path, format!("[{top}.{name}]: {m}")))?;
                    let kind = match top {
                        "resource" => Kind::Resource,
                        "job" => Kind::Job,
                        "species" => Kind::Species,
                        "building" => Kind::Building,
                        "building-upgrade" => Kind::Upgrade,
                        "technology" => Kind::Technology,
                        "ruleset" => Kind::Ruleset,
                        "ship-weapon" => Kind::Weapon,
                        "ship-outfit" => Kind::Outfit,
                        _ => Kind::Ship,
                    };
                    let list = &mut decls[kind as usize];
                    names[fi][kind as usize].insert(name.clone(), list.len() as u32);
                    list.push((file, name, def));
                }
            }
            let state = std::mem::take(table);
            if file == root {
                root_state = state;
            } else if !state.is_empty() || !self.groups[fi].is_empty() {
                self.warnings.push(format!(
                    "{}: game state tables ignored (only read from the root file)",
                    path.display()
                ));
            }
        }

        let mut defs = Defs {
            files: self.files.clone(),
            resources: Vec::new(),
            jobs: Vec::new(),
            species: Vec::new(),
            buildings: Vec::new(),
            upgrades: Vec::new(),
            technologies: Vec::new(),
            rulesets: Vec::new(),
            ships: Vec::new(),
            weapons: Vec::new(),
            outfits: Vec::new(),
            tags: Vec::new(),
            names,
        };
        let [
            resources,
            jobs,
            species,
            buildings,
            upgrades,
            techs,
            rulesets,
            ships,
            weapons,
            outfits,
        ] = decls;

        for (file, name, value) in resources {
            let ctx = Ctx::new(&defs, file, Kind::Resource, &name);
            let raw: RawResource = ctx.de(value)?;
            defs.resources.push(ResourceDef {
                key: DefKey { file, name },
                display_name: raw.display_name,
                description: raw.description,
                transient: raw.transient,
                tradeable: raw.tradeable,
                base_value: raw.base_value,
            });
        }
        for (file, name, value) in jobs {
            let (raw, associated_resources) = {
                let ctx = Ctx::new(&defs, file, Kind::Job, &name);
                let raw: RawJob = ctx.de(value)?;
                if !(0..=100).contains(&raw.appeal) {
                    return Err(ctx.err("appeal must be between 0 and 100"));
                }
                let associated: Vec<ResourceId> = raw
                    .associated_resource
                    .iter()
                    .map(|r| ctx.resource(r))
                    .collect::<Result<_>>()?;
                (raw, associated)
            };
            let header = format!("[job.{name}]");
            let tags = resolve_tags(&mut defs, file, &header, &raw.job_tags)?;
            defs.jobs.push(JobDef {
                key: DefKey { file, name },
                display_name: raw.display_name,
                description: raw.description,
                appeal: raw.appeal,
                tags,
                associated_resources,
            });
        }
        for (file, name, value) in species {
            let ctx = Ctx::new(&defs, file, Kind::Species, &name);
            let raw: RawSpecies = ctx.de(value)?;
            let opt = |r: &Option<String>| r.as_deref().map(|r| ctx.resource(r)).transpose();
            let g = &raw.growth;
            let def = SpeciesDef {
                display_name: raw.display_name.clone(),
                description: raw.description.clone(),
                upkeep: ctx.amounts(&raw.upkeep.upkeep_cost)?,
                starvation_resource: opt(&raw.upkeep.starvation_resource)?,
                comfort_resource: opt(&raw.upkeep.comfort_resource)?,
                growth_rate: g.growth_rate.unwrap_or(0.003),
                min_growth: g.min_growth.unwrap_or(0.05),
                starvation_rate: g.starvation_rate.unwrap_or(0.01),
                migration_rate: g.migration_rate.unwrap_or(0.05),
                key: DefKey { file, name },
            };
            defs.species.push(def);
        }
        for (file, name, value) in buildings {
            let def = parse_building(&mut defs, file, name, value)?;
            defs.buildings.push(def);
        }
        for (file, name, value) in upgrades {
            let def = parse_upgrade(&mut defs, file, name, value)?;
            defs.upgrades.push(def);
        }
        for (file, name, value) in techs {
            let def = parse_tech(&mut defs, file, name, value)?;
            defs.technologies.push(def);
        }
        for (file, name, value) in rulesets {
            let def = parse_ruleset(&defs, file, name, value)?;
            defs.rulesets.push(def);
        }
        // Ships combine the stats of their weapons and outfits, so those come first.
        for (file, name, value) in weapons {
            let ctx = Ctx::new(&defs, file, Kind::Weapon, &name);
            let raw: RawWeapon = ctx.de(value)?;
            if raw.fire_rate <= 0.0 || raw.range < 0.0 {
                return Err(ctx.err("fire-rate must be positive and range non-negative"));
            }
            let def = WeaponDef {
                display_name: raw.display_name,
                description: raw.description,
                build_cost: ctx.amounts(&raw.build_cost)?,
                shield_damage: raw.shield_damage,
                hull_damage: raw.hull_damage,
                fire_rate: raw.fire_rate,
                range: raw.range,
                key: DefKey { file, name },
            };
            defs.weapons.push(def);
        }
        for (file, name, value) in outfits {
            let ctx = Ctx::new(&defs, file, Kind::Outfit, &name);
            let mut table = ctx.table(value)?;
            let head: RawOutfitHead =
                ctx.de(Value::Table(take_keys(&mut table, OUTFIT_HEAD_KEYS)))?;
            let mods = ship_mods(&ctx, ctx.de(Value::Table(table))?)?;
            let def = OutfitDef {
                display_name: head.display_name,
                description: head.description,
                build_cost: ctx.amounts(&head.build_cost)?,
                upkeep: ctx.amounts(&head.upkeep_cost)?,
                mods,
                key: DefKey { file, name },
            };
            defs.outfits.push(def);
        }
        for (file, name, value) in ships {
            let def = parse_ship(&defs, file, name, value)?;
            defs.ships.push(def);
        }

        compute_upgrade_availability(&mut defs)?;
        self.warn_unused_tags(&defs);
        Ok((defs, root_state))
    }

    /// Tags need no declaration, so a misspelt one silently matches nothing: warn about tags
    /// that upgrades, adjacency bonuses or technologies target but no building carries.
    fn warn_unused_tags(&mut self, defs: &Defs) {
        let mut targeted: Vec<(TagId, String)> = Vec::new();
        for u in &defs.upgrades {
            for t in &u.applies_to.tags {
                targeted.push((*t, format!("[building-upgrade.{}]", u.key.name)));
            }
        }
        for t in &defs.technologies {
            for m in &t.buildings {
                for tag in &m.targets.tags {
                    targeted.push((*tag, format!("[technology.{}]", t.key.name)));
                }
            }
        }
        for b in &defs.buildings {
            for m in &b.adjacency {
                for tag in &m.targets.tags {
                    targeted.push((*tag, format!("[building.{}]", b.key.name)));
                }
            }
        }
        for (tag, header) in targeted {
            if !defs.buildings.iter().any(|b| b.has_tag(tag)) {
                let t = defs.tag(tag);
                let file = t
                    .file
                    .map(|f| defs.files[f.idx()].path.display().to_string())
                    .unwrap_or_default();
                self.warnings.push(format!(
                    "{header}: no building has the tag '{}' of {file}",
                    t.name
                ));
            }
        }
    }
}

fn validate_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("names must not be empty".into());
    }
    if name.contains('.') || name.contains('/') {
        return Err("names must not contain '.' or '/'".into());
    }
    Ok(())
}

/// Ensures any `[import]` header is the first thing in the file.
fn check_import_header(text: &str, path: &Path) -> Result<()> {
    let is_import = |t: &str| {
        t.strip_prefix('[')
            .and_then(|r| r.split_once(']'))
            .is_some_and(|(inner, rest)| {
                let rest = rest.trim();
                inner.trim() == "import" && (rest.is_empty() || rest.starts_with('#'))
            })
    };
    let mut first = None;
    let mut has_import = false;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        first.get_or_insert(t);
        has_import |= is_import(t);
    }
    if has_import && !first.is_some_and(is_import) {
        return Err(Error::invalid(
            path,
            "[import] must be the first section of the file",
        ));
    }
    Ok(())
}

/// Parsing context for one definition: knows its file scope for reference resolution and
/// produces errors that point at `file: [kind.name]`.
struct Ctx<'a> {
    defs: &'a Defs,
    file: FileId,
    header: String,
}

impl<'a> Ctx<'a> {
    fn new(defs: &'a Defs, file: FileId, kind: Kind, name: &str) -> Self {
        Ctx {
            defs,
            file,
            header: format!("[{}.{name}]", kind.table()),
        }
    }

    fn with_header(defs: &'a Defs, file: FileId, header: String) -> Self {
        Ctx { defs, file, header }
    }

    fn err(&self, message: impl std::fmt::Display) -> Error {
        Error::invalid(
            &self.defs.files[self.file.idx()].path,
            format!("{}: {message}", self.header),
        )
    }

    fn de<T: serde::de::DeserializeOwned>(&self, value: Value) -> Result<T> {
        value
            .try_into()
            .map_err(|e: toml::de::Error| self.err(e.message()))
    }

    fn table(&self, value: Value) -> Result<Table> {
        match value {
            Value::Table(t) => Ok(t),
            _ => Err(self.err("must be a table")),
        }
    }

    fn lookup(&self, kind: Kind, reference: &str) -> Result<u32> {
        self.defs
            .lookup(self.file, kind, reference)
            .map_err(|m| self.err(m))
    }

    fn resource(&self, r: &str) -> Result<ResourceId> {
        self.lookup(Kind::Resource, r).map(ResourceId)
    }

    fn tech(&self, r: &str) -> Result<TechId> {
        self.lookup(Kind::Technology, r).map(TechId)
    }

    fn techs(&self, list: &[String]) -> Result<Vec<TechId>> {
        list.iter().map(|t| self.tech(t)).collect()
    }

    fn amount(&self, resource: &str, count: f64) -> Result<ResourceAmount> {
        if !count.is_finite() {
            return Err(self.err(format!("count for '{resource}' must be finite")));
        }
        Ok(ResourceAmount {
            resource: self.resource(resource)?,
            count,
        })
    }

    fn amounts(&self, raw: &[RawAmount]) -> Result<Vec<ResourceAmount>> {
        raw.iter()
            .map(|a| self.amount(&a.resource, a.count))
            .collect()
    }

    /// Reads a list mixing `{ resource, count }` entries with one special entry kind
    /// (`days` or `amenities`), whose values are summed and returned separately.
    fn mixed_list(
        &self,
        what: &str,
        special: &str,
        values: &[Value],
    ) -> Result<(Vec<ResourceAmount>, f64)> {
        let mut resources: Vec<ResourceAmount> = Vec::new();
        let mut extra = 0.0;
        for v in values {
            let Some(t) = v.as_table() else {
                return Err(self.err(format!("{what} entries must be tables")));
            };
            if let Some(x) = t.get(special) {
                let n = match x {
                    Value::Integer(i) => *i as f64,
                    Value::Float(f) => *f,
                    _ => return Err(self.err(format!("`{special}` in {what} must be a number"))),
                };
                if t.len() != 1 || !n.is_finite() || n < 0.0 {
                    return Err(self.err(format!(
                        "{what}: `{{ {special} = N }}` takes a single non-negative number"
                    )));
                }
                extra += n;
                continue;
            }
            let a: RawAmount = self.de(v.clone()).map_err(|_| {
                self.err(format!(
                    "{what} entries must be {{ resource, count }} or {{ {special} }}"
                ))
            })?;
            add_amounts(&mut resources, &[self.amount(&a.resource, a.count)?]);
        }
        Ok((resources, extra))
    }

    /// A project cost: resources plus `{ days }`.
    fn cost(&self, what: &str, values: &[Value]) -> Result<Cost> {
        let (resources, days) = self.mixed_list(what, "days", values)?;
        if days.fract() != 0.0 {
            return Err(self.err(format!("{what}: days must be a whole number")));
        }
        Ok(Cost {
            resources,
            days: days as u32,
        })
    }

    /// A production list: resources plus `{ amenities }`.
    fn yields(&self, what: &str, values: &[Value]) -> Result<Yield> {
        let (resources, amenities) = self.mixed_list(what, "amenities", values)?;
        Ok(Yield {
            resources,
            amenities,
        })
    }

    fn modifier(&self, raw: RawModifier, allow_job: bool) -> Result<Modifier> {
        if raw.job.is_some() && !allow_job {
            return Err(self.err("only upgrades can replace a building's `job`"));
        }
        Ok(Modifier {
            upkeep_add: self.amounts(&raw.upkeep_cost_add)?,
            upkeep_mult: raw.upkeep_cost_mult.unwrap_or(1.0),
            input_add: self.amounts(&raw.input_add)?,
            input_mult: raw.input_mult.unwrap_or(1.0),
            output_add: self.yields("output-add", &raw.output_add)?,
            output_mult: raw.output_mult.unwrap_or(1.0),
            passive_add: self.yields("passive-output-add", &raw.passive_output_add)?,
            passive_mult: raw.passive_output_mult.unwrap_or(1.0),
            max_workers_add: raw.max_workers_add.unwrap_or(0.0),
            max_workers_mult: raw.max_workers_mult.unwrap_or(1.0),
            housing_add: raw.housing_add.unwrap_or(0.0),
            housing_mult: raw.housing_mult.unwrap_or(1.0),
            job: raw
                .job
                .as_deref()
                .map(|j| self.lookup(Kind::Job, j).map(JobId))
                .transpose()?,
        })
    }
}

fn ship_mods(ctx: &Ctx, raw: RawShipMods) -> Result<ShipMods> {
    let d = ShipMods::default();
    let m = ShipMods {
        hull_add: raw.hull_add.unwrap_or(d.hull_add),
        shield_add: raw.shield_add.unwrap_or(d.shield_add),
        shield_regen_add: raw.shield_regen_add.unwrap_or(d.shield_regen_add),
        base_speed_add: raw.base_speed_add.unwrap_or(d.base_speed_add),
        hull_mult: raw.hull_mult.unwrap_or(d.hull_mult),
        shield_mult: raw.shield_mult.unwrap_or(d.shield_mult),
        shield_regen_mult: raw.shield_regen_mult.unwrap_or(d.shield_regen_mult),
        base_speed_mult: raw.base_speed_mult.unwrap_or(d.base_speed_mult),
        hyperjump_mult: raw.hyperjump_mult.unwrap_or(d.hyperjump_mult),
        fire_rate_mult: raw.fire_rate_mult.unwrap_or(d.fire_rate_mult),
        damage_mult: raw.damage_mult.unwrap_or(d.damage_mult),
        range_mult: raw.range_mult.unwrap_or(d.range_mult),
    };
    if m.fire_rate_mult <= 0.0 {
        return Err(ctx.err("fire-rate-mult must be positive"));
    }
    Ok(m)
}

fn take_keys(table: &mut Table, keys: &[&str]) -> Table {
    let mut out = Table::new();
    for k in keys {
        if let Some(v) = table.remove(*k) {
            out.insert((*k).to_string(), v);
        }
    }
    out
}

/// Resolves a tag reference: a reserved tag, `"tag"` in the current file or `"alias.tag"` in
/// an imported one. Tags need no declaration; any name can be used.
fn resolve_tag(defs: &mut Defs, file: FileId, header: &str, s: &str) -> Result<TagId> {
    let tag = if RESERVED_TAGS.contains(&s) {
        Tag {
            file: None,
            name: s.to_string(),
        }
    } else {
        match s.split_once('.') {
            Some((alias, name)) => {
                let target = defs.files[file.idx()].import(alias).ok_or_else(|| {
                    Ctx::with_header(defs, file, header.to_string())
                        .err(format!("unknown import alias '{alias}' in tag '{s}'"))
                })?;
                Tag {
                    file: Some(target),
                    name: name.to_string(),
                }
            }
            None => Tag {
                file: Some(file),
                name: s.to_string(),
            },
        }
    };
    Ok(match defs.tag_id(&tag) {
        Some(id) => id,
        None => {
            defs.tags.push(tag);
            TagId::from_idx(defs.tags.len() - 1)
        }
    })
}

fn resolve_tags(
    defs: &mut Defs,
    file: FileId,
    header: &str,
    list: &[String],
) -> Result<Vec<TagId>> {
    list.iter()
        .map(|t| resolve_tag(defs, file, header, t))
        .collect()
}

/// Resolves `buildings` and `building-tags` lists into [`Targets`].
fn resolve_targets(
    defs: &mut Defs,
    file: FileId,
    header: &str,
    buildings: &[String],
    tags: &[String],
) -> Result<Targets> {
    let ctx = Ctx::with_header(defs, file, header.to_string());
    let buildings = buildings
        .iter()
        .map(|b| ctx.lookup(Kind::Building, b).map(BuildingId))
        .collect::<Result<Vec<_>>>()?;
    Ok(Targets {
        buildings,
        tags: resolve_tags(defs, file, header, tags)?,
    })
}

/// Parses an `adjacency-bonus` entry: `buildings` and/or `building-tags` plus modifier
/// fields.
fn parse_adjacency(
    defs: &mut Defs,
    file: FileId,
    header: &str,
    mut table: Table,
) -> Result<TargetedModifier> {
    let header = format!("{header} adjacency-bonus");
    let (names, tags) = {
        let ctx = Ctx::with_header(defs, file, header.clone());
        let list = |t: &mut Table, k: &str| -> Result<Vec<String>> {
            match t.remove(k) {
                None => Ok(Vec::new()),
                Some(v) => ctx.de(v),
            }
        };
        (
            list(&mut table, "buildings")?,
            list(&mut table, "building-tags")?,
        )
    };
    if names.is_empty() && tags.is_empty() {
        return Err(Ctx::with_header(defs, file, header)
            .err("needs `buildings` and/or `building-tags` to say which neighbours count"));
    }
    let targets = resolve_targets(defs, file, &header, &names, &tags)?;
    let ctx = Ctx::with_header(defs, file, header);
    let modifier = ctx.modifier(ctx.de(Value::Table(table))?, false)?;
    Ok(TargetedModifier { targets, modifier })
}

fn parse_building(
    defs: &mut Defs,
    file: FileId,
    name: String,
    value: Value,
) -> Result<BuildingDef> {
    let header = format!("[building.{name}]");
    let raw: RawBuilding = Ctx::with_header(defs, file, header.clone()).de(value)?;
    let tags = resolve_tags(defs, file, &header, &raw.building_tags)?;
    let has = |tag: &str| raw.building_tags.iter().any(|c| c == tag);
    let (barren, blocker, water, space) = (
        has(BARREN_TAG),
        has(BLOCKER_TAG),
        has(WATER_TAG),
        has(SPACE_TAG),
    );
    // Land is implicit unless the building is tagged water or space.
    let land = has(LAND_TAG) || (!water && !space);
    let mut adjacency = Vec::new();
    for t in raw.adjacency_bonus {
        adjacency.push(parse_adjacency(defs, file, &header, t)?);
    }
    let ctx = Ctx::with_header(defs, file, header);
    let job = raw
        .job
        .as_deref()
        .map(|j| ctx.lookup(Kind::Job, j).map(JobId))
        .transpose()?;
    if raw.max_workers.is_some_and(|w| w > 0) && job.is_none() {
        return Err(ctx.err("buildings with max-workers need a `job`"));
    }
    if job.is_some() && raw.max_workers.is_none() {
        return Err(ctx.err("buildings with a `job` need max-workers"));
    }
    Ok(BuildingDef {
        display_name: raw.display_name,
        description: raw.description,
        max_workers: raw.max_workers,
        job,
        tags,
        barren,
        blocker,
        land,
        water,
        space,
        housing: raw.housing,
        build_cost: ctx.cost("build-cost", &raw.build_cost)?,
        remove_cost: raw
            .remove_cost
            .as_deref()
            .map(|c| ctx.cost("remove-cost", c))
            .transpose()?,
        max_per_planet: raw.max_per_planet,
        hp: raw.hp,
        requires_tech: ctx.techs(&raw.requires_tech)?,
        upkeep: ctx.amounts(&raw.upkeep_cost)?,
        inputs: ctx.amounts(&raw.inputs)?,
        outputs: ctx.yields("outputs", &raw.outputs)?,
        passive_outputs: ctx.yields("passive-outputs", &raw.passive_outputs)?,
        adjacency,
        available_upgrades: Vec::new(),
        key: DefKey { file, name },
    })
}

fn parse_upgrade(defs: &mut Defs, file: FileId, name: String, value: Value) -> Result<UpgradeDef> {
    let header = format!("[building-upgrade.{name}]");
    let (head, modifier) = {
        let ctx = Ctx::with_header(defs, file, header.clone());
        let mut table = ctx.table(value)?;
        let head: RawUpgradeHead =
            ctx.de(Value::Table(take_keys(&mut table, UPGRADE_HEAD_KEYS)))?;
        let raw: RawModifier = ctx.de(Value::Table(table)).map_err(|e| {
            ctx.err(format!(
                "{e} (upgrades take display-name, description, buildings, building-tags, \
                 after-upgrades, requires-tech, build-cost and modifier fields)"
            ))
        })?;
        (head, ctx.modifier(raw, true)?)
    };
    if head.buildings.is_empty() && head.building_tags.is_empty() {
        return Err(Ctx::with_header(defs, file, header)
            .err("an upgrade needs `buildings` and/or `building-tags` to apply to"));
    }
    let applies_to = resolve_targets(defs, file, &header, &head.buildings, &head.building_tags)?;
    let ctx = Ctx::with_header(defs, file, header);
    let after_upgrades = head
        .after_upgrades
        .iter()
        .map(|u| ctx.lookup(Kind::Upgrade, u).map(UpgradeId))
        .collect::<Result<Vec<_>>>()?;
    Ok(UpgradeDef {
        display_name: head.display_name,
        description: head.description,
        applies_to,
        after_upgrades,
        modifier,
        build_cost: ctx.cost("build-cost", &head.build_cost)?,
        requires_tech: ctx.techs(&head.requires_tech)?,
        key: DefKey { file, name },
    })
}

fn parse_tech(defs: &mut Defs, file: FileId, name: String, value: Value) -> Result<TechDef> {
    let header = format!("[technology.{name}]");
    let raw: RawTech = Ctx::with_header(defs, file, header.clone()).de(value)?;

    // `[technology.x.buildings]`: one modifier per field, each with its own targets.
    let mut buildings = Vec::new();
    for (field, v) in raw.buildings.clone().unwrap_or_default() {
        let fctx = Ctx::with_header(defs, file, format!("{header}.buildings.{field}"));
        let Value::Table(mut t) = v else {
            return Err(
                fctx.err("must be { value = ..., buildings = [...], building-tags = [...] }")
            );
        };
        let Some(value) = t.remove("value") else {
            return Err(fctx.err("missing `value`"));
        };
        let list = |t: &mut Table, k: &str| -> Result<Vec<String>> {
            match t.remove(k) {
                None => Ok(Vec::new()),
                Some(b) => fctx.de(b),
            }
        };
        let (names, tags) = (list(&mut t, "buildings")?, list(&mut t, "building-tags")?);
        if let Some(k) = t.keys().next() {
            return Err(fctx.err(format!(
                "unknown field `{k}` (expected value, buildings and building-tags)"
            )));
        }
        let mut one = Table::new();
        one.insert(field.clone(), value);
        let modifier = fctx.modifier(fctx.de(Value::Table(one))?, false)?;
        let targets = resolve_targets(defs, file, &header, &names, &tags)?;
        buildings.push(TargetedModifier { targets, modifier });
    }

    let tags = resolve_tags(defs, file, &header, &raw.research_tags)?;
    let ctx = Ctx::with_header(defs, file, header);
    let (fleet_size_add, ships) = match raw.ships.clone() {
        None => (0, ShipMods::default()),
        Some(mut t) => {
            let add = match t.remove("fleet-size-add") {
                None => 0,
                Some(Value::Integer(n)) if n >= 0 => n as u32,
                Some(_) => return Err(ctx.err("ships.fleet-size-add must be a whole number")),
            };
            (add, ship_mods(&ctx, ctx.de(Value::Table(t))?)?)
        }
    };
    let cost = ctx.amounts(&raw.research_resources)?;
    if cost.iter().any(|a| a.count < 0.0) {
        return Err(ctx.err("research-resources must be non-negative"));
    }
    Ok(TechDef {
        display_name: raw.display_name,
        description: raw.description,
        cost,
        tags,
        prerequisites: ctx.techs(&raw.prerequisites)?,
        pop_growth_mult: raw.pop.growth_mult.unwrap_or(1.0),
        fleet_size_add,
        ships,
        buildings,
        key: DefKey { file, name },
    })
}

fn parse_ruleset(defs: &Defs, file: FileId, name: String, value: Value) -> Result<Ruleset> {
    let ctx = Ctx::new(defs, file, Kind::Ruleset, &name);
    let raw: RawRuleset = ctx.de(value)?;
    let normal = |r: &Option<String>, what: &str| -> Result<Option<ResourceId>> {
        let Some(r) = r.as_deref() else {
            return Ok(None);
        };
        let id = ctx.resource(r)?;
        if defs.is_transient(id) {
            return Err(ctx.err(format!("{what} cannot be a transient resource")));
        }
        Ok(Some(id))
    };
    let terraform = |t: &Option<RawTerraform>| -> Result<Option<TerraformRule>> {
        t.as_ref()
            .map(|t| {
                Ok(TerraformRule {
                    requires_tech: t
                        .requires_tech
                        .as_deref()
                        .map(|r| ctx.tech(r))
                        .transpose()?,
                    cost: Cost {
                        resources: ctx.amounts(&t.cost)?,
                        days: t.days,
                    },
                })
            })
            .transpose()
    };
    let b = Ruleset::builtin(defs.resource_count());
    let mut storage_caps = vec![None; defs.resource_count()];
    for a in ctx.amounts(&raw.resources.storage_caps)? {
        storage_caps[a.resource.idx()] = Some(a.count.max(0.0));
    }
    let wr = &raw.war;
    let (am, ec, ex, sh, inv) = (
        &raw.amenities,
        &raw.economy,
        &raw.expansion,
        &raw.ships,
        &raw.invasion,
    );
    Ok(Ruleset {
        display_name: raw.display_name.clone(),
        builtin: false,
        starting_stores: ctx.amounts(&raw.resources.starting_stores)?,
        storage_caps,
        base_amenities: am.base_amenities.unwrap_or(b.base_amenities),
        amenities_per_pop: am.amenities_per_pop.unwrap_or(b.amenities_per_pop),
        base_stability: am.base_stability.unwrap_or(b.base_stability),
        stability_output_factor: am
            .stability_output_factor
            .unwrap_or(b.stability_output_factor),
        unemployment_stability_penalty: am
            .unemployment_stability_penalty
            .unwrap_or(b.unemployment_stability_penalty),
        homelessness_stability_penalty: am
            .homelessness_stability_penalty
            .unwrap_or(b.homelessness_stability_penalty),
        comfort_stability_penalty: am
            .comfort_stability_penalty
            .unwrap_or(b.comfort_stability_penalty),
        crisis_stability_penalty: am
            .crisis_stability_penalty
            .unwrap_or(b.crisis_stability_penalty),
        currency: normal(&ec.currency, "economy.currency")?,
        logistics_loss: ec.logistics_loss.unwrap_or(b.logistics_loss),
        market_fee: ec.market_fee.unwrap_or(b.market_fee),
        market_reserve: ec.market_reserve.unwrap_or(b.market_reserve),
        scarcity_appeal: ec.scarcity_appeal.unwrap_or(b.scarcity_appeal),
        scarcity_target_days: ec.scarcity_target_days.unwrap_or(b.scarcity_target_days),
        ai_interval_days: raw.ai.ai_interval_days.unwrap_or(b.ai_interval_days).max(1),
        research_speed_mult: raw
            .technology
            .research_speed_mult
            .unwrap_or(b.research_speed_mult)
            .max(0.0),
        expansion_resource: normal(&ex.expansion_resource, "expansion.expansion-resource")?,
        expansion_resource_income: ex
            .expansion_resource_income
            .unwrap_or(b.expansion_resource_income),
        expansion_base_cost: ex.expansion_base_cost.unwrap_or(b.expansion_base_cost),
        expansion_base_days: ex.expansion_base_days.unwrap_or(b.expansion_base_days),
        hyperjump_days: sh.hyperjump_days.unwrap_or(b.hyperjump_days).max(0.0),
        combat_subticks: sh.combat_subticks.unwrap_or(b.combat_subticks).max(1),
        arrival_radius: sh.arrival_radius.unwrap_or(b.arrival_radius),
        ship_repair_rate: sh.ship_repair_rate.unwrap_or(b.ship_repair_rate),
        base_fleet_size: sh.base_fleet_size.unwrap_or(b.base_fleet_size).max(1),
        building_hp: inv.building_hp.unwrap_or(b.building_hp).max(1.0),
        building_disable_damage: inv
            .building_disable_damage
            .unwrap_or(b.building_disable_damage),
        building_repair_rate: inv.building_repair_rate.unwrap_or(b.building_repair_rate),
        bombardment_amenities_penalty: inv
            .bombardment_amenities_penalty
            .unwrap_or(b.bombardment_amenities_penalty),
        surrender_resistance: inv.surrender_resistance.unwrap_or(b.surrender_resistance),
        siege_decay: inv.siege_decay.unwrap_or(b.siege_decay),
        truce_days: raw.diplomacy.truce_days.unwrap_or(b.truce_days),
        humiliate_cost: wr.humiliate_cost.unwrap_or(b.humiliate_cost),
        claim_cost: wr.claim_cost.unwrap_or(b.claim_cost),
        total_cost: wr.total_cost.unwrap_or(b.total_cost),
        humiliation_days: wr.humiliation_days.unwrap_or(b.humiliation_days),
        daily_exhaustion: wr.daily_exhaustion.unwrap_or(b.daily_exhaustion),
        ship_loss_exhaustion: wr.ship_loss_exhaustion.unwrap_or(b.ship_loss_exhaustion),
        planet_loss_exhaustion: wr
            .planet_loss_exhaustion
            .unwrap_or(b.planet_loss_exhaustion),
        populated_planet_loss_exhaustion: wr
            .populated_planet_loss_exhaustion
            .unwrap_or(b.populated_planet_loss_exhaustion),
        crisis_exhaustion: wr.crisis_exhaustion.unwrap_or(b.crisis_exhaustion),
        terraform_large: terraform(&raw.terraforming.large)?,
        terraform_small: terraform(&raw.terraforming.small)?,
        key: DefKey { file, name },
    })
}

/// Parses a ship design, combining its own cost and stats with its weapons and outfits.
fn parse_ship(defs: &Defs, file: FileId, name: String, value: Value) -> Result<ShipDef> {
    let ctx = Ctx::new(defs, file, Kind::Ship, &name);
    let raw: RawShip = ctx.de(value)?;
    if raw.hull <= 0.0 || raw.base_speed < 0.0 {
        return Err(ctx.err("hull must be positive and base-speed non-negative"));
    }
    let weapons = raw
        .weapons
        .iter()
        .map(|w| ctx.lookup(Kind::Weapon, w).map(WeaponId))
        .collect::<Result<Vec<_>>>()?;
    let outfits = raw
        .outfits
        .iter()
        .map(|o| ctx.lookup(Kind::Outfit, o).map(OutfitId))
        .collect::<Result<Vec<_>>>()?;

    let mut build_cost = ctx.cost("build-cost", &raw.build_cost)?;
    let mut upkeep = ctx.amounts(&raw.upkeep_cost)?;
    let mut outfit_mods = ShipMods::default();
    for w in &weapons {
        add_amounts(&mut build_cost.resources, &defs.weapon(*w).build_cost);
    }
    for o in &outfits {
        let o = defs.outfit(*o);
        add_amounts(&mut build_cost.resources, &o.build_cost);
        add_amounts(&mut upkeep, &o.upkeep);
        outfit_mods.combine(&o.mods);
    }
    let mut def = ShipDef {
        display_name: raw.display_name,
        description: raw.description,
        requires_tech: ctx.techs(&raw.requires_tech)?,
        build_cost,
        upkeep,
        hull: raw.hull,
        shield: raw.shield,
        shield_regen: raw.shield_regen,
        base_speed: raw.base_speed,
        hyperjump_mult: raw.hyperjump_mult,
        fleet_size: raw.fleet_size.max(1),
        outfit_mods,
        base_stats: ShipStats {
            max_hull: 0.0,
            max_shield: 0.0,
            shield_regen: 0.0,
            speed: 0.0,
            hyperjump_mult: 1.0,
            armament: Vec::new(),
            max_range: 0.0,
        },
        weapons,
        outfits,
        key: DefKey { file, name },
    };
    def.base_stats = def.stats(defs, &ShipMods::default());
    Ok(def)
}

/// An upgrade is available for a building if `applies-to` matches it and every upgrade in
/// `after-upgrades` is (recursively) available for it too.
fn compute_upgrade_availability(defs: &mut Defs) -> Result<()> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unknown,
        Visiting,
        Yes,
        No,
    }
    fn visit(
        defs: &Defs,
        b: BuildingId,
        u: usize,
        state: &mut [State],
    ) -> std::result::Result<bool, String> {
        match state[u] {
            State::Yes => return Ok(true),
            State::No => return Ok(false),
            State::Visiting => {
                return Err(format!(
                    "upgrade dependency cycle involving [building.{}]",
                    defs.upgrades[u].key.name
                ));
            }
            State::Unknown => {}
        }
        state[u] = State::Visiting;
        let up = &defs.upgrades[u];
        let mut ok = defs.targets(&up.applies_to, b);
        for dep in &up.after_upgrades {
            // Evaluate every dependency so cycles are always detected.
            ok &= visit(defs, b, dep.idx(), state)?;
        }
        state[u] = if ok { State::Yes } else { State::No };
        Ok(ok)
    }

    let mut all = Vec::with_capacity(defs.buildings.len());
    for b in 0..defs.buildings.len() {
        let mut state = vec![State::Unknown; defs.upgrades.len()];
        let mut available = Vec::new();
        for u in 0..defs.upgrades.len() {
            if visit(defs, BuildingId::from_idx(b), u, &mut state)
                .map_err(|m| Error::invalid(&defs.files[defs.upgrades[u].key.file.idx()].path, m))?
            {
                available.push(UpgradeId::from_idx(u));
            }
        }
        all.push(available);
    }
    for (b, available) in defs.buildings.iter_mut().zip(all) {
        b.available_upgrades = available;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Game state
// ---------------------------------------------------------------------------------------------

/// Empire parts that refer to systems and planets, resolved once those exist.
struct Deferred {
    id: EmpireId,
    name: String,
    shipbuilding: Vec<RawShipbuilding>,
    fleets: Table,
}

fn build_world(
    mut defs: Defs,
    root: FileId,
    mut state: Table,
    groups: Vec<Group>,
    text: &str,
    warnings: &mut Vec<String>,
) -> Result<World> {
    let root_path = defs.files[root.idx()].path.clone();
    let invalid = |m: String| Error::invalid(&root_path, m);

    let game: RawGame = match state.remove("game") {
        None => RawGame::default(),
        Some(v) => Ctx::with_header(&defs, root, "[game]".into()).de(v)?,
    };
    let ruleset = match &game.ruleset {
        Some(r) => {
            RulesetId(Ctx::with_header(&defs, root, "[game]".into()).lookup(Kind::Ruleset, r)?)
        }
        None if defs.rulesets.len() == 1 => RulesetId(0),
        None if defs.rulesets.is_empty() => {
            warnings.push("no [ruleset.*] loaded; using built-in defaults".into());
            defs.rulesets.push(Ruleset::builtin(defs.resource_count()));
            RulesetId(0)
        }
        None => {
            return Err(invalid(
                "several rulesets are loaded; choose one with [game] ruleset = \"...\"".into(),
            ));
        }
    };

    let (empire_groups, system_groups): (Vec<Group>, Vec<Group>) =
        groups.into_iter().partition(|g| g.top == "empire");
    let empire_ids: HashMap<String, EmpireId> = empire_groups
        .iter()
        .enumerate()
        .map(|(i, g)| (g.name.clone(), EmpireId::from_idx(i)))
        .collect();
    let system_ids: HashMap<String, SystemId> = system_groups
        .iter()
        .enumerate()
        .map(|(i, g)| (g.name.clone(), SystemId::from_idx(i)))
        .collect();
    for g in empire_groups.iter().chain(&system_groups) {
        validate_name(&g.name).map_err(|m| invalid(format!("'{}': {m}", g.name)))?;
    }

    let defs = Arc::new(defs);
    let mut world = World::new(defs.clone(), ruleset);
    world.day = game.day;
    world.seed = game.seed;
    world.root_file = Some(root);

    let gctx = Ctx::with_header(&defs, root, "[game]".into());
    let mut war_ids = HashMap::new();
    for (name, w) in game.wars.0 {
        validate_name(&name).map_err(|m| gctx.err(format!("war '{name}': {m}")))?;
        let goal = |g: &str| {
            WarGoal::parse(g).ok_or_else(|| {
                gctx.err(format!(
                    "war '{name}': unknown goal '{g}' (expected humiliate, claim or total)"
                ))
            })
        };
        let offensive = goal(&w.offensive_goal)?;
        let defensive = w.defensive_goal.as_deref().map(goal).transpose()?;
        let victory = w
            .victory
            .as_deref()
            .map(|v| {
                WarOutcome::parse(v).ok_or_else(|| {
                    gctx.err(format!(
                        "war '{name}': unknown victory '{v}' (expected offensive, defensive or status quo)"
                    ))
                })
            })
            .transpose()?;
        for x in [w.offensive_exhaustion, w.defensive_exhaustion] {
            if !(0.0..=1.0).contains(&x) {
                return Err(gctx.err(format!("war '{name}': exhaustion {x} is not within 0..1")));
            }
        }
        war_ids.insert(name.clone(), WarId::from_idx(world.wars.len()));
        world.wars.push(War {
            name,
            display_name: w.display_name,
            goals: [Some(offensive), defensive],
            starting_day: w.starting_day,
            exhaustion: [w.offensive_exhaustion, w.defensive_exhaustion],
            victory,
            proposals: [None, None],
        });
    }
    let mut alliance_ids = HashMap::new();
    for (name, a) in game.alliances.0 {
        validate_name(&name).map_err(|m| gctx.err(format!("alliance '{name}': {m}")))?;
        alliance_ids.insert(name.clone(), AllianceId::from_idx(world.alliances.len()));
        world.alliances.push(Alliance {
            name,
            display_name: a.display_name,
            invites: Vec::new(),
        });
    }
    let mut truce_ids = HashMap::new();
    for (name, t) in game.truces.0 {
        validate_name(&name).map_err(|m| gctx.err(format!("truce '{name}': {m}")))?;
        truce_ids.insert(name.clone(), TruceId::from_idx(world.truces.len()));
        world.truces.push(Truce {
            name,
            display_name: t.display_name,
            valid_until: t.valid_until,
        });
    }

    let mut deferred = Vec::new();
    for g in empire_groups {
        let name = g.name.clone();
        let value = g.parse(&root_path, text)?;
        let ctx = Ctx::with_header(&defs, root, format!("[empire.{name}]"));
        let raw: RawEmpire = ctx.de(value)?;
        let mut e = Empire::new(name.clone(), raw.display_name, &defs);
        for a in ctx.amounts(&raw.resource_stores)? {
            if defs.is_transient(a.resource) {
                return Err(ctx.err(format!(
                    "resource-stores: {} is transient and cannot be stored",
                    defs.resource(a.resource).key.name
                )));
            }
            e.storage[a.resource.idx()] += a.count;
        }
        for t in ctx.techs(&raw.technologies)? {
            e.techs[t.idx()] = true;
        }
        for r in &raw.current_research {
            let tech = ctx.tech(&r.technology)?;
            if e.techs[tech.idx()] || e.is_researching(tech) {
                return Err(ctx.err(format!(
                    "current-research: {} is already known or listed twice",
                    r.technology
                )));
            }
            let remaining = match &r.remaining {
                Some(list) => ctx.cost("current-research remaining", list)?,
                None => defs.research_cost(tech),
            };
            e.research.push(ResearchProject {
                tech,
                paid: paid_list(&ctx, &r.paid)?,
                remaining,
            });
        }
        e.capital = raw
            .capital
            .as_deref()
            .map(|c| {
                system_ids
                    .get(c)
                    .copied()
                    .ok_or_else(|| ctx.err(format!("unknown capital system '{c}'")))
            })
            .transpose()?;
        e.dominant_species = raw
            .dominant_species
            .as_deref()
            .map(|s| ctx.lookup(Kind::Species, s).map(SpeciesId))
            .transpose()?;
        e.ai = raw
            .ai
            .map(|a| {
                let traits = [
                    ("militant", a.militant),
                    ("expansionist", a.expansionist),
                    ("scientific", a.scientific),
                ];
                if let Some((name, v)) = traits.iter().find(|(_, v)| !(0.0..=1.0).contains(v)) {
                    return Err(ctx.err(format!("ai.{name} must be between 0 and 1 (got {v})")));
                }
                Ok(AiPersonality {
                    militant: a.militant,
                    expansionist: a.expansionist,
                    scientific: a.scientific,
                })
            })
            .transpose()?;
        e.accepts_migrants = raw.policy.accepts_migrants;
        e.market_access = raw.policy.market_access;
        e.color = raw.color;
        e.total_war = raw.policy.total_war;
        let d = raw.diplomacy;
        e.humiliated = d.humiliated;
        for c in &d.claims {
            let sys = *system_ids
                .get(c)
                .ok_or_else(|| ctx.err(format!("unknown system '{c}' in claims")))?;
            if !e.claims.contains(&sys) {
                e.claims.push(sys);
            }
        }
        for w in &d.wars {
            let war = *war_ids
                .get(&w.war)
                .ok_or_else(|| ctx.err(format!("unknown war '{}' in wars", w.war)))?;
            let side = Side::parse(&w.side).ok_or_else(|| {
                ctx.err(format!(
                    "war '{}': unknown side '{}' (expected offense or defense)",
                    w.war, w.side
                ))
            })?;
            if e.wars.iter().any(|(x, _)| *x == war) {
                return Err(ctx.err(format!("war '{}' is listed twice", w.war)));
            }
            e.wars.push((war, side));
        }
        for t in &d.truces {
            let truce = *truce_ids
                .get(t)
                .ok_or_else(|| ctx.err(format!("unknown truce '{t}' in truces")))?;
            if !e.truces.contains(&truce) {
                e.truces.push(truce);
            }
        }
        for a in &d.alliances {
            let alliance = *alliance_ids
                .get(a)
                .ok_or_else(|| ctx.err(format!("unknown alliance '{a}' in alliances")))?;
            if !e.alliances.contains(&alliance) {
                e.alliances.push(alliance);
            }
        }
        let id = world.add_empire(e);
        deferred.push(Deferred {
            id,
            name,
            shipbuilding: raw.current_shipbuilding,
            fleets: raw.fleets,
        });
    }

    // Systems are parsed and built in parallel, then added in file order.
    let built: Vec<Result<ParsedSystem>> = system_groups
        .into_par_iter()
        .map(|g| {
            let name = g.name.clone();
            let value = g.parse(&root_path, text)?;
            parse_system(&defs, root, &empire_ids, &system_ids, name, value)
        })
        .collect();
    let mut links = Vec::new();
    for b in built {
        let (sys, planets, linked, w) = b?;
        warnings.extend(w);
        let id = world.add_system(sys, planets);
        links.extend(linked.into_iter().map(|other| (id, other)));
    }
    for (a, b) in links {
        world.link(a, b);
    }

    for d in deferred {
        let ctx = Ctx::with_header(&defs, root, format!("[empire.{}]", d.name));
        for (fname, fvalue) in d.fleets {
            validate_name(&fname).map_err(|m| ctx.err(format!("fleet '{fname}': {m}")))?;
            let fctx = Ctx::with_header(&defs, root, format!("[empire.{}.fleets.{fname}]", d.name));
            let raw: RawFleet = fctx.de(fvalue)?;
            let system = *system_ids
                .get(&raw.system)
                .ok_or_else(|| fctx.err(format!("unknown system '{}'", raw.system)))?;
            let mut fleet = Fleet::new(fname.clone(), raw.display_name, d.id, system);
            fleet.in_combat = raw.in_combat;
            let n = raw.ships.len().max(1) as f64;
            let centre = [
                raw.ships.iter().map(|s| s.coords[0]).sum::<f64>() / n,
                raw.ships.iter().map(|s| s.coords[1]).sum::<f64>() / n,
            ];
            fleet.coordinates = centre;
            for s in raw.ships {
                let def = ShipId(fctx.lookup(Kind::Ship, &s.ship)?);
                let stats = world.ship_stats(d.id, def);
                let mut ship = Ship::new(def, stats);
                ship.hull = s.hull.unwrap_or(stats.max_hull).clamp(0.0, stats.max_hull);
                ship.shield = s
                    .shield
                    .unwrap_or(stats.max_shield)
                    .clamp(0.0, stats.max_shield);
                ship.offset = [s.coords[0] - centre[0], s.coords[1] - centre[1]];
                ship.jump_charge = s.jump_charge.clamp(0.0, 1.0);
                fleet.ships.push(ship);
            }
            let limit = world.empire(d.id).max_fleet_size(world.rules());
            if fleet.size(&defs) > limit {
                warnings.push(format!(
                    "{}: {} fleet slots exceed the limit of {limit}",
                    fctx.header,
                    fleet.size(&defs)
                ));
            }
            // Orders other than jumps refer to the system the fleet will be in by then.
            let mut at = system;
            for o in raw.orders {
                let order = parse_fleet_order(&fctx, &world, &system_ids, at, o)?;
                if let FleetOrder::Jump { system } = order {
                    at = system;
                }
                fleet.orders.push(order);
            }
            world.add_fleet(fleet);
        }
        for o in d.shipbuilding {
            let ship = ShipId(ctx.lookup(Kind::Ship, &o.ship)?);
            let planet = world
                .planet_by_name(&format!("{}/{}", o.system, o.planet))
                .ok_or_else(|| {
                    ctx.err(format!(
                        "current-shipbuilding: no planet '{}' in system '{}'",
                        o.planet, o.system
                    ))
                })?;
            let remaining = match &o.remaining {
                Some(list) => ctx.cost("current-shipbuilding remaining", list)?,
                None => defs.ship(ship).build_cost.clone(),
            };
            if world.planet(planet).controller() != Some(d.id) {
                warnings.push(format!(
                    "{}: ship under construction at {}/{}, which the empire does not control",
                    ctx.header, o.system, o.planet
                ));
            }
            world.empires[d.id.idx()].shipbuilding.push(ShipProject {
                ship,
                planet,
                paid: paid_list(&ctx, &o.paid)?,
                remaining,
            });
        }
    }
    for w in 0..world.wars.len() {
        world
            .check_war_goals(WarId::from_idx(w))
            .map_err(|m| invalid(format!("[game.wars.{}]: {m}", world.wars[w].name)))?;
    }
    Ok(world)
}

/// Normal resources already paid into a project (transient ones cannot be refunded, so they
/// are not tracked).
fn paid_list(ctx: &Ctx, raw: &[RawAmount]) -> Result<Vec<ResourceAmount>> {
    let paid = ctx.amounts(raw)?;
    if let Some(a) = paid.iter().find(|a| ctx.defs.is_transient(a.resource)) {
        return Err(ctx.err(format!(
            "`paid` only tracks normal resources, not {}",
            ctx.defs.resource(a.resource).key.name
        )));
    }
    Ok(paid)
}

/// Parses one fleet order; `at` is the system the fleet will be in when it carries it out.
/// A parsed `[system.x]`: the system, its planets, the systems it links to and any warnings.
type ParsedSystem = (System, Vec<Planet>, Vec<SystemId>, Vec<String>);

/// Parses one `[system.x]` with its planets: the system, its planets, the systems it links to
/// and any warnings.
fn parse_system(
    defs: &Defs,
    root: FileId,
    empire_ids: &HashMap<String, EmpireId>,
    system_ids: &HashMap<String, SystemId>,
    name: String,
    value: Value,
) -> Result<ParsedSystem> {
    let ctx = Ctx::with_header(defs, root, format!("[system.{name}]"));
    let raw: RawSystem = ctx.de(value)?;
    let mut sys = System::new(name.clone(), raw.display_name);
    sys.star_type = raw.star_type;
    sys.star_count = raw.star_count;
    sys.coordinates = raw.coords;
    sys.owner = raw
        .owner
        .as_deref()
        .map(|o| {
            empire_ids
                .get(o)
                .copied()
                .ok_or_else(|| ctx.err(format!("unknown empire '{o}' in owner")))
        })
        .transpose()?;
    sys.occupier = raw
        .occupier
        .as_deref()
        .map(|o| {
            empire_ids
                .get(o)
                .copied()
                .ok_or_else(|| ctx.err(format!("unknown empire '{o}' in occupier")))
        })
        .transpose()?;
    if sys.occupier.is_some() && (sys.owner.is_none() || sys.occupier == sys.owner) {
        return Err(ctx.err("`occupier` needs an owner other than the occupier"));
    }
    let links = raw
        .linked_systems
        .iter()
        .map(|l| {
            system_ids
                .get(l)
                .copied()
                .ok_or_else(|| ctx.err(format!("unknown linked system '{l}'")))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut warnings = Vec::new();
    let mut planets = Vec::with_capacity(raw.planets.len());
    for (pname, pvalue) in raw.planets {
        validate_name(&pname).map_err(|m| ctx.err(format!("planet '{pname}': {m}")))?;
        let pctx = PlanetCtx {
            header: format!("[system.{name}.planets.{pname}]"),
            empires: empire_ids,
        };
        planets.push(parse_planet(
            defs,
            root,
            &pctx,
            pname,
            pvalue,
            &mut warnings,
        )?);
    }
    Ok((sys, planets, links, warnings))
}

fn parse_fleet_order(
    ctx: &Ctx,
    world: &World,
    system_ids: &HashMap<String, SystemId>,
    at: SystemId,
    o: RawFleetOrder,
) -> Result<FleetOrder> {
    let unexpected = |what: bool, field: &str| -> Result<()> {
        if what {
            Err(ctx.err(format!("`{field}` is not used by {} orders", o.order)))
        } else {
            Ok(())
        }
    };
    Ok(match o.order.as_str() {
        "move" => {
            unexpected(o.system.is_some(), "system")?;
            unexpected(o.planet.is_some(), "planet")?;
            unexpected(o.tile.is_some(), "tile")?;
            FleetOrder::Move {
                coordinates: o
                    .coords
                    .ok_or_else(|| ctx.err("move orders need `coords`"))?,
            }
        }
        "jump" => {
            unexpected(o.coords.is_some(), "coords")?;
            unexpected(o.planet.is_some(), "planet")?;
            unexpected(o.tile.is_some(), "tile")?;
            let s = o
                .system
                .as_deref()
                .ok_or_else(|| ctx.err("jump orders need a `system`"))?;
            let system = *system_ids
                .get(s)
                .ok_or_else(|| ctx.err(format!("unknown system '{s}' in jump order")))?;
            if !world.system(at).links.contains(&system) {
                return Err(ctx.err(format!(
                    "jump order to '{s}', which has no hyperlane to '{}'",
                    world.system(at).name
                )));
            }
            FleetOrder::Jump { system }
        }
        "bombard" => {
            unexpected(o.coords.is_some(), "coords")?;
            unexpected(o.system.is_some(), "system")?;
            let p = o
                .planet
                .as_deref()
                .ok_or_else(|| ctx.err("bombard orders need a `planet`"))?;
            let sys = world.system(at);
            let planet = sys
                .planet_ids()
                .find(|id| world.planet(*id).name == p)
                .ok_or_else(|| {
                    ctx.err(format!(
                        "bombard order: no planet '{p}' in '{}' (where the fleet will be)",
                        sys.name
                    ))
                })?;
            FleetOrder::Bombard {
                planet,
                tile: o.tile,
            }
        }
        other => {
            return Err(ctx.err(format!(
                "unknown order '{other}' (expected move, jump or bombard)"
            )));
        }
    })
}

struct PlanetCtx<'a> {
    header: String,
    empires: &'a HashMap<String, EmpireId>,
}

fn parse_planet(
    defs: &Defs,
    root: FileId,
    pctx: &PlanetCtx,
    name: String,
    value: Value,
    warnings: &mut Vec<String>,
) -> Result<Planet> {
    let header = pctx.header.clone();
    let ctx = Ctx::with_header(defs, root, header.clone());
    let mut table = ctx.table(value)?;
    let tile_keys: Vec<String> = table
        .keys()
        .filter(|k| k.starts_with("tile-"))
        .cloned()
        .collect();
    let tiles: Vec<(String, Value)> = tile_keys
        .into_iter()
        .map(|k| {
            let v = table.remove(&k).unwrap();
            (k, v)
        })
        .collect();
    let raw: RawPlanet = ctx.de(Value::Table(table))?;
    let size = match raw.size {
        RawSize::Small => PlanetSize::Small,
        RawSize::Large => PlanetSize::Large,
    };
    let mut p = Planet::new(
        name,
        raw.display_name,
        size,
        raw.habitable,
        defs.resource_count(),
    );
    for pop in &raw.population {
        let s = SpeciesId(ctx.lookup(Kind::Species, &pop.species)?);
        p.add_pops(s, pop.count);
    }
    p.siege = raw.siege;
    p.coordinates = raw.coords;
    if let Some(t) = &raw.terraform {
        if p.habitable {
            return Err(ctx.err("a habitable planet cannot be terraformed"));
        }
        p.terraform = Some(Box::new(TerraformProject {
            paid: paid_list(&ctx, &t.paid)?,
            remaining: ctx.cost("terraform remaining", &t.remaining)?,
        }));
    }
    let empire = |e: &str| {
        pctx.empires
            .get(e)
            .copied()
            .ok_or_else(|| ctx.err(format!("unknown empire '{e}'")))
    };
    p.owner = raw.owner.as_deref().map(empire).transpose()?;
    p.occupier = raw.occupier.as_deref().map(empire).transpose()?;
    if p.occupier.is_some() && (p.owner.is_none() || p.occupier == p.owner) {
        return Err(ctx.err("`occupier` needs an owner other than the occupier"));
    }

    for (key, tvalue) in tiles {
        let tctx = Ctx::with_header(defs, root, format!("{header}.{key}"));
        let index: usize = key["tile-".len()..]
            .parse()
            .map_err(|_| tctx.err("tile tables are named tile-N with N a tile index"))?;
        if index >= p.tiles.len() {
            return Err(tctx.err(format!(
                "a {} planet only has tiles 0..={}",
                size.as_str(),
                p.tiles.len() - 1
            )));
        }
        let raw_tile: RawTile = tctx.de(tvalue)?;
        let tile = &mut p.tiles[index];
        tile.water = raw_tile.water;
        tile.resource_mults = raw_tile
            .resource_multipliers
            .iter()
            .map(|m| Ok((tctx.resource(&m.resource)?, m.mult)))
            .collect::<Result<_>>()?;
        if let Some(b) = raw_tile.building {
            let inst = parse_tile_building(&tctx, b)?;
            let def = defs.building(inst.def);
            if !p.habitable && !def.barren {
                warnings.push(format!(
                    "{}: {} is not tagged barren but sits on a barren planet",
                    tctx.header, def.key.name
                ));
            }
            if !def.fits_terrain(tile.water) {
                warnings.push(format!(
                    "{}: {} cannot stand on {}",
                    tctx.header,
                    def.key.name,
                    if tile.water { "water" } else { "land" },
                ));
            }
            tile.building = Some(inst);
        }
        if let Some(c) = raw_tile.construction {
            let remaining = tctx.cost("construction remaining", &c.remaining)?;
            let kind = match (c.building, c.upgrade, c.remove) {
                (Some(b), None, false) => {
                    if tile.building.is_some() {
                        return Err(
                            tctx.err("building construction on a tile that already has a building")
                        );
                    }
                    ProjectKind::Building(BuildingId(tctx.lookup(Kind::Building, &b)?))
                }
                (None, Some(u), false) => {
                    let Some(inst) = &tile.building else {
                        return Err(tctx.err("upgrade construction on a tile without a building"));
                    };
                    ProjectKind::Upgrade(resolve_tile_upgrade(&tctx, inst.def, &u)?)
                }
                (None, None, true) => {
                    if tile.building.is_none() {
                        return Err(tctx.err("removal on a tile without a building"));
                    }
                    ProjectKind::Remove
                }
                _ => {
                    return Err(tctx.err(
                        "construction needs exactly one of `building`, `upgrade` or `remove = true`",
                    ));
                }
            };
            tile.construction = Some(Box::new(Construction {
                kind,
                paid: paid_list(&tctx, &c.paid)?,
                remaining,
                empire: c.empire.as_deref().map(empire).transpose()?,
            }));
        }
    }
    Ok(p)
}

fn resolve_tile_upgrade(ctx: &Ctx, building: BuildingId, u: &str) -> Result<UpgradeId> {
    let def = ctx.defs.building(building);
    let id = UpgradeId(ctx.lookup(Kind::Upgrade, u)?);
    if !def.available_upgrades.contains(&id) {
        return Err(ctx.err(format!(
            "upgrade '{u}' does not apply to building '{}'",
            def.key.name
        )));
    }
    Ok(id)
}

fn parse_tile_building(ctx: &Ctx, value: Value) -> Result<BuildingInstance> {
    match value {
        Value::String(s) => Ok(BuildingInstance::new(BuildingId(
            ctx.lookup(Kind::Building, &s)?,
        ))),
        v @ Value::Table(_) => {
            let raw: RawTileBuilding = ctx.de(v)?;
            let def = BuildingId(ctx.lookup(Kind::Building, &raw.building)?);
            let mut inst = BuildingInstance::new(def);
            inst.upgrades = raw
                .upgrades
                .iter()
                .map(|u| resolve_tile_upgrade(ctx, def, u))
                .collect::<Result<_>>()?;
            for u in &inst.upgrades {
                for need in &ctx.defs.upgrade(*u).after_upgrades {
                    if !inst.upgrades.contains(need) {
                        return Err(ctx.err(format!(
                            "upgrade '{}' requires '{}'",
                            ctx.defs.upgrade(*u).key.name,
                            ctx.defs.upgrade(*need).key.name
                        )));
                    }
                }
            }
            let max = ctx.defs.building(def).max_workers.unwrap_or(0);
            inst.workers = raw.workers.min(max);
            inst.job_cap = raw.job_cap;
            inst.pinned = raw.pinned;
            inst.enabled = raw.enabled;
            inst.damage = raw.damage.clamp(0.0, 1.0);
            Ok(inst)
        }
        _ => Err(ctx.err("`building` must be a building reference or a table")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_header_position() {
        let p = Path::new("x.toml");
        assert!(check_import_header("# hi\n[import]\na = \"b\"\n[resource.x]\n", p).is_ok());
        assert!(check_import_header("[resource.x]\ndisplay-name = \"x\"\n[import]\n", p).is_err());
        assert!(check_import_header("[resource.x]\n", p).is_ok());
    }
}
