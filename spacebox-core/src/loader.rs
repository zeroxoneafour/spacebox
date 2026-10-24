//! TOML loading, import handling and reference resolution.
//!
//! # Files and imports
//!
//! A file may start with a single `[import]` table mapping aliases to paths relative to the
//! importing file. Imports are loaded transitively (each file once, deduplicated by canonical
//! path, cycles allowed), but *names* do not leak through: a file can only reference its own
//! definitions (`"name"`) and those of files it imports directly (`"alias.name"`).
//!
//! # Typed references
//!
//! The slot a reference appears in determines which table it is looked up in. For example
//! `output_resources = [{ resource = "res.alloy", count = 1 }]` looks for `[resource.alloy]` in
//! the file imported as `res`, and `job = "jobs.metallurgist"` looks for `[job.metallurgist]`.
//!
//! Building upgrade names in a tile (`upgrades = ["faster-forge"]`) are resolved relative to the
//! file that defines the building when unqualified, matching how `apply_to` is written.
//!
//! # Assets vs. state
//!
//! Asset tables (`resource`, `job`, `building`, `building-upgrade`, `technology`, `ruleset`) are
//! collected from every loaded file. Game state (`game`, `empire`, `system`) is only read from
//! the root file, so a save can import a scenario file to reuse its assets without duplicating
//! the scenario's systems.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use toml::{Table, Value};

use crate::defs::*;
use crate::error::{Error, Result};
use crate::geometry::PlanetSize;
use crate::ids::*;
use crate::raw::*;
use crate::world::*;

const STATE_TABLES: [&str; 3] = ["game", "empire", "system"];

/// Loads only the asset definitions reachable from `path`.
pub fn load_assets(path: impl AsRef<Path>) -> Result<(Arc<Defs>, Vec<String>)> {
    let mut loader = Loader::default();
    let root = loader.load_file(path.as_ref())?;
    let (mut defs, _state) = loader.build_defs(root)?;
    if defs.rulesets.is_empty() {
        defs.rulesets.push(Ruleset::builtin());
    }
    Ok((Arc::new(defs), loader.warnings))
}

/// Loads a save/scenario file: all reachable assets plus the root file's game state.
pub fn load_world(path: impl AsRef<Path>) -> Result<(World, Vec<String>)> {
    let mut loader = Loader::default();
    let root = loader.load_file(path.as_ref())?;
    let (defs, state) = loader.build_defs(root)?;
    let world = build_world(defs, root, state, &mut loader.warnings)?;
    Ok((world, loader.warnings))
}

#[derive(Default)]
struct Loader {
    files: Vec<SourceFile>,
    tables: Vec<Table>,
    by_path: HashMap<PathBuf, FileId>,
    warnings: Vec<String>,
}

