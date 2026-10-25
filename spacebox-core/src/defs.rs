//! Strongly-typed, fully resolved asset definitions.
//!
//! These are produced by the [`loader`](crate::loader) from `[resource.*]`, `[job.*]`,
//! `[species.*]`, `[building.*]`, `[building-upgrade.*]`, `[technology.*]`, `[ship.*]`,
//! `[ship-weapon.*]`, `[ship-outfit.*]` and `[ruleset.*]` tables. All cross-references have been resolved to ids;
//! nothing here is mutated during simulation.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::geometry::PlanetSize;
use crate::ids::*;

/// Building tags with built-in meaning (see `design/data/buildings.toml`): `barren` buildings
/// can be built on barren planets; `blocker`s generate naturally and must be removed to free
/// their tile; `land` and `water` buildings stand on tiles without or with water (every
/// building is implicitly `land` unless it is tagged `water` or `space`); `space` buildings
/// belong on orbital megastructures, which do not exist yet.
pub const BARREN_TAG: &str = "barren";
pub const BLOCKER_TAG: &str = "blocker";
pub const LAND_TAG: &str = "land";
pub const WATER_TAG: &str = "water";
pub const SPACE_TAG: &str = "space";

/// The reserved building tags, which are global rather than scoped to a file.
pub const RESERVED_TAGS: [&str; 5] = [BARREN_TAG, BLOCKER_TAG, LAND_TAG, WATER_TAG, SPACE_TAG];

/// A tag (on buildings, jobs or technologies). Tags are scoped to the file that uses them and
/// referenced from other files as `alias.tag`, except the reserved building tags, which are
/// global (`file` is `None`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Tag {
    pub file: Option<FileId>,
    pub name: String,
}

/// Where a definition came from: needed to write references back out in save files.
#[derive(Clone, Debug)]
pub struct DefKey {
    pub file: FileId,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub path: PathBuf,
    /// `alias -> file`, in declaration order.
    pub imports: Vec<(String, FileId)>,
}

impl SourceFile {
    pub fn import(&self, alias: &str) -> Option<FileId> {
        self.imports
            .iter()
            .find(|(a, _)| a == alias)
            .map(|(_, f)| *f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResourceAmount {
    pub resource: ResourceId,
    pub count: f64,
}

/// Merges `items` into `list`, summing counts per resource.
pub fn add_amounts(list: &mut Vec<ResourceAmount>, items: &[ResourceAmount]) {
    for a in items {
        match list.iter_mut().find(|x| x.resource == a.resource) {
            Some(x) => x.count += a.count,
            None => list.push(*a),
        }
    }
}

/// The cost of a project (construction, removal, terraforming, research, a ship): resources
/// plus the special `{ days = N }` entry. Also used for what is still *remaining* on a project.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cost {
    pub resources: Vec<ResourceAmount>,
    pub days: u32,
}

impl Cost {
    pub fn is_done(&self) -> bool {
        self.days == 0 && self.resources.iter().all(|a| a.count <= 1e-9)
    }

    pub fn get(&self, r: ResourceId) -> f64 {
        self.resources
            .iter()
            .filter(|a| a.resource == r)
            .map(|a| a.count)
            .sum()
    }
}

/// What a building produces: resources plus the special `{ amenities = N }` entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Yield {
    pub resources: Vec<ResourceAmount>,
    pub amenities: f64,
}

impl Yield {
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty() && self.amenities == 0.0
    }

    pub fn get(&self, r: ResourceId) -> f64 {
        self.resources
            .iter()
            .filter(|a| a.resource == r)
            .map(|a| a.count)
            .sum()
    }

    pub fn add(&mut self, other: &Yield) {
        add_amounts(&mut self.resources, &other.resources);
        self.amenities += other.amenities;
    }
}

#[derive(Clone, Debug)]
pub struct ResourceDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    /// Transient resources (research, shipbuilding) are never stored: each day's production
    /// goes to active projects or is wasted.
    pub transient: bool,
    /// Whether the resource can be bought and sold on the galactic market.
    pub tradeable: bool,
    /// Reference price on the galactic market, in units of the ruleset currency.
    pub base_value: f64,
}

