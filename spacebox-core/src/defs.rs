//! Strongly-typed, fully resolved asset definitions.
//!
//! These are produced by the [`loader`](crate::loader) from `[resource.*]`, `[job.*]`,
//! `[building.*]`, `[building-upgrade.*]`, `[technology.*]`, `[ship.*]`, `[ship-weapon.*]`,
//! `[ship-outfit.*]` and `[ruleset.*]` tables. All
//! cross-references have been resolved to ids; nothing here is mutated during simulation.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::geometry::PlanetSize;
use crate::ids::*;

/// Building classes with built-in meaning (see the design document):
/// `barren` buildings can be built on barren planets; `blocker`s are natural features that
/// are generated rather than built and must be removed to free their tile; `land` and `water`
/// buildings must stand on tiles without or with water; `space` buildings belong on orbital
/// megastructures, which do not exist yet, so they cannot be placed anywhere.
pub const BARREN_CLASS: &str = "barren";
pub const BLOCKER_CLASS: &str = "blocker";
pub const LAND_CLASS: &str = "land";
pub const WATER_CLASS: &str = "water";
pub const SPACE_CLASS: &str = "space";

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

#[derive(Clone, Debug)]
pub struct ResourceDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    pub image: Option<PathBuf>,
    /// Reference price on the galactic market, in units of the ruleset currency.
    pub base_value: f64,
    /// Whether the resource can be bought and sold on the galactic market.
    pub tradeable: bool,
    /// Whether surpluses accumulate in empire storage (false for e.g. research).
    pub stockpile: bool,
    /// Maximum amount an empire can store.
    pub storage_cap: Option<f64>,
    /// Only produced while an empire has a use for it. Buildings whose outputs include an
    /// on-demand resource sit idle (no workers, no inputs) when nothing needs them.
    pub on_demand: bool,
}

#[derive(Clone, Debug)]
pub struct JobDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    /// Pops fill higher-appeal jobs first.
    pub appeal: i32,
    /// Daily upkeep per worker, on top of the ruleset's per-pop upkeep.
    pub upkeep: Vec<ResourceAmount>,
}

/// Something a modifier can apply to: a specific building or every building of a class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildingTarget {
    Building(BuildingId),
    Class(ClassId),
}

/// A bundle of resource additions and multipliers.
///
/// Used by upgrades, adjacency bonuses and technologies. When several modifiers apply to one
/// building their resource lists are summed first, then the product of all multipliers is
/// applied to the totals.
#[derive(Clone, Debug)]
pub struct Modifier {
    pub input_mult: f64,
    pub output_mult: f64,
    pub upkeep_mult: f64,
    pub housing_mult: f64,
    pub amenities_mult: f64,
    pub inputs: Vec<ResourceAmount>,
    pub outputs: Vec<ResourceAmount>,
    pub upkeep: Vec<ResourceAmount>,
    pub housing: f64,
    pub amenities: f64,
}

