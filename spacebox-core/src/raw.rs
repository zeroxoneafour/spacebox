//! Serde mirrors of the TOML format (see `design/data/`). References are still unresolved
//! strings here.
//!
//! Every struct uses kebab-case keys and denies unknown fields, so typos in mods are reported
//! instead of ignored. Lists that mix kinds of entries (costs with `{ days }`, outputs with
//! `{ amenities }`) are kept as raw TOML values and interpreted by the loader, which also
//! splits tables that combine fixed fields with modifier fields.

use serde::Deserialize;

fn one() -> f64 {
    1.0
}
fn yes() -> bool {
    true
}
fn half() -> f64 {
    0.5
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawAmount {
    pub resource: String,
    pub count: f64,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawMult {
    pub resource: String,
    pub mult: f64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawResource {
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub transient: bool,
    #[serde(default = "yes")]
    pub tradeable: bool,
    #[serde(default = "one")]
    pub base_value: f64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawJob {
    pub display_name: String,
    pub description: Option<String>,
    pub appeal: i32,
    #[serde(default)]
    pub job_tags: Vec<String>,
    #[serde(default)]
    pub associated_resource: Vec<String>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawSpeciesUpkeep {
    #[serde(default)]
    pub upkeep_cost: Vec<RawAmount>,
    pub starvation_resource: Option<String>,
    pub comfort_resource: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawSpeciesGrowth {
    pub growth_rate: Option<f64>,
    pub min_growth: Option<f64>,
    pub starvation_rate: Option<f64>,
    pub migration_rate: Option<f64>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawSpecies {
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub upkeep: RawSpeciesUpkeep,
    #[serde(default)]
    pub growth: RawSpeciesGrowth,
}

/// A `[building.*]` table.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawBuilding {
    pub display_name: String,
    pub description: Option<String>,
    pub max_workers: Option<u32>,
    pub job: Option<String>,
    #[serde(default)]
    pub building_tags: Vec<String>,
    #[serde(default)]
    pub housing: f64,
    /// Resources and `{ days }`.
    #[serde(default)]
    pub build_cost: Vec<toml::Value>,
    /// Resources and `{ days }`; absent means removal is free and instant.
    pub remove_cost: Option<Vec<toml::Value>>,
    pub max_per_planet: Option<u32>,
    pub hp: Option<f64>,
    #[serde(default)]
    pub requires_tech: Vec<String>,
    #[serde(default)]
    pub upkeep_cost: Vec<RawAmount>,
    #[serde(default)]
    pub inputs: Vec<RawAmount>,
    /// Resources and `{ amenities }`; scale with workers.
    #[serde(default)]
    pub outputs: Vec<toml::Value>,
    /// Resources and `{ amenities }`; do not scale with workers.
    #[serde(default)]
    pub passive_outputs: Vec<toml::Value>,
    /// `building` or `building-tag` plus modifier fields; parsed by the loader.
    #[serde(default)]
    pub adjacency_bonus: Vec<toml::Table>,
}

/// The fixed fields of a `[building-upgrade.*]` table. The remaining fields are a
/// [`RawModifier`].
#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawUpgradeHead {
    pub display_name: String,
    pub description: Option<String>,
    /// Buildings the upgrade applies to.
    #[serde(default)]
    pub buildings: Vec<String>,
    /// Building tags the upgrade applies to.
    #[serde(default)]
    pub building_tags: Vec<String>,
    #[serde(default)]
    pub after_upgrades: Vec<String>,
    #[serde(default)]
    pub requires_tech: Vec<String>,
    /// Resources and `{ days }`.
    #[serde(default)]
    pub build_cost: Vec<toml::Value>,
}

pub const UPGRADE_HEAD_KEYS: &[&str] = &[
    "display-name",
    "description",
    "buildings",
    "building-tags",
    "after-upgrades",
    "requires-tech",
    "build-cost",
];

/// Modifier fields shared by upgrades, adjacency bonuses and technology building effects.
/// Additions are applied before multipliers.
#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawModifier {
    #[serde(default)]
    pub upkeep_cost_add: Vec<RawAmount>,
    pub upkeep_cost_mult: Option<f64>,
    #[serde(default)]
    pub input_add: Vec<RawAmount>,
    pub input_mult: Option<f64>,
    /// Resources and `{ amenities }`.
    #[serde(default)]
    pub output_add: Vec<toml::Value>,
    pub output_mult: Option<f64>,
    /// Resources and `{ amenities }`.
    #[serde(default)]
    pub passive_output_add: Vec<toml::Value>,
    pub passive_output_mult: Option<f64>,
    pub max_workers_add: Option<f64>,
    pub max_workers_mult: Option<f64>,
    pub housing_add: Option<f64>,
    pub housing_mult: Option<f64>,
    /// Upgrades only: replaces the building's job.
    pub job: Option<String>,
}

/// Ship stat modifiers, shared by outfits and technology `ships` effects.
#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawShipMods {
    pub hull_add: Option<f64>,
    pub shield_add: Option<f64>,
    pub shield_regen_add: Option<f64>,
    pub base_speed_add: Option<f64>,
    pub hull_mult: Option<f64>,
    pub shield_mult: Option<f64>,
    pub shield_regen_mult: Option<f64>,
    pub base_speed_mult: Option<f64>,
    pub hyperjump_mult: Option<f64>,
    pub fire_rate_mult: Option<f64>,
    pub damage_mult: Option<f64>,
    pub range_mult: Option<f64>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawTechPop {
    pub growth_mult: Option<f64>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawTech {
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub research_resources: Vec<RawAmount>,
    #[serde(default)]
    pub research_tags: Vec<String>,
    #[serde(default)]
    pub prerequisites: Vec<String>,
    #[serde(default)]
    pub pop: RawTechPop,
    /// Ship stat modifiers plus `fleet-size-add` (split by the loader).
    pub ships: Option<toml::Table>,
    /// `field = { value, buildings = [...] }` building modifiers.
    pub buildings: Option<toml::Table>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawWeapon {
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub build_cost: Vec<RawAmount>,
    #[serde(default)]
    pub shield_damage: f64,
    #[serde(default)]
    pub hull_damage: f64,
    /// Days between shots.
    pub fire_rate: f64,
    pub range: f64,
}

/// The fixed fields of a `[ship-outfit.*]`; the remaining fields are [`RawShipMods`].
#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawOutfitHead {
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub build_cost: Vec<RawAmount>,
    #[serde(default)]
    pub upkeep_cost: Vec<RawAmount>,
}

pub const OUTFIT_HEAD_KEYS: &[&str] = &["display-name", "description", "build-cost", "upkeep-cost"];

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawShip {
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub requires_tech: Vec<String>,
    /// Resources and (optionally) `{ days }`.
    #[serde(default)]
    pub build_cost: Vec<toml::Value>,
    #[serde(default)]
    pub upkeep_cost: Vec<RawAmount>,
    pub hull: f64,
    #[serde(default)]
    pub shield: f64,
    pub base_speed: f64,
    #[serde(default)]
    pub shield_regen: f64,
    #[serde(default = "one")]
    pub hyperjump_mult: f64,
    /// Fleet slots the ship takes up.
    #[serde(default = "one_u32")]
    pub fleet_size: u32,
    #[serde(default)]
    pub weapons: Vec<String>,
    #[serde(default)]
    pub outfits: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// Ruleset
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawRulesResources {
    #[serde(default)]
    pub starting_stores: Vec<RawAmount>,
    #[serde(default)]
    pub storage_caps: Vec<RawAmount>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawRulesAmenities {
    pub base_amenities: Option<f64>,
    pub amenities_per_pop: Option<f64>,
    pub base_stability: Option<f64>,
    pub stability_output_factor: Option<f64>,
    pub unemployment_stability_penalty: Option<f64>,
    pub homelessness_stability_penalty: Option<f64>,
    pub comfort_stability_penalty: Option<f64>,
    pub crisis_stability_penalty: Option<f64>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawRulesEconomy {
    pub currency: Option<String>,
    pub logistics_loss: Option<f64>,
    pub market_fee: Option<f64>,
    pub market_reserve: Option<f64>,
    pub scarcity_appeal: Option<f64>,
    pub scarcity_target_days: Option<f64>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawRulesAi {
    pub ai_interval_days: Option<u32>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawRulesTechnology {
    pub research_speed_mult: Option<f64>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawRulesExpansion {
    pub expansion_resource: Option<String>,
    pub expansion_resource_income: Option<f64>,
    pub expansion_base_cost: Option<f64>,
    pub expansion_base_days: Option<f64>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawRulesShips {
    pub hyperjump_days: Option<f64>,
    pub combat_subticks: Option<u32>,
    pub arrival_radius: Option<f64>,
    pub ship_repair_rate: Option<f64>,
    pub base_fleet_size: Option<u32>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawRulesInvasion {
    pub building_hp: Option<f64>,
    pub building_disable_damage: Option<f64>,
    pub building_repair_rate: Option<f64>,
    pub bombardment_amenities_penalty: Option<f64>,
    pub surrender_resistance: Option<f64>,
    pub siege_decay: Option<f64>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawRulesDiplomacy {
    pub truce_days: Option<u64>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawTerraform {
    pub requires_tech: Option<String>,
    pub days: u32,
    #[serde(default)]
    pub cost: Vec<RawAmount>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawRulesTerraforming {
    pub large: Option<RawTerraform>,
    pub small: Option<RawTerraform>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawRuleset {
    pub display_name: String,
    #[serde(default)]
    pub resources: RawRulesResources,
    #[serde(default)]
    pub amenities: RawRulesAmenities,
    #[serde(default)]
    pub economy: RawRulesEconomy,
    #[serde(default)]
    pub ai: RawRulesAi,
    #[serde(default)]
    pub technology: RawRulesTechnology,
    #[serde(default)]
    pub expansion: RawRulesExpansion,
    #[serde(default)]
    pub ships: RawRulesShips,
    #[serde(default)]
    pub invasion: RawRulesInvasion,
    #[serde(default)]
    pub diplomacy: RawRulesDiplomacy,
    #[serde(default)]
    pub terraforming: RawRulesTerraforming,
}

// ---------------------------------------------------------------------------------------------
// Save-file state
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawGame {
    #[serde(default)]
    pub day: u64,
    #[serde(default)]
    pub seed: u64,
    pub ruleset: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawAi {
    #[serde(default = "half")]
    pub militant: f64,
    #[serde(default = "half")]
    pub expansionist: f64,
    #[serde(default = "half")]
    pub scientific: f64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawWar {
    pub empire: String,
    pub since_day: u64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawTruce {
    pub empire: String,
    pub until_day: u64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawResearch {
    pub technology: String,
    /// Defaults to the full research cost.
    pub remaining: Option<Vec<toml::Value>>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawShipbuilding {
    pub ship: String,
    pub system: String,
    pub planet: String,
    /// Normal resources already spent (refunded if cancelled).
    #[serde(default)]
    pub paid: Vec<RawAmount>,
    /// Defaults to the full build cost.
    pub remaining: Option<Vec<toml::Value>>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawEmpire {
    pub display_name: String,
    pub color: Option<String>,
    pub capital: Option<String>,
    pub dominant_species: Option<String>,
    #[serde(default)]
    pub resource_stores: Vec<RawAmount>,
    #[serde(default)]
    pub technologies: Vec<String>,
    #[serde(default)]
    pub current_research: Vec<RawResearch>,
    #[serde(default)]
    pub current_shipbuilding: Vec<RawShipbuilding>,
    #[serde(default)]
    pub policy: RawPolicy,
    #[serde(default)]
    pub diplomacy: RawDiplomacy,
    /// Presence makes the empire AI-run.
    pub ai: Option<RawAi>,
    #[serde(default)]
    pub fleets: toml::Table,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawPolicy {
    #[serde(default = "yes")]
    pub accepts_migrants: bool,
    #[serde(default = "yes")]
    pub market_access: bool,
}

impl Default for RawPolicy {
    fn default() -> Self {
        RawPolicy {
            accepts_migrants: true,
            market_access: true,
        }
    }
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawDiplomacy {
    #[serde(default)]
    pub wars: Vec<RawWar>,
    #[serde(default)]
    pub truces: Vec<RawTruce>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawShipInstance {
    pub ship: String,
    pub hull: Option<f64>,
    pub shield: Option<f64>,
    pub coords: [f64; 2],
    /// Hyperdrive charge, 0 to 1.
    #[serde(default)]
    pub jump_charge: f64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawFleetOrder {
    /// `"move"`, `"jump"` or `"bombard"`.
    pub order: String,
    pub coords: Option<[f64; 2]>,
    pub system: Option<String>,
    pub planet: Option<String>,
    pub tile: Option<usize>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawFleet {
    pub display_name: String,
    pub system: String,
    #[serde(default)]
    pub ships: Vec<RawShipInstance>,
    #[serde(default)]
    pub orders: Vec<RawFleetOrder>,
    #[serde(default)]
    pub in_combat: bool,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawSystem {
    pub display_name: String,
    pub star_type: String,
    #[serde(default = "one_u32")]
    pub star_count: u32,
    pub owner: Option<String>,
    #[serde(default)]
    pub linked_systems: Vec<String>,
    pub coords: [f64; 2],
    #[serde(default)]
    pub planets: toml::Table,
}

fn one_u32() -> u32 {
    1
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RawSize {
    Small,
    Large,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawPopulation {
    pub species: String,
    pub count: u64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawProject {
    #[serde(default)]
    pub paid: Vec<RawAmount>,
    pub remaining: Vec<toml::Value>,
}

/// A planet table without its `tile-N` entries (split off by the loader).
#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawPlanet {
    pub display_name: String,
    pub size: RawSize,
    pub habitable: bool,
    pub coords: [f64; 2],
    pub owner: Option<String>,
    #[serde(default)]
    pub population: Vec<RawPopulation>,
    /// Accumulated bombardment towards surrender.
    #[serde(default)]
    pub siege: f64,
    /// Terraforming in progress.
    pub terraform: Option<RawProject>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawTile {
    #[serde(default)]
    pub resource_multipliers: Vec<RawMult>,
    #[serde(default)]
    pub water: bool,
    /// Either a bare building reference or a [`RawTileBuilding`] table.
    pub building: Option<toml::Value>,
    pub construction: Option<RawConstruction>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawTileBuilding {
    pub building: String,
    #[serde(default)]
    pub upgrades: Vec<String>,
    #[serde(default)]
    pub workers: u32,
    pub job_cap: Option<u32>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Bombardment damage, 0 (intact) to 1 (destroyed).
    #[serde(default)]
    pub damage: f64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawConstruction {
    pub building: Option<String>,
    pub upgrade: Option<String>,
    /// Removing the tile's building (e.g. clearing a blocker).
    #[serde(default)]
    pub remove: bool,
    /// Normal resources already spent (refunded if cancelled).
    #[serde(default)]
    pub paid: Vec<RawAmount>,
    pub remaining: Vec<toml::Value>,
    /// Who is building, when it is not the planet's owner.
    pub empire: Option<String>,
}