#[derive(Clone, Debug)]
pub struct JobDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    /// 0..100; pops fill higher-appeal jobs first.
    pub appeal: i32,
    pub tags: Vec<TagId>,
    /// The job gains appeal while any of these resources is scarce.
    pub associated_resources: Vec<ResourceId>,
}

#[derive(Clone, Debug)]
pub struct SpeciesDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    /// Daily consumption per pop.
    pub upkeep: Vec<ResourceAmount>,
    /// A shortage of this resource kills pops.
    pub starvation_resource: Option<ResourceId>,
    /// A shortage of this resource lowers stability (`None`: always satisfied).
    pub comfort_resource: Option<ResourceId>,
    /// Logistic growth per pop per day.
    pub growth_rate: f64,
    /// Minimum growth per day on a planet with these pops and free housing.
    pub min_growth: f64,
    /// Fraction of pops lost per day at a total starvation-resource shortage.
    pub starvation_rate: f64,
    /// Fraction of unemployed/homeless pops that try to move each day.
    pub migration_rate: f64,
}

/// Which buildings something applies to: the listed buildings plus every building with one
/// of the listed tags.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Targets {
    pub buildings: Vec<BuildingId>,
    pub tags: Vec<TagId>,
}

/// Additions and multipliers for a building's numbers. Used by upgrades, adjacency bonuses
/// and technologies. When several apply to one building, all additions are summed first and
/// then the product of all multipliers is applied to the totals.
#[derive(Clone, Debug)]
pub struct Modifier {
    pub upkeep_add: Vec<ResourceAmount>,
    pub upkeep_mult: f64,
    pub input_add: Vec<ResourceAmount>,
    pub input_mult: f64,
    pub output_add: Yield,
    pub output_mult: f64,
    pub passive_add: Yield,
    pub passive_mult: f64,
    pub max_workers_add: f64,
    pub max_workers_mult: f64,
    pub housing_add: f64,
    pub housing_mult: f64,
    /// Upgrades only: the building's job is replaced by this one.
    pub job: Option<JobId>,
}

impl Default for Modifier {
    fn default() -> Self {
        Modifier {
            upkeep_add: Vec::new(),
            upkeep_mult: 1.0,
            input_add: Vec::new(),
            input_mult: 1.0,
            output_add: Yield::default(),
            output_mult: 1.0,
            passive_add: Yield::default(),
            passive_mult: 1.0,
            max_workers_add: 0.0,
            max_workers_mult: 1.0,
            housing_add: 0.0,
            housing_mult: 1.0,
            job: None,
        }
    }
}

impl Modifier {
    /// Folds `m` into this accumulated modifier.
    pub fn combine(&mut self, m: &Modifier) {
        add_amounts(&mut self.upkeep_add, &m.upkeep_add);
        add_amounts(&mut self.input_add, &m.input_add);
        self.output_add.add(&m.output_add);
        self.passive_add.add(&m.passive_add);
        self.max_workers_add += m.max_workers_add;
        self.housing_add += m.housing_add;
        self.upkeep_mult *= m.upkeep_mult;
        self.input_mult *= m.input_mult;
        self.output_mult *= m.output_mult;
        self.passive_mult *= m.passive_mult;
        self.max_workers_mult *= m.max_workers_mult;
        self.housing_mult *= m.housing_mult;
        if m.job.is_some() {
            self.job = m.job;
        }
    }
}

/// A modifier and what it applies to (no targets at all: every building).
#[derive(Clone, Debug)]
pub struct TargetedModifier {
    pub targets: Targets,
    pub modifier: Modifier,
}