impl Default for Modifier {
    fn default() -> Self {
        Modifier {
            input_mult: 1.0,
            output_mult: 1.0,
            upkeep_mult: 1.0,
            housing_mult: 1.0,
            amenities_mult: 1.0,
            inputs: Vec::new(),
            outputs: Vec::new(),
            upkeep: Vec::new(),
            housing: 0.0,
            amenities: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TargetedModifier {
    pub target: BuildingTarget,
    pub modifier: Modifier,
}

#[derive(Clone, Debug)]
pub struct BuildingDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    pub image: Option<PathBuf>,
    pub max_workers: u32,
    pub job: Option<JobId>,
    pub classes: Vec<ClassId>,
    /// Whether this building may be placed on barren planets (has the `barren` class).
    pub barren: bool,
    /// A natural feature (`blocker` class): generated, never constructed.
    pub blocker: bool,
    /// Must stand on a tile without water (`land` class).
    pub land: bool,
    /// Must stand on a water tile (`water` class).
    pub water: bool,
    /// Belongs on an orbital megastructure (`space` class; not implemented yet).
    pub space: bool,
    pub upkeep: Vec<ResourceAmount>,
    pub inputs: Vec<ResourceAmount>,
    pub outputs: Vec<ResourceAmount>,
    pub housing: f64,
    pub amenities: f64,
    /// Bonuses this building receives for each adjacent building matching the target.
    pub adjacency: Vec<TargetedModifier>,
    pub build_cost: Vec<ResourceAmount>,
    pub build_days: u32,
    pub requires_tech: Vec<TechId>,
    /// False for buildings that only appear via scenarios (e.g. capitals).
    pub buildable: bool,
    pub max_per_planet: Option<u32>,
    /// Bombardment damage absorbed before destruction (`None`: the ruleset's `building_hp`).
    pub hp: Option<f64>,
    /// Paid to demolish the building (or clear a blocker).
    pub remove_cost: Vec<ResourceAmount>,
    /// Upgrades whose `apply_to` matches this building and whose dependencies are reachable.
    pub available_upgrades: Vec<UpgradeId>,
}

impl BuildingDef {
    pub fn has_class(&self, class: ClassId) -> bool {
        self.classes.contains(&class)
    }
}

#[derive(Clone, Debug)]
pub struct UpgradeDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    pub apply_to: Vec<BuildingTarget>,
    pub needs_upgrades: Vec<UpgradeId>,
    pub modifier: Modifier,
    pub cost: Vec<ResourceAmount>,
    pub build_days: u32,
    pub requires_tech: Vec<TechId>,
}

#[derive(Clone, Debug)]
pub struct TechDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    /// Research points required.
    pub cost: f64,
    pub prerequisites: Vec<TechId>,
    /// Empire-wide modifiers granted to matching buildings.
    pub modifiers: Vec<TargetedModifier>,
    pub pop_growth_mult: f64,
    /// Flat stability bonus on every planet of the empire.
    pub stability: f64,
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
    pub hull: f64,
    pub shield: f64,
    pub shield_regen: f64,
    pub hull_mult: f64,
    pub shield_mult: f64,
    pub shield_regen_mult: f64,
    pub speed_mult: f64,
    pub hyperjump_mult: f64,
    /// Divides weapon `fire_rate` (the delay between shots).
    pub fire_rate_mult: f64,
    pub damage_mult: f64,
    pub range_mult: f64,
}

/// One weapon mount on a ship, with outfit bonuses applied.
#[derive(Clone, Debug)]
pub struct Armament {
    pub weapon: WeaponId,
    pub shield_damage: f64,
    pub hull_damage: f64,
    /// Days between shots.
    pub cooldown: f64,
    pub range: f64,
}