impl Loader {
    fn load_file(&mut self, path: &Path) -> Result<FileId> {
        let canon = fs::canonicalize(path).map_err(|e| Error::io(path, e))?;
        if let Some(id) = self.by_path.get(&canon) {
            return Ok(*id);
        }
        let text = fs::read_to_string(&canon).map_err(|e| Error::io(&canon, e))?;
        check_import_header(&text, &canon)?;
        let mut table: Table = text.parse().map_err(|e: toml::de::Error| Error::Parse {
            path: canon.clone(),
            message: e.to_string(),
        })?;

        let id = FileId::from_idx(self.files.len());
        self.files.push(SourceFile {
            path: canon.clone(),
            imports: Vec::new(),
        });
        self.tables.push(Table::new());
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
            let known =
                Kind::ALL.iter().any(|k| k.table() == key) || STATE_TABLES.contains(&key.as_str());
            if !known {
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
            for kind in Kind::ALL {
                let Some(value) = table.remove(kind.table()) else {
                    continue;
                };
                let Value::Table(entries) = value else {
                    return Err(Error::invalid(
                        path,
                        format!("[{}] must be a table of definitions", kind.table()),
                    ));
                };
                for (name, def) in entries {
                    validate_name(&name).map_err(|m| {
                        Error::invalid(path, format!("[{}.{name}]: {m}", kind.table()))
                    })?;
                    let list = &mut decls[kind as usize];
                    names[fi][kind as usize].insert(name.clone(), list.len() as u32);
                    list.push((file, name, def));
                }
            }
            let state = std::mem::take(table);
            if file == root {
                root_state = state;
            } else if !state.is_empty() {
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
            buildings: Vec::new(),
            upgrades: Vec::new(),
            technologies: Vec::new(),
            rulesets: Vec::new(),
            ships: Vec::new(),
            weapons: Vec::new(),
            outfits: Vec::new(),
            classes: Vec::new(),
            names,
        };
        let [
            resources,
            jobs,
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
            let def = ResourceDef {
                key: DefKey { file, name },
                display_name: raw.display_name,
                description: raw.description,
                image: raw.image.map(|i| ctx.relative(&i)),
                base_value: raw.base_value,
                tradeable: raw.tradeable,
                stockpile: raw.stockpile,
                storage_cap: raw.storage_cap,
                on_demand: raw.on_demand,
            };
            defs.resources.push(def);
        }
        for (file, name, value) in jobs {
            let ctx = Ctx::new(&defs, file, Kind::Job, &name);
            let raw: RawJob = ctx.de(value)?;
            let upkeep = ctx.amounts(&raw.upkeep)?;
            defs.jobs.push(JobDef {
                key: DefKey { file, name },
                display_name: raw.display_name,
                description: raw.description,
                appeal: raw.appeal,
                upkeep,
            });
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
        // Ships derive their stats from weapons and outfits, so those come first.
        for (file, name, value) in weapons {
            let ctx = Ctx::new(&defs, file, Kind::Weapon, &name);
            let raw: RawWeapon = ctx.de(value)?;
            if raw.fire_rate <= 0.0 || raw.range < 0.0 {
                return Err(ctx.err("fire_rate must be positive and range non-negative"));
            }
            let def = WeaponDef {
                display_name: raw.display_name.unwrap_or_else(|| name.clone()),
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
            let raw: RawOutfit = ctx.de(value)?;
            if raw.fire_rate_mult <= 0.0 {
                return Err(ctx.err("fire_rate_mult must be positive"));
            }
            let def = OutfitDef {
                display_name: raw.display_name.unwrap_or_else(|| name.clone()),
                description: raw.description,
                build_cost: ctx.amounts(&raw.build_cost)?,
                upkeep: ctx.amounts(&raw.upkeep_cost)?,
                hull: raw.hull,
                shield: raw.shield,
                shield_regen: raw.shield_regen,
                hull_mult: raw.hull_mult,
                shield_mult: raw.shield_mult,
                shield_regen_mult: raw.shield_regen_mult,
                speed_mult: raw.speed_mult,
                hyperjump_mult: raw.hyperjump_mult,
                fire_rate_mult: raw.fire_rate_mult,
                damage_mult: raw.damage_mult,
                range_mult: raw.range_mult,
                key: DefKey { file, name },
            };
            defs.outfits.push(def);
        }
        for (file, name, value) in ships {
            let def = parse_ship(&defs, file, name, value)?;
            defs.ships.push(def);
        }

        compute_upgrade_availability(&mut defs)?;
        Ok((defs, root_state))
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

    fn relative(&self, p: &str) -> PathBuf {
        let path = &self.defs.files[self.file.idx()].path;
        path.parent().unwrap_or(Path::new(".")).join(p)
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

    fn amounts(&self, raw: &[RawAmount]) -> Result<Vec<ResourceAmount>> {
        raw.iter()
            .map(|a| {
                if !a.count.is_finite() {
                    return Err(self.err(format!("count for '{}' must be finite", a.resource)));
                }
                Ok(ResourceAmount {
                    resource: self.resource(&a.resource)?,
                    count: a.count,
                })
            })
            .collect()
    }

    fn modifier(&self, raw: RawModifier) -> Result<Modifier> {
        Ok(Modifier {
            input_mult: raw.input_mult,
            output_mult: raw.output_mult,
            upkeep_mult: raw.upkeep_mult,
            housing_mult: raw.housing_mult,
            amenities_mult: raw.amenities_mult,
            inputs: self.amounts(&raw.input_resources)?,
            outputs: self.amounts(&raw.output_resources)?,
            upkeep: self.amounts(&raw.upkeep_resources)?,
            housing: raw.housing,
            amenities: raw.amenities,
        })
    }
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

fn intern_class(defs: &mut Defs, name: &str) -> ClassId {
    match defs.class_id(name) {
        Some(id) => id,
        None => {
            defs.classes.push(name.to_string());
            ClassId::from_idx(defs.classes.len() - 1)
        }
    }
}

/// Resolves an `apply_to`/adjacency entry: a building reference if one exists, otherwise a
/// building class name.
fn resolve_target(defs: &mut Defs, file: FileId, header: &str, s: &str) -> Result<BuildingTarget> {
    match defs.lookup(file, Kind::Building, s) {
        Ok(id) => Ok(BuildingTarget::Building(BuildingId(id))),
        Err(_) if !s.contains('.') => Ok(BuildingTarget::Class(intern_class(defs, s))),
        Err(m) => Err(Ctx::with_header(defs, file, header.to_string()).err(m)),
    }
}

fn parse_targeted(
    defs: &mut Defs,
    file: FileId,
    header: &str,
    mut table: Table,
) -> Result<TargetedModifier> {
    let head = take_keys(&mut table, TARGET_KEYS);
    let (target_raw, modifier) = {
        let ctx = Ctx::with_header(defs, file, header.to_string());
        let target: RawTarget = ctx.de(Value::Table(head))?;
        let raw: RawModifier = ctx.de(Value::Table(table))?;
        (target, ctx.modifier(raw)?)
    };
    let target = match (target_raw.building, target_raw.building_class) {
        (Some(b), None) => resolve_target(defs, file, header, &b)?,
        (None, Some(c)) => BuildingTarget::Class(intern_class(defs, &c)),
        _ => {
            return Err(Ctx::with_header(defs, file, header.to_string())
                .err("modifier needs exactly one of `building` or `building_class`"));
        }
    };
    Ok(TargetedModifier { target, modifier })
}

fn parse_building(
    defs: &mut Defs,
    file: FileId,
    name: String,
    value: Value,
) -> Result<BuildingDef> {
    let header = format!("[building.{name}]");
    let raw: RawBuilding = Ctx::with_header(defs, file, header.clone()).de(value)?;
    let classes: Vec<ClassId> = raw
        .building_class
        .iter()
        .map(|c| intern_class(defs, c))
        .collect();
    let has = |class: &str| raw.building_class.iter().any(|c| c == class);
    let (barren, blocker, land, water, space) = (
        has(BARREN_CLASS),
        has(BLOCKER_CLASS),
        has(LAND_CLASS),
        has(WATER_CLASS),
        has(SPACE_CLASS),
    );
    let mut adjacency = Vec::new();
    for t in raw.adjacency_bonus {
        adjacency.push(parse_targeted(defs, file, &header, t)?);
    }
    let ctx = Ctx::with_header(defs, file, header);
    if barren && raw.max_workers > 0 {
        return Err(ctx.err("buildings with the \"barren\" class must have max_workers = 0"));
    }
    if land && water {
        return Err(ctx.err("a building cannot have both the \"land\" and \"water\" classes"));
    }
    let job = raw
        .job
        .as_deref()
        .map(|j| ctx.lookup(Kind::Job, j).map(JobId))
        .transpose()?;
    if raw.max_workers > 0 && job.is_none() {
        return Err(ctx.err("buildings with max_workers > 0 need a `job`"));
    }
    Ok(BuildingDef {
        display_name: raw.display_name.unwrap_or_else(|| name.clone()),
        description: raw.description,
        image: raw.image.map(|i| ctx.relative(&i)),
        max_workers: raw.max_workers,
        job,
        classes,
        barren,
        blocker,
        land,
        water,
        space,
        upkeep: ctx.amounts(&raw.upkeep_resources)?,
        inputs: ctx.amounts(&raw.input_resources)?,
        outputs: ctx.amounts(&raw.output_resources)?,
        housing: raw.housing,
        amenities: raw.amenities,
        adjacency,
        build_cost: ctx.amounts(&raw.build_cost)?,
        build_days: raw.build_days,
        requires_tech: ctx.techs(&raw.requires_tech)?,
        // Blockers occur naturally; they are never constructed.
        buildable: raw.buildable && !blocker,
        max_per_planet: raw.max_per_planet,
        hp: raw.hp,
        remove_cost: ctx.amounts(&raw.remove_cost)?,
        available_upgrades: Vec::new(),
        key: DefKey { file, name },
    })
}

fn parse_upgrade(defs: &mut Defs, file: FileId, name: String, value: Value) -> Result<UpgradeDef> {
    let header = format!("[building-upgrade.{name}]");
    let ctx = Ctx::with_header(defs, file, header.clone());
    let Value::Table(mut table) = value else {
        return Err(ctx.err("must be a table"));
    };
    let head: RawUpgradeHead = ctx.de(Value::Table(take_keys(&mut table, UPGRADE_HEAD_KEYS)))?;
    let modifier = ctx.modifier(ctx.de(Value::Table(table))?)?;
    let cost = ctx.amounts(&head.cost)?;
    let requires_tech = ctx.techs(&head.requires_tech)?;
    let needs_upgrades = head
        .needs_upgrades
        .iter()
        .map(|u| ctx.lookup(Kind::Upgrade, u).map(UpgradeId))
        .collect::<Result<Vec<_>>>()?;
    if head.apply_to.is_empty() {
        return Err(ctx.err("`apply_to` must list at least one building or building class"));
    }
    let mut apply_to = Vec::new();
    for t in &head.apply_to {
        apply_to.push(resolve_target(defs, file, &header, t)?);
    }
    Ok(UpgradeDef {
        display_name: head.display_name.unwrap_or_else(|| name.clone()),
        description: head.description,
        apply_to,
        needs_upgrades,
        modifier,
        cost,
        build_days: head.build_days,
        requires_tech,
        key: DefKey { file, name },
    })
}

fn parse_tech(defs: &mut Defs, file: FileId, name: String, value: Value) -> Result<TechDef> {
    let header = format!("[technology.{name}]");
    let raw: RawTech = Ctx::with_header(defs, file, header.clone()).de(value)?;
    let mut modifiers = Vec::new();
    for m in raw.modifiers {
        modifiers.push(parse_targeted(defs, file, &header, m)?);
    }
    let ctx = Ctx::with_header(defs, file, header);
    if raw.cost.is_nan() || raw.cost < 0.0 {
        return Err(ctx.err("cost must be non-negative"));
    }
    Ok(TechDef {
        display_name: raw.display_name,
        description: raw.description,
        category: raw.category,
        cost: raw.cost,
        prerequisites: ctx.techs(&raw.prerequisites)?,
        modifiers,
        pop_growth_mult: raw.pop_growth_mult,
        stability: raw.stability,
        key: DefKey { file, name },
    })
}

fn parse_ruleset(defs: &Defs, file: FileId, name: String, value: Value) -> Result<Ruleset> {
    let ctx = Ctx::new(defs, file, Kind::Ruleset, &name);
    let raw: RawRuleset = ctx.de(value)?;
    let opt_res = |r: &Option<String>| r.as_deref().map(|r| ctx.resource(r)).transpose();
    let terraform = |t: &Option<RawTerraform>| -> Result<Option<TerraformRule>> {
        t.as_ref()
            .map(|t| {
                Ok(TerraformRule {
                    requires_tech: t
                        .requires_tech
                        .as_deref()
                        .map(|r| ctx.tech(r))
                        .transpose()?,
                    days: t.days,
                    cost: ctx.amounts(&t.cost)?,
                })
            })
            .transpose()
    };
    let b = Ruleset::builtin();
    Ok(Ruleset {
        builtin: false,
        pop_upkeep: ctx.amounts(&raw.pop_upkeep)?,
        starvation_resource: opt_res(&raw.starvation_resource)?,
        comfort_resource: opt_res(&raw.comfort_resource)?,
        research_resource: opt_res(&raw.research_resource)?,
        currency: opt_res(&raw.currency)?,
        pop_growth_rate: raw.pop_growth_rate.unwrap_or(b.pop_growth_rate),
        min_pop_growth: raw.min_pop_growth.unwrap_or(b.min_pop_growth),
        starvation_rate: raw.starvation_rate.unwrap_or(b.starvation_rate),
        migration_rate: raw.migration_rate.unwrap_or(b.migration_rate),
        base_housing_large: raw.base_housing_large.unwrap_or(b.base_housing_large),
        base_housing_small: raw.base_housing_small.unwrap_or(b.base_housing_small),
        base_amenities: raw.base_amenities.unwrap_or(b.base_amenities),
        amenities_per_pop: raw.amenities_per_pop.unwrap_or(b.amenities_per_pop),
        base_stability: raw.base_stability.unwrap_or(b.base_stability),
        stability_output_factor: raw
            .stability_output_factor
            .unwrap_or(b.stability_output_factor),
        unemployment_stability_penalty: raw
            .unemployment_stability_penalty
            .unwrap_or(b.unemployment_stability_penalty),
        homelessness_stability_penalty: raw
            .homelessness_stability_penalty
            .unwrap_or(b.homelessness_stability_penalty),
        comfort_stability_penalty: raw
            .comfort_stability_penalty
            .unwrap_or(b.comfort_stability_penalty),
        crisis_stability_penalty: raw
            .crisis_stability_penalty
            .unwrap_or(b.crisis_stability_penalty),
        logistics_loss: raw.logistics_loss.unwrap_or(b.logistics_loss),
        market_fee: raw.market_fee.unwrap_or(b.market_fee),
        market_reserve: raw.market_reserve.unwrap_or(b.market_reserve),
        scarcity_appeal: raw.scarcity_appeal.unwrap_or(b.scarcity_appeal),
        scarcity_target_days: raw.scarcity_target_days.unwrap_or(b.scarcity_target_days),
        ai_interval_days: raw.ai_interval_days.unwrap_or(b.ai_interval_days).max(1),
        terraform_large: terraform(&raw.terraforming.large)?,
        terraform_small: terraform(&raw.terraforming.small)?,
        empire_income: ctx.amounts(&raw.empire_income)?,
        expansion_resource: opt_res(&raw.expansion_resource)?,
        expansion_base_cost: raw.expansion_base_cost.unwrap_or(b.expansion_base_cost),
        expansion_base_days: raw.expansion_base_days.unwrap_or(b.expansion_base_days),
        hyperjump_days: raw.hyperjump_days.unwrap_or(b.hyperjump_days).max(0.0),
        combat_subticks: raw.combat_subticks.unwrap_or(b.combat_subticks).max(1),
        arrival_radius: raw.arrival_radius.unwrap_or(b.arrival_radius),
        ship_repair_rate: raw.ship_repair_rate.unwrap_or(b.ship_repair_rate),
        building_hp: raw.building_hp.unwrap_or(b.building_hp).max(1.0),
        building_disable_damage: raw
            .building_disable_damage
            .unwrap_or(b.building_disable_damage),
        building_repair_rate: raw.building_repair_rate.unwrap_or(b.building_repair_rate),
        bombardment_amenities_penalty: raw
            .bombardment_amenities_penalty
            .unwrap_or(b.bombardment_amenities_penalty),
        surrender_resistance: raw.surrender_resistance.unwrap_or(b.surrender_resistance),
        siege_decay: raw.siege_decay.unwrap_or(b.siege_decay),
        truce_days: raw.truce_days.unwrap_or(b.truce_days),
        key: DefKey { file, name },
    })
}

/// Merges `items` into `list`, summing counts per resource.
fn add_amounts(list: &mut Vec<ResourceAmount>, items: &[ResourceAmount]) {
    for a in items {
        match list.iter_mut().find(|x| x.resource == a.resource) {
            Some(x) => x.count += a.count,
            None => list.push(*a),
        }
    }
}

/// Parses a ship design and derives its combat stats from its weapons and outfits.
fn parse_ship(defs: &Defs, file: FileId, name: String, value: Value) -> Result<ShipDef> {
    let ctx = Ctx::new(defs, file, Kind::Ship, &name);
    let raw: RawShip = ctx.de(value)?;
    if raw.hull <= 0.0 || raw.base_speed < 0.0 {
        return Err(ctx.err("hull must be positive and base_speed non-negative"));
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

    let mut build_cost = ctx.amounts(&raw.build_cost)?;
    let mut upkeep = ctx.amounts(&raw.upkeep_cost)?;
    let (mut hull, mut shield, mut regen) = (raw.hull, raw.shield, raw.shield_regen);
    let (mut hull_m, mut shield_m, mut regen_m, mut speed_m, mut jump_m) =
        (1.0, 1.0, 1.0, 1.0, raw.hyperjump_mult);
    let (mut rate_m, mut dmg_m, mut range_m) = (1.0, 1.0, 1.0);
    for w in &weapons {
        add_amounts(&mut build_cost, &defs.weapon(*w).build_cost);
    }
    for o in &outfits {
        let o = defs.outfit(*o);
        add_amounts(&mut build_cost, &o.build_cost);
        add_amounts(&mut upkeep, &o.upkeep);
        hull += o.hull;
        shield += o.shield;
        regen += o.shield_regen;
        hull_m *= o.hull_mult;
        shield_m *= o.shield_mult;
        regen_m *= o.shield_regen_mult;
        speed_m *= o.speed_mult;
        jump_m *= o.hyperjump_mult;
        rate_m *= o.fire_rate_mult;
        dmg_m *= o.damage_mult;
        range_m *= o.range_mult;
    }
    let armament: Vec<Armament> = weapons
        .iter()
        .map(|w| {
            let d = defs.weapon(*w);
            Armament {
                weapon: *w,
                shield_damage: d.shield_damage * dmg_m,
                hull_damage: d.hull_damage * dmg_m,
                cooldown: d.fire_rate / rate_m,
                range: d.range * range_m,
            }
        })
        .collect();
    Ok(ShipDef {
        display_name: raw.display_name.unwrap_or_else(|| name.clone()),
        description: raw.description,
        requires_tech: ctx.techs(&raw.requires_tech)?,
        build_cost,
        upkeep,
        max_hull: hull * hull_m,
        max_shield: shield * shield_m,
        shield_regen: regen * regen_m,
        speed: raw.base_speed * speed_m,
        hyperjump_mult: jump_m,
        max_range: armament.iter().map(|a| a.range).fold(0.0, f64::max),
        armament,
        weapons,
        outfits,
        key: DefKey { file, name },
    })
}

/// An upgrade is available for a building if `apply_to` matches it and every upgrade in
/// `needs_upgrades` is (recursively) available for it too.
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
                    "upgrade dependency cycle involving [building-upgrade.{}]",
                    defs.upgrades[u].key.name
                ));
            }
            State::Unknown => {}
        }
        state[u] = State::Visiting;
        let up = &defs.upgrades[u];
        let mut ok = up.apply_to.iter().any(|t| defs.target_matches(*t, b));
        for dep in &up.needs_upgrades {
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

fn build_world(
    mut defs: Defs,
    root: FileId,
    mut state: Table,
    warnings: &mut Vec<String>,
) -> Result<World> {
    let root_path = defs.files[root.idx()].path.clone();
    let invalid = |m: String| Error::invalid(&root_path, m);
    let as_table = |v: Option<Value>, what: &str| -> Result<Table> {
        match v {
            None => Ok(Table::new()),
            Some(Value::Table(t)) => Ok(t),
            Some(_) => Err(Error::invalid(
                &root_path,
                format!("[{what}] must be a table"),
            )),
        }
    };

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
            defs.rulesets.push(Ruleset::builtin());
            RulesetId(0)
        }
        None => {
            return Err(invalid(
                "several rulesets are loaded; choose one with [game] ruleset = \"...\"".into(),
            ));
        }
    };

    let empire_tables = as_table(state.remove("empire"), "empire")?;
    let system_tables = as_table(state.remove("system"), "system")?;
    let empire_ids: HashMap<String, EmpireId> = empire_tables
        .keys()
        .enumerate()
        .map(|(i, k)| (k.clone(), EmpireId::from_idx(i)))
        .collect();
    let system_ids: HashMap<String, SystemId> = system_tables
        .keys()
        .enumerate()
        .map(|(i, k)| (k.clone(), SystemId::from_idx(i)))
        .collect();
    for name in empire_tables.keys().chain(system_tables.keys()) {
        validate_name(name).map_err(|m| invalid(format!("'{name}': {m}")))?;
    }

    let defs = Arc::new(defs);
    let mut world = World::new(defs.clone(), ruleset);
    world.day = game.day;
    world.seed = game.seed;
    world.root_file = Some(root);

    // Fleets, wars and ship orders refer to systems and planets, so they are resolved after
    // the systems are built.
    let mut deferred = Vec::new();
    for (name, value) in empire_tables {
        let ctx = Ctx::with_header(&defs, root, format!("[empire.{name}]"));
        let raw: RawEmpire = ctx.de(value)?;
        let mut e = Empire::new(name.clone(), raw.display_name, &defs);
        for a in ctx.amounts(&raw.resource_stores)? {
            e.storage[a.resource.idx()] += a.count;
        }
        for t in ctx.techs(&raw.technologies)? {
            e.techs[t.idx()] = true;
        }
        e.researching = raw
            .researching
            .as_deref()
            .map(|t| ctx.tech(t))
            .transpose()?;
        e.research_progress = raw.research_progress;
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
        e.accepts_migrants = raw.accepts_migrants;
        e.market_access = raw.market_access;
        e.color = raw.color;
        for c in raw.crises {
            e.crises.push(Crisis {
                resource: ctx.resource(&c.resource)?,
                since_day: c.since_day,
                severity: c.severity,
            });
        }
        let empire = |name: &str, what: &str| {
            empire_ids
                .get(name)
                .copied()
                .ok_or_else(|| ctx.err(format!("unknown empire '{name}' in {what}")))
        };
        for w in &raw.wars {
            e.wars.push(War {
                enemy: empire(&w.empire, "wars")?,
                since_day: w.since_day,
            });
        }
        for t in &raw.truces {
            e.truces.push(Truce {
                with: empire(&t.empire, "truces")?,
                until_day: t.until_day,
            });
        }
        let id = world.add_empire(e);
        deferred.push((id, name, raw.ship_queue, raw.fleets));
    }

    let mut links = Vec::new();
    for (name, value) in system_tables {
        let ctx = Ctx::with_header(&defs, root, format!("[system.{name}]"));
        let raw: RawSystem = ctx.de(value)?;
        let mut sys = System::new(name.clone(), raw.display_name);
        sys.star_type = raw.star_type;
        sys.star_count = raw.star_count;
        sys.coordinates = [raw.coordinates.x, raw.coordinates.y];
        sys.owner = raw
            .owned_by
            .as_deref()
            .map(|o| {
                empire_ids
                    .get(o)
                    .copied()
                    .ok_or_else(|| ctx.err(format!("unknown empire '{o}' in owned_by")))
            })
            .transpose()?;
        let id = system_ids[&name];
        for l in &raw.linked_systems {
            let other = system_ids
                .get(l)
                .copied()
                .ok_or_else(|| ctx.err(format!("unknown linked system '{l}'")))?;
            links.push((id, other));
        }
        let mut planets = Vec::new();
        for (i, (pname, pvalue)) in raw.planets.into_iter().enumerate() {
            validate_name(&pname).map_err(|m| ctx.err(format!("planet '{pname}': {m}")))?;
            let pctx = PlanetCtx {
                header: format!("[system.{name}.planets.{pname}]"),
                index: i,
                system_owner: sys.owner,
                empires: &empire_ids,
            };
            planets.push(parse_planet(&defs, root, &pctx, pname, pvalue, warnings)?);
        }
        world.add_system(sys, planets);
    }
    for (a, b) in links {
        world.link(a, b);
    }

    for (eid, ename, queue, fleets) in deferred {
        let header = format!("[empire.{ename}]");
        let ctx = Ctx::with_header(&defs, root, header.clone());
        let mut fleet_ids = HashMap::new();
        for (fname, fvalue) in fleets {
            validate_name(&fname).map_err(|m| ctx.err(format!("fleet '{fname}': {m}")))?;
            let fctx = Ctx::with_header(&defs, root, format!("[empire.{ename}.fleets.{fname}]"));
            let raw: RawFleet = fctx.de(fvalue)?;
            let system = match raw.system.as_deref() {
                Some(s) => *system_ids
                    .get(s)
                    .ok_or_else(|| fctx.err(format!("unknown system '{s}'")))?,
                None => world.empire(eid).capital.ok_or_else(|| {
                    fctx.err("fleet needs a `system` (the empire has no capital)")
                })?,
            };
            let mut fleet = Fleet::new(fname.clone(), raw.display_name, eid, system);
            fleet.coordinates = raw.coordinates.map_or([0.0, 0.0], |c| [c.x, c.y]);
            fleet.jump_charge = raw.jump_charge;
            for s in raw.ships {
                let def = ShipId(fctx.lookup(Kind::Ship, &s.ship)?);
                let mut ship = Ship::new(def, &defs);
                let d = defs.ship(def);
                ship.hull = s.hull.unwrap_or(d.max_hull).clamp(0.0, d.max_hull);
                ship.shield = s.shield.unwrap_or(d.max_shield).clamp(0.0, d.max_shield);
                fleet.ships.push(ship);
            }
            if let Some(o) = raw.order {
                fleet.order = parse_fleet_order(&fctx, &world, &system_ids, o)?;
            }
            let id = world.add_fleet(fleet);
            fleet_ids.insert(fname, id);
        }
        for o in queue {
            let ship = ShipId(ctx.lookup(Kind::Ship, &o.ship)?);
            let planet = world
                .planet_by_name(&o.planet)
                .ok_or_else(|| ctx.err(format!("unknown shipyard planet '{}'", o.planet)))?;
            let remaining = match o.remaining {
                Some(list) => ctx
                    .amounts(&list)?
                    .into_iter()
                    .map(|a| (a.resource, a.count))
                    .collect(),
                None => on_demand_cost(&defs, ship),
            };
            let fleet = o
                .fleet
                .map(|f| {
                    fleet_ids
                        .get(&f)
                        .copied()
                        .ok_or_else(|| ctx.err(format!("unknown fleet '{f}' in ship_queue")))
                })
                .transpose()?;
            world.empires[eid.idx()].ship_queue.push(ShipOrder {
                ship,
                planet,
                remaining,
                fleet,
            });
        }
    }
    // Keep wars symmetric even if only one side lists them.
    for a in 0..world.empires.len() {
        for w in world.empires[a].wars.clone() {
            let other = &mut world.empires[w.enemy.idx()];
            if !other.at_war_with(EmpireId::from_idx(a)) {
                other.wars.push(War {
                    enemy: EmpireId::from_idx(a),
                    since_day: w.since_day,
                });
            }
        }
    }
    Ok(world)
}

/// The on-demand part of a ship's cost (e.g. shipbuilding), which shipyards supply over time.
pub(crate) fn on_demand_cost(defs: &Defs, ship: ShipId) -> Vec<(ResourceId, f64)> {
    defs.ship(ship)
        .build_cost
        .iter()
        .filter(|a| defs.resource(a.resource).on_demand)
        .map(|a| (a.resource, a.count))
        .collect()
}

fn parse_fleet_order(
    ctx: &Ctx,
    world: &World,
    system_ids: &HashMap<String, SystemId>,
    o: RawFleetOrder,
) -> Result<FleetOrder> {
    let system = |s: &Option<String>| -> Result<SystemId> {
        let s = s
            .as_deref()
            .ok_or_else(|| ctx.err("move orders need a `system`"))?;
        system_ids
            .get(s)
            .copied()
            .ok_or_else(|| ctx.err(format!("unknown system '{s}' in order")))
    };
    Ok(match o.kind.as_str() {
        "idle" => FleetOrder::Idle,
        "move" => FleetOrder::Move {
            system: system(&o.system)?,
            coordinates: o.coordinates.map(|c| [c.x, c.y]),
        },
        "bombard" => {
            let p = o
                .planet
                .as_deref()
                .ok_or_else(|| ctx.err("bombard orders need a `planet`"))?;
            FleetOrder::Bombard {
                planet: world
                    .planet_by_name(p)
                    .ok_or_else(|| ctx.err(format!("unknown planet '{p}' in order")))?,
                tile: o.tile,
            }
        }
        other => return Err(ctx.err(format!("unknown order type '{other}'"))),
    })
}

struct PlanetCtx<'a> {
    header: String,
    /// Position within the system, used to place planets without coordinates.
    index: usize,
    system_owner: Option<EmpireId>,
    empires: &'a HashMap<String, EmpireId>,
}

/// Default planet placement: an outward spiral from the star, under 100 units radially.
pub fn default_planet_coordinates(index: usize) -> [f64; 2] {
    let r = (15.0 + index as f64 * 10.0).min(95.0);
    let a = index as f64 * 2.399_963; // golden angle
    [
        (r * a.cos() * 10.0).round() / 10.0,
        (r * a.sin() * 10.0).round() / 10.0,
    ]
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
    let raw: RawPlanet = ctx.de(value)?;
    let size = match raw.size {
        RawSize::Small => PlanetSize::Small,
        RawSize::Large => PlanetSize::Large,
    };
    let habitable = raw.habitable.unwrap_or(size == PlanetSize::Large);
    let mut p = Planet::new(
        name,
        raw.display_name,
        size,
        habitable,
        defs.resource_count(),
    );
    p.pop = raw.total_pop;
    p.moon_of = raw.moon_of;
    p.growth_progress = raw.growth_progress;
    p.terraform_days_left = raw.terraform_days_left;
    p.siege = raw.siege;
    p.coordinates = raw
        .coordinates
        .map_or_else(|| default_planet_coordinates(pctx.index), |c| [c.x, c.y]);
    let empire = |e: &str| {
        pctx.empires
            .get(e)
            .copied()
            .ok_or_else(|| ctx.err(format!("unknown empire '{e}'")))
    };
    p.owner = match raw.owned_by.as_deref() {
        Some(o) => Some(empire(o)?),
        None => pctx.system_owner,
    };

    for (key, tvalue) in raw.tiles {
        let tctx = Ctx::with_header(defs, root, format!("{header}.tiles.{key}"));
        let index: usize = key
            .parse()
            .map_err(|_| tctx.err("tile keys must be integers"))?;
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
        for m in &raw_tile.resource_multipliers {
            tile.resource_mults
                .push((tctx.resource(&m.resource)?, m.mult));
        }
        if let Some(b) = raw_tile.building {
            let inst = parse_tile_building(&tctx, b)?;
            let def = defs.building(inst.def);
            if !habitable && !def.barren {
                warnings.push(format!(
                    "{}: {} is not a barren-class building but sits on a barren planet",
                    tctx.header, def.key.name
                ));
            }
            if (def.land && tile.water) || (def.water && !tile.water) {
                warnings.push(format!(
                    "{}: {} must stand on {} but the tile is {}",
                    tctx.header,
                    def.key.name,
                    if def.water { "water" } else { "land" },
                    if tile.water { "water" } else { "land" },
                ));
            }
            tile.building = Some(inst);
        }
        if let Some(c) = raw_tile.construction {
            tile.construction = Some(match (c.building, c.upgrade) {
                (Some(b), None) => Construction::Building {
                    def: BuildingId(tctx.lookup(Kind::Building, &b)?),
                    days_left: c.days_left,
                    empire: match c.empire.as_deref() {
                        Some(e) => Some(empire(e)?),
                        None => p.owner,
                    },
                    paid: tctx.amounts(&c.paid)?,
                },
                (None, Some(u)) => {
                    let Some(inst) = &tile.building else {
                        return Err(tctx.err("upgrade construction on a tile without a building"));
                    };
                    Construction::Upgrade {
                        upgrade: resolve_tile_upgrade(&tctx, inst.def, &u)?,
                        days_left: c.days_left,
                    }
                }
                _ => {
                    return Err(
                        tctx.err("construction needs exactly one of `building` or `upgrade`")
                    );
                }
            });
        }
    }
    Ok(p)
}

fn resolve_tile_upgrade(ctx: &Ctx, building: BuildingId, u: &str) -> Result<UpgradeId> {
    let def = ctx.defs.building(building);
    let id = UpgradeId(ctx.lookup(Kind::Upgrade, u)?);
    if !def.available_upgrades.contains(&id) {
        return Err(ctx.err(format!(
            "upgrade '{u}' is not available for building '{}'",
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
            for u in &raw.upgrades {
                inst.upgrades.push(resolve_tile_upgrade(ctx, def, u)?);
            }
            for u in &inst.upgrades {
                for need in &ctx.defs.upgrade(*u).needs_upgrades {
                    if !inst.upgrades.contains(need) {
                        return Err(ctx.err(format!(
                            "upgrade '{}' requires '{}'",
                            ctx.defs.upgrade(*u).key.name,
                            ctx.defs.upgrade(*need).key.name
                        )));
                    }
                }
            }
            inst.workers = raw.workers.min(ctx.defs.building(def).max_workers);
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
        assert!(check_import_header("[resource.x]\ndisplay_name = \"x\"\n[import]\n", p).is_err());
        assert!(check_import_header("[resource.x]\n", p).is_ok());
    }
}