#[derive(Clone, Debug)]
pub struct BuildingDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    /// `None`: no workers, always runs at 100%.
    pub max_workers: Option<u32>,
    pub job: Option<JobId>,
    pub tags: Vec<TagId>,
    /// Can be built on barren planets (`barren` tag).
    pub barren: bool,
    /// Generates naturally (`blocker` tag); never constructed.
    pub blocker: bool,
    /// May stand on land tiles (explicit `land` tag, or implicit without `water`/`space`).
    pub land: bool,
    /// May stand on water tiles (`water` tag).
    pub water: bool,
    /// Belongs on an orbital megastructure (`space` tag; not implemented yet).
    pub space: bool,
    pub housing: f64,
    pub build_cost: Cost,
    /// `None`: removal is free and instant.
    pub remove_cost: Option<Cost>,
    pub max_per_planet: Option<u32>,
    /// Bombardment damage absorbed before destruction (`None`: the ruleset's `building_hp`).
    pub hp: Option<f64>,
    pub requires_tech: Vec<TechId>,
    /// Daily, independent of workers.
    pub upkeep: Vec<ResourceAmount>,
    /// Daily at full employment.
    pub inputs: Vec<ResourceAmount>,
    /// Daily at full employment.
    pub outputs: Yield,
    /// Daily, independent of workers.
    pub passive_outputs: Yield,
    /// Bonuses this building receives for each adjacent building matching the target.
    pub adjacency: Vec<TargetedModifier>,
    /// Upgrades whose `applies-to` matches this building and whose `after-upgrades` are
    /// (recursively) available for it too.
    pub available_upgrades: Vec<UpgradeId>,
}

impl BuildingDef {
    pub fn has_tag(&self, tag: TagId) -> bool {
        self.tags.contains(&tag)
    }

    /// Whether this building may stand on a tile with (`true`) or without water.
    pub fn fits_terrain(&self, water: bool) -> bool {
        if water { self.water } else { self.land }
    }
}

/// A building upgrade (`[building-upgrade.*]`).
#[derive(Clone, Debug)]
pub struct UpgradeDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    /// Buildings (and building tags) the upgrade can be installed on.
    pub applies_to: Targets,
    pub after_upgrades: Vec<UpgradeId>,
    pub modifier: Modifier,
    pub build_cost: Cost,
    pub requires_tech: Vec<TechId>,
}

/// Ship stat additions and multipliers, from outfits and technologies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShipMods {
    pub hull_add: f64,
    pub shield_add: f64,
    pub shield_regen_add: f64,
    pub base_speed_add: f64,
    pub hull_mult: f64,
    pub shield_mult: f64,
    pub shield_regen_mult: f64,
    pub base_speed_mult: f64,
    pub hyperjump_mult: f64,
    /// Divides weapon `fire-rate` (the delay between shots).
    pub fire_rate_mult: f64,
    pub damage_mult: f64,
    pub range_mult: f64,
}

impl Default for ShipMods {
    fn default() -> Self {
        ShipMods {
            hull_add: 0.0,
            shield_add: 0.0,
            shield_regen_add: 0.0,
            base_speed_add: 0.0,
            hull_mult: 1.0,
            shield_mult: 1.0,
            shield_regen_mult: 1.0,
            base_speed_mult: 1.0,
            hyperjump_mult: 1.0,
            fire_rate_mult: 1.0,
            damage_mult: 1.0,
            range_mult: 1.0,
        }
    }
}

impl ShipMods {
    pub fn combine(&mut self, m: &ShipMods) {
        self.hull_add += m.hull_add;
        self.shield_add += m.shield_add;
        self.shield_regen_add += m.shield_regen_add;
        self.base_speed_add += m.base_speed_add;
        self.hull_mult *= m.hull_mult;
        self.shield_mult *= m.shield_mult;
        self.shield_regen_mult *= m.shield_regen_mult;
        self.base_speed_mult *= m.base_speed_mult;
        self.hyperjump_mult *= m.hyperjump_mult;
        self.fire_rate_mult *= m.fire_rate_mult;
        self.damage_mult *= m.damage_mult;
        self.range_mult *= m.range_mult;
    }
}

#[derive(Clone, Debug)]
pub struct TechDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    /// What researching it costs. Transient resources come from daily output, normal ones
    /// from the stockpile.
    pub cost: Vec<ResourceAmount>,
    pub tags: Vec<TagId>,
    pub prerequisites: Vec<TechId>,
    pub pop_growth_mult: f64,
    pub fleet_size_add: u32,
    pub ships: ShipMods,
    /// Empire-wide modifiers granted to matching buildings.
    pub buildings: Vec<TargetedModifier>,
}

#[derive(Clone, Debug)]
pub struct WeaponDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    pub build_cost: Vec<ResourceAmount>,
    pub shield_damage: f64,
    pub hull_damage: f64,
    /// Days between shots.
    pub fire_rate: f64,
    /// In system units.
    pub range: f64,
}