/// A ship design. The combat stats are derived at load time from the hull, weapons and
/// outfits: additive outfit bonuses are summed first, then multipliers applied.
#[derive(Clone, Debug)]
pub struct ShipDef {
    pub key: DefKey,
    pub display_name: String,
    pub description: Option<String>,
    pub weapons: Vec<WeaponId>,
    pub outfits: Vec<OutfitId>,
    pub requires_tech: Vec<TechId>,
    /// Hull, weapon and outfit costs combined.
    pub build_cost: Vec<ResourceAmount>,
    /// Daily upkeep of the hull plus its outfits.
    pub upkeep: Vec<ResourceAmount>,
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

#[derive(Clone, Debug)]
pub struct TerraformRule {
    pub requires_tech: Option<TechId>,
    pub days: u32,
    pub cost: Vec<ResourceAmount>,
}

/// Global simulation constants. Selected by `[game] ruleset = "..."`.
#[derive(Clone, Debug)]
pub struct Ruleset {
    pub key: DefKey,
    /// True for the fallback ruleset used when no `[ruleset.*]` was loaded.
    pub builtin: bool,
    /// Daily consumption per pop.
    pub pop_upkeep: Vec<ResourceAmount>,
    /// Running short of this resource kills pops.
    pub starvation_resource: Option<ResourceId>,
    /// Running short of this resource lowers stability.
    pub comfort_resource: Option<ResourceId>,
    /// Resource whose income is fed into technology research.
    pub research_resource: Option<ResourceId>,
    /// Resource used to settle galactic market trades.
    pub currency: Option<ResourceId>,
    /// Logistic growth rate per pop per day.
    pub pop_growth_rate: f64,
    /// Minimum daily growth on a planet with pops and free housing.
    pub min_pop_growth: f64,
    /// Fraction of pops lost per day at a total starvation-resource shortage.
    pub starvation_rate: f64,
    /// Fraction of unemployed/homeless pops that try to emigrate each day.
    pub migration_rate: f64,
    pub base_housing_large: f64,
    pub base_housing_small: f64,
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
    /// Fraction of surplus lost when a non-capital system ships it to the rest of the empire.
    pub logistics_loss: f64,
    /// Fee paid by buyers on the galactic market, as a fraction of the price.
    pub market_fee: f64,
    /// Empires only sell stock above this amount.
    pub market_reserve: f64,
    /// Maximum appeal bonus for jobs whose outputs are scarce in the pool.
    pub scarcity_appeal: f64,
    /// Stock (in days of demand) below which a resource counts as scarce.
    pub scarcity_target_days: f64,
    /// Days between automated governor decisions for AI empires.
    pub ai_interval_days: u32,
    pub terraform_large: Option<TerraformRule>,
    pub terraform_small: Option<TerraformRule>,
    /// Resources every empire receives each day (e.g. 5 influence).
    pub empire_income: Vec<ResourceAmount>,
    /// Resource spent to build in unowned systems.
    pub expansion_resource: Option<ResourceId>,
    /// Expansion costs `expansion_base_cost * 2^n`, where n is the jump distance from the
    /// empire's borders minus one.
    pub expansion_base_cost: f64,
    /// Expansion adds `expansion_base_days * 2^(n/2)` days of construction.
    pub expansion_base_days: f64,
    /// Days for a ship to charge its hyperdrive (scaled by the ship's `hyperjump_mult`).
    pub hyperjump_days: f64,
    /// Combat steps per day.
    pub combat_subticks: u32,
    /// Distance from the star at which fleets arrive from a hyperjump.
    pub arrival_radius: f64,
    /// Fraction of max hull repaired per day in friendly, peaceful systems.
    pub ship_repair_rate: f64,
    /// Default damage a building absorbs before it is destroyed.
    pub building_hp: f64,
    /// Damage fraction at which a building stops working.
    pub building_disable_damage: f64,
    /// Damage fraction repaired per day when not under bombardment.
    pub building_repair_rate: f64,
    /// Amenities lost on a planet while it is being bombarded.
    pub bombardment_amenities_penalty: f64,
    /// Base bombardment damage a planet withstands before surrendering (scaled by pops and
    /// stability).
    pub surrender_resistance: f64,
    /// Fraction of siege progress lost per day without bombardment.
    pub siege_decay: f64,
    /// Days after a peace treaty during which AI empires will not renew the war.
    pub truce_days: u64,
}

impl Ruleset {
    pub fn base_housing(&self, size: PlanetSize, habitable: bool) -> f64 {
        match (habitable, size) {
            (false, _) => 0.0,
            (true, PlanetSize::Large) => self.base_housing_large,
            (true, PlanetSize::Small) => self.base_housing_small,
        }
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

    pub(crate) fn builtin() -> Self {
        Ruleset {
            key: DefKey {
                file: FileId(u32::MAX),
                name: "builtin".into(),
            },
            builtin: true,
            pop_upkeep: Vec::new(),
            starvation_resource: None,
            comfort_resource: None,
            research_resource: None,
            currency: None,
            pop_growth_rate: 0.003,
            min_pop_growth: 0.05,
            starvation_rate: 0.01,
            migration_rate: 0.05,
            base_housing_large: 100.0,
            base_housing_small: 40.0,
            base_amenities: 10.0,
            amenities_per_pop: 0.1,
            base_stability: 50.0,
            stability_output_factor: 0.004,
            unemployment_stability_penalty: 15.0,
            homelessness_stability_penalty: 20.0,
            comfort_stability_penalty: 20.0,
            crisis_stability_penalty: 25.0,
            logistics_loss: 0.05,
            market_fee: 0.1,
            market_reserve: 1000.0,
            scarcity_appeal: 40.0,
            scarcity_target_days: 60.0,
            ai_interval_days: 30,
            terraform_large: None,
            terraform_small: None,
            empire_income: Vec::new(),
            expansion_resource: None,
            expansion_base_cost: 100.0,
            expansion_base_days: 30.0,
            hyperjump_days: 3.0,
            combat_subticks: 8,
            arrival_radius: 90.0,
            ship_repair_rate: 0.05,
            building_hp: 1000.0,
            building_disable_damage: 0.5,
            building_repair_rate: 0.02,
            bombardment_amenities_penalty: 40.0,
            surrender_resistance: 4000.0,
            siege_decay: 0.02,
            truce_days: 360,
        }
    }
}

/// Which definition table a reference points into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Resource = 0,
    Job = 1,
    Building = 2,
    Upgrade = 3,
    Technology = 4,
    Ruleset = 5,
    Ship = 6,
    Weapon = 7,
    Outfit = 8,
}

pub(crate) const KIND_COUNT: usize = 9;

impl Kind {
    pub const ALL: [Kind; KIND_COUNT] = [
        Kind::Resource,
        Kind::Job,
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
            Kind::Building => "building",
            Kind::Upgrade => "building-upgrade",
            Kind::Technology => "technology",
            Kind::Ruleset => "ruleset",
            Kind::Ship => "ship",
            Kind::Weapon => "ship-weapon",
            Kind::Outfit => "ship-outfit",
        }
    }
}