#[derive(Clone, Debug)]
pub struct OutfitDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    pub build_cost: Vec<ResourceAmount>,
    pub upkeep: Vec<ResourceAmount>,
    pub mods: ShipMods,
}

/// One weapon mount on a ship, with outfit and technology bonuses applied.
#[derive(Clone, Debug)]
pub struct Armament {
    pub weapon: WeaponId,
    pub shield_damage: f64,
    pub hull_damage: f64,
    /// Days between shots.
    pub cooldown: f64,
    pub range: f64,
}

/// A ship design's effective combat stats.
#[derive(Clone, Debug)]
pub struct ShipStats {
    pub max_hull: f64,
    pub max_shield: f64,
    /// Shield points regenerated per day.
    pub shield_regen: f64,
    /// System units per day.
    pub speed: f64,
    /// Multiplier on the ruleset's `hyperjump_days`.
    pub hyperjump_mult: f64,
    pub armament: Vec<Armament>,
    /// Longest weapon range (0 for unarmed ships).
    pub max_range: f64,
}

/// A ship design.
#[derive(Clone, Debug)]
pub struct ShipDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    pub weapons: Vec<WeaponId>,
    pub outfits: Vec<OutfitId>,
    pub requires_tech: Vec<TechId>,
    /// Hull, weapon and outfit costs combined.
    pub build_cost: Cost,
    /// Daily upkeep of the hull plus its outfits.
    pub upkeep: Vec<ResourceAmount>,
    pub hull: f64,
    pub shield: f64,
    pub shield_regen: f64,
    pub base_speed: f64,
    pub hyperjump_mult: f64,
    /// Fleet slots the ship takes up.
    pub fleet_size: u32,
    /// The design's outfits combined.
    pub outfit_mods: ShipMods,
    /// Stats without any technology bonuses.
    pub base_stats: ShipStats,
}

impl ShipDef {
    /// Effective stats with an empire's technology bonuses (`tech`) on top of the outfits.
    pub fn stats(&self, defs: &Defs, tech: &ShipMods) -> ShipStats {
        let mut m = self.outfit_mods;
        m.combine(tech);
        let armament: Vec<Armament> = self
            .weapons
            .iter()
            .map(|w| {
                let d = defs.weapon(*w);
                Armament {
                    weapon: *w,
                    shield_damage: d.shield_damage * m.damage_mult,
                    hull_damage: d.hull_damage * m.damage_mult,
                    cooldown: d.fire_rate / m.fire_rate_mult.max(1e-9),
                    range: d.range * m.range_mult,
                }
            })
            .collect();
        ShipStats {
            max_hull: ((self.hull + m.hull_add) * m.hull_mult).max(1.0),
            max_shield: ((self.shield + m.shield_add) * m.shield_mult).max(0.0),
            shield_regen: ((self.shield_regen + m.shield_regen_add) * m.shield_regen_mult).max(0.0),
            speed: ((self.base_speed + m.base_speed_add) * m.base_speed_mult).max(0.0),
            hyperjump_mult: (self.hyperjump_mult * m.hyperjump_mult).max(0.0),
            max_range: armament.iter().map(|a| a.range).fold(0.0, f64::max),
            armament,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TerraformRule {
    pub requires_tech: Option<TechId>,
    pub cost: Cost,
}

/// Global simulation constants, selected by `[game] ruleset = "..."`. Field groups follow the
/// `[ruleset.x.*]` sub-tables.
#[derive(Clone, Debug)]
pub struct Ruleset {
    pub key: DefKey,
    pub display_name: String,
    /// True for the fallback ruleset used when no `[ruleset.*]` was loaded.
    pub builtin: bool,

    // resources
    /// What new empires start with.
    pub starting_stores: Vec<ResourceAmount>,
    /// Maximum an empire can store, per resource (indexed by `ResourceId`).
    pub storage_caps: Vec<Option<f64>>,

    // amenities
    pub base_amenities: f64,
    pub amenities_per_pop: f64,
    pub base_stability: f64,
    /// Output multiplier is `1 + (stability - 50) * stability_output_factor`.
    pub stability_output_factor: f64,
    pub unemployment_stability_penalty: f64,
    pub homelessness_stability_penalty: f64,
    pub comfort_stability_penalty: f64,
    /// Stability lost per unit of total crisis severity.
    pub crisis_stability_penalty: f64,

    // economy
    /// Resource used to settle galactic market trades.
    pub currency: Option<ResourceId>,
    /// Fraction of surplus lost when a non-capital system ships it to the rest of the empire.
    pub logistics_loss: f64,
    /// Fee paid by buyers on the galactic market, as a fraction of the price.
    pub market_fee: f64,
    /// Empires only sell stock above this amount.
    pub market_reserve: f64,
    /// Maximum appeal bonus for jobs whose associated resource is scarce.
    pub scarcity_appeal: f64,
    /// Stock (in days of demand) below which a resource counts as scarce.
    pub scarcity_target_days: f64,

    // ai
    /// Days between AI governor decisions.
    pub ai_interval_days: u32,

    // technology
    /// Multiplier on transient income spent on research.
    pub research_speed_mult: f64,

    // expansion
    /// Resource every empire receives daily and spends to build in unowned systems.
    pub expansion_resource: Option<ResourceId>,
    pub expansion_resource_income: f64,
    /// Expansion costs `expansion_base_cost * 2^n`, where n is the jump distance from the
    /// empire's borders minus one.
    pub expansion_base_cost: f64,
    /// Expansion adds `expansion_base_days * 2^(n/2)` days of construction.
    pub expansion_base_days: f64,

    // ships
    /// Days for a ship to charge its hyperdrive (scaled by the ship's `hyperjump_mult`).
    pub hyperjump_days: f64,
    /// Combat steps per day.
    pub combat_subticks: u32,
    /// Distance from the star at which fleets arrive from (and leave for) a hyperjump.
    pub arrival_radius: f64,
    /// Fraction of max hull repaired per day in friendly, peaceful systems.
    pub ship_repair_rate: f64,
    /// Most ships a fleet can hold before technology bonuses.
    pub base_fleet_size: u32,

    // invasion
    pub building_hp: f64,
    pub building_disable_damage: f64,
    pub building_repair_rate: f64,
    pub bombardment_amenities_penalty: f64,
    /// Base bombardment a planet withstands before surrendering (scaled by pops and
    /// stability).
    pub surrender_resistance: f64,
    pub siege_decay: f64,

    // diplomacy
    /// Days after a peace treaty during which AI empires will not renew the war.
    pub truce_days: u64,

    // terraforming
    pub terraform_large: Option<TerraformRule>,
    pub terraform_small: Option<TerraformRule>,
}

impl Ruleset {
    pub fn storage_cap(&self, r: ResourceId) -> Option<f64> {
        self.storage_caps.get(r.idx()).copied().flatten()
    }

    /// Influence cost and extra days for building `jumps` hyperlanes beyond the borders.
    pub fn expansion(&self, jumps: u32) -> (f64, u32) {
        let n = jumps.saturating_sub(1) as f64;
        let cost = self.expansion_base_cost * 2f64.powf(n);
        let days = (self.expansion_base_days * 2f64.powf(n / 2.0)).round() as u32;
        (cost, days)
    }

    /// Surrender threshold of a planet under bombardment.
    pub fn surrender_threshold(&self, pop: u64, stability: f64) -> f64 {
        self.surrender_resistance * (1.0 + pop as f64 / 500.0) * (0.5 + stability / 100.0)
    }

    pub fn terraform(&self, size: PlanetSize) -> Option<&TerraformRule> {
        match size {
            PlanetSize::Large => self.terraform_large.as_ref(),
            PlanetSize::Small => self.terraform_small.as_ref(),
        }
    }

    pub(crate) fn builtin(resources: usize) -> Self {
        Ruleset {
            key: DefKey {
                file: FileId(u32::MAX),
                name: "builtin".into(),
            },
            display_name: "Built-in Rules".into(),
            builtin: true,
            starting_stores: Vec::new(),
            storage_caps: vec![None; resources],
            base_amenities: 10.0,
            amenities_per_pop: 0.1,
            base_stability: 50.0,
            stability_output_factor: 0.004,
            unemployment_stability_penalty: 15.0,
            homelessness_stability_penalty: 20.0,
            comfort_stability_penalty: 20.0,
            crisis_stability_penalty: 25.0,
            currency: None,
            logistics_loss: 0.05,
            market_fee: 0.1,
            market_reserve: 1000.0,
            scarcity_appeal: 40.0,
            scarcity_target_days: 60.0,
            ai_interval_days: 30,
            research_speed_mult: 1.0,
            expansion_resource: None,
            expansion_resource_income: 5.0,
            expansion_base_cost: 100.0,
            expansion_base_days: 30.0,
            hyperjump_days: 3.0,
            combat_subticks: 8,
            arrival_radius: 90.0,
            ship_repair_rate: 0.05,
            base_fleet_size: 10,
            building_hp: 1000.0,
            building_disable_damage: 0.5,
            building_repair_rate: 0.02,
            bombardment_amenities_penalty: 40.0,
            surrender_resistance: 4000.0,
            siege_decay: 0.02,
            truce_days: 360,
            terraform_large: None,
            terraform_small: None,
        }
    }
}

/// Which definition table a reference points into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Resource = 0,
    Job = 1,
    Species = 2,
    Building = 3,
    Upgrade = 4,
    Technology = 5,
    Ruleset = 6,
    Ship = 7,
    Weapon = 8,
    Outfit = 9,
}

pub(crate) const KIND_COUNT: usize = 10;

impl Kind {
    pub const ALL: [Kind; KIND_COUNT] = [
        Kind::Resource,
        Kind::Job,
        Kind::Species,
        Kind::Building,
        Kind::Upgrade,
        Kind::Technology,
        Kind::Ruleset,
        Kind::Ship,
        Kind::Weapon,
        Kind::Outfit,
    ];

    /// The top-level TOML table name for this kind.
    pub fn table(self) -> &'static str {
        match self {
            Kind::Resource => "resource",
            Kind::Job => "job",
            Kind::Species => "species",
            Kind::Building => "building",
            Kind::Upgrade => "building-upgrade",
            Kind::Technology => "technology",
            Kind::Ruleset => "ruleset",
            Kind::Ship => "ship",
            Kind::Weapon => "ship-weapon",
            Kind::Outfit => "ship-outfit",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Kind::Upgrade => "building upgrade",
            k => k.table(),
        }
    }
}

/// All asset definitions reachable from the root file.
#[derive(Debug)]
pub struct Defs {
    pub files: Vec<SourceFile>,
    pub resources: Vec<ResourceDef>,
    pub jobs: Vec<JobDef>,
    pub species: Vec<SpeciesDef>,
    pub buildings: Vec<BuildingDef>,
    pub upgrades: Vec<UpgradeDef>,
    pub technologies: Vec<TechDef>,
    pub rulesets: Vec<Ruleset>,
    pub ships: Vec<ShipDef>,
    pub weapons: Vec<WeaponDef>,
    pub outfits: Vec<OutfitDef>,
    pub tags: Vec<Tag>,
    /// Per file, per kind: local name -> raw id.
    pub(crate) names: Vec<[HashMap<String, u32>; KIND_COUNT]>,
}

impl Defs {
    pub fn resource(&self, id: ResourceId) -> &ResourceDef {
        &self.resources[id.idx()]
    }
    pub fn job(&self, id: JobId) -> &JobDef {
        &self.jobs[id.idx()]
    }
    pub fn species_def(&self, id: SpeciesId) -> &SpeciesDef {
        &self.species[id.idx()]
    }
    pub fn building(&self, id: BuildingId) -> &BuildingDef {
        &self.buildings[id.idx()]
    }
    pub fn upgrade(&self, id: UpgradeId) -> &UpgradeDef {
        &self.upgrades[id.idx()]
    }
    pub fn tech(&self, id: TechId) -> &TechDef {
        &self.technologies[id.idx()]
    }
    pub fn ruleset(&self, id: RulesetId) -> &Ruleset {
        &self.rulesets[id.idx()]
    }
    pub fn ship(&self, id: ShipId) -> &ShipDef {
        &self.ships[id.idx()]
    }
    pub fn weapon(&self, id: WeaponId) -> &WeaponDef {
        &self.weapons[id.idx()]
    }
    pub fn outfit(&self, id: OutfitId) -> &OutfitDef {
        &self.outfits[id.idx()]
    }
    pub fn tag(&self, id: TagId) -> &Tag {
        &self.tags[id.idx()]
    }
    /// The tag's name without its file.
    pub fn tag_name(&self, id: TagId) -> &str {
        &self.tags[id.idx()].name
    }
    pub fn tag_id(&self, tag: &Tag) -> Option<TagId> {
        self.tags.iter().position(|t| t == tag).map(TagId::from_idx)
    }
    /// A reserved (global) building tag.
    pub fn reserved_tag(&self, name: &str) -> Option<TagId> {
        self.tag_id(&Tag {
            file: None,
            name: name.to_string(),
        })
    }