/// All asset definitions reachable from the root file.
#[derive(Debug)]
pub struct Defs {
    pub files: Vec<SourceFile>,
    pub resources: Vec<ResourceDef>,
    pub jobs: Vec<JobDef>,
    pub buildings: Vec<BuildingDef>,
    pub upgrades: Vec<UpgradeDef>,
    pub technologies: Vec<TechDef>,
    pub rulesets: Vec<Ruleset>,
    pub ships: Vec<ShipDef>,
    pub weapons: Vec<WeaponDef>,
    pub outfits: Vec<OutfitDef>,
    pub classes: Vec<String>,
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
    pub fn class_name(&self, id: ClassId) -> &str {
        &self.classes[id.idx()]
    }
    pub fn class_id(&self, name: &str) -> Option<ClassId> {
        self.classes
            .iter()
            .position(|c| c == name)
            .map(ClassId::from_idx)
    }

    /// Resolves a reference the same way the loader does: `alias.name` goes through the
    /// imports of `scope`, a bare `name` is looked up in `scope` itself.
    pub fn lookup(&self, scope: FileId, kind: Kind, reference: &str) -> Result<u32, String> {
        let (file, name) = match reference.split_once('.') {
            Some((alias, name)) => {
                let file = self.files[scope.idx()].import(alias).ok_or_else(|| {
                    format!(
                        "unknown import alias '{alias}' in {} reference '{reference}'",
                        kind.table()
                    )
                })?;
                (file, name)
            }
            None => (scope, reference),
        };
        self.names[file.idx()][kind as usize]
            .get(name)
            .copied()
            .ok_or_else(|| {
                format!(
                    "unknown {kind} '{reference}' (no [{kind}.{name}] in {})",
                    self.files[file.idx()].path.display(),
                    kind = kind.table(),
                )
            })
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

    /// Whether any building output is an on-demand resource.
    pub fn produces_on_demand(&self, b: BuildingId) -> bool {
        self.building(b)
            .outputs
            .iter()
            .any(|o| self.resource(o.resource).on_demand)
    }

    pub fn resource_count(&self) -> usize {
        self.resources.len()
    }

    /// Does `target` match building `id`?
    pub fn target_matches(&self, target: BuildingTarget, id: BuildingId) -> bool {
        match target {
            BuildingTarget::Building(b) => b == id,
            BuildingTarget::Class(c) => self.building(id).has_class(c),
        }
    }

    /// Whether `tech` is not yet known but all of its prerequisites are.
    pub fn tech_available(&self, tech: TechId, known: &[bool]) -> bool {
        !known[tech.idx()] && self.tech(tech).prerequisites.iter().all(|p| known[p.idx()])
    }
}