    /// Resolves a reference the same way the loader does: `alias.name` goes through the
    /// imports of `scope`, a bare `name` is looked up in `scope` itself.
    pub fn lookup(&self, scope: FileId, kind: Kind, reference: &str) -> Result<u32, String> {
        let (file, name) = match reference.split_once('.') {
            Some((alias, name)) => {
                let file = self.files[scope.idx()].import(alias).ok_or_else(|| {
                    format!(
                        "unknown import alias '{alias}' in {} reference '{reference}'",
                        kind.noun()
                    )
                })?;
                (file, name)
            }
            None => (scope, reference),
        };
        let tables = &self.names[file.idx()];
        if let Some(id) = tables[kind as usize].get(name) {
            return Ok(*id);
        }
        Err(format!(
            "unknown {} '{reference}' (no [{}.{name}] in {})",
            kind.noun(),
            kind.table(),
            self.files[file.idx()].path.display(),
        ))
    }

    /// Finds a definition by bare name in any loaded file (first match in load order).
    /// Intended for tools and generators; game data should use scoped references.
    pub fn find(&self, kind: Kind, name: &str) -> Option<u32> {
        if let Some((alias, rest)) = name.split_once('.') {
            for f in &self.files {
                if let Some(file) = f.import(alias)
                    && let Some(id) = self.names[file.idx()][kind as usize].get(rest)
                {
                    return Some(*id);
                }
            }
        }
        self.names
            .iter()
            .find_map(|tables| tables[kind as usize].get(name).copied())
    }

    pub fn find_resource(&self, name: &str) -> Option<ResourceId> {
        self.find(Kind::Resource, name).map(ResourceId)
    }
    pub fn find_species(&self, name: &str) -> Option<SpeciesId> {
        self.find(Kind::Species, name).map(SpeciesId)
    }
    pub fn find_building(&self, name: &str) -> Option<BuildingId> {
        self.find(Kind::Building, name).map(BuildingId)
    }
    pub fn find_upgrade(&self, name: &str) -> Option<UpgradeId> {
        self.find(Kind::Upgrade, name).map(UpgradeId)
    }
    pub fn find_tech(&self, name: &str) -> Option<TechId> {
        self.find(Kind::Technology, name).map(TechId)
    }
    pub fn find_ship(&self, name: &str) -> Option<ShipId> {
        self.find(Kind::Ship, name).map(ShipId)
    }

    pub fn resource_count(&self) -> usize {
        self.resources.len()
    }

    pub fn is_transient(&self, r: ResourceId) -> bool {
        self.resources[r.idx()].transient
    }

    /// Whether `targets` names building `id` or one of its tags.
    pub fn targets(&self, targets: &Targets, id: BuildingId) -> bool {
        targets.buildings.contains(&id)
            || targets.tags.iter().any(|t| self.building(id).has_tag(*t))
    }

    /// Like [`Defs::targets`], but empty targets match every building (technology effects).
    pub fn targets_or_all(&self, targets: &Targets, id: BuildingId) -> bool {
        (targets.buildings.is_empty() && targets.tags.is_empty()) || self.targets(targets, id)
    }

    /// Whether `tech` is not yet known but all of its prerequisites are.
    pub fn tech_available(&self, tech: TechId, known: &[bool]) -> bool {
        !known[tech.idx()] && self.tech(tech).prerequisites.iter().all(|p| known[p.idx()])
    }

    /// A tech's research cost as a project cost.
    pub fn research_cost(&self, tech: TechId) -> Cost {
        Cost {
            resources: self.tech(tech).cost.clone(),
            days: 0,
        }
    }
}
