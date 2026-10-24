//! Serde mirrors of the TOML format. References are still unresolved strings here.
//!
//! Every struct denies unknown fields so that typos in mods are reported instead of ignored.
//! Only the current format is accepted: there are no aliases for renamed fields.

use serde::Deserialize;

fn one() -> f64 {
    1.0
}
fn yes() -> bool {
    true
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
#[serde(deny_unknown_fields)]
pub struct RawResource {
    pub display_name: String,
    pub description: Option<String>,
    pub image: Option<String>,
    #[serde(default = "one")]
    pub base_value: f64,
    #[serde(default = "yes")]
    pub tradeable: bool,
    #[serde(default = "yes")]
    pub stockpile: bool,
    pub storage_cap: Option<f64>,
    /// Produced only while something needs it (e.g. shipbuilding capacity).
    #[serde(default)]
    pub on_demand: bool,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawJob {
    pub display_name: String,
    pub description: Option<String>,
    pub appeal: i32,
    #[serde(default)]
    pub upkeep: Vec<RawAmount>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawBuilding {
    /// Defaults to the building's key.
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
    #[serde(default)]
    pub max_workers: u32,
    pub job: Option<String>,
    #[serde(default)]
    pub building_class: Vec<String>,
    #[serde(default)]
    pub upkeep_resources: Vec<RawAmount>,
    #[serde(default)]
    pub input_resources: Vec<RawAmount>,
    #[serde(default)]
    pub output_resources: Vec<RawAmount>,
    #[serde(default)]
    pub housing: f64,
    #[serde(default)]
    pub amenities: f64,
    /// Parsed separately as targeted modifiers.
    #[serde(default)]
    pub adjacency_bonus: Vec<toml::Table>,
    #[serde(default)]
    pub build_cost: Vec<RawAmount>,
    #[serde(default)]
    pub build_days: u32,
    #[serde(default)]
    pub requires_tech: Vec<String>,
    #[serde(default = "yes")]
    pub buildable: bool,
    pub max_per_planet: Option<u32>,
    /// Damage a building absorbs before it is destroyed (default: the ruleset's building_hp).
    pub hp: Option<f64>,
    /// Paid to demolish (or, for natural blockers, clear) the building.
    #[serde(default)]
    pub remove_cost: Vec<RawAmount>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawWeapon {
    pub display_name: Option<String>,
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

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawOutfit {
    pub display_name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub build_cost: Vec<RawAmount>,
    #[serde(default)]
    pub upkeep_cost: Vec<RawAmount>,
    #[serde(default)]
    pub hull: f64,
    #[serde(default)]
    pub shield: f64,
    #[serde(default)]
    pub shield_regen: f64,
    #[serde(default = "one")]
    pub hull_mult: f64,
    #[serde(default = "one")]
    pub shield_mult: f64,
    #[serde(default = "one")]
    pub shield_regen_mult: f64,
    #[serde(default = "one")]
    pub speed_mult: f64,
    #[serde(default = "one")]
    pub hyperjump_mult: f64,
    #[serde(default = "one")]
    pub fire_rate_mult: f64,
    #[serde(default = "one")]
    pub damage_mult: f64,
    #[serde(default = "one")]
    pub range_mult: f64,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawShip {
    pub display_name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub build_cost: Vec<RawAmount>,
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
    #[serde(default)]
    pub weapons: Vec<String>,
    #[serde(default)]
    pub outfits: Vec<String>,
    #[serde(default)]
    pub requires_tech: Vec<String>,
}

/// The modifier fields shared by upgrades, adjacency bonuses and technology modifiers.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawModifier {
    #[serde(default = "one")]
    pub input_mult: f64,
    #[serde(default = "one")]
    pub output_mult: f64,
    #[serde(default = "one")]
    pub upkeep_mult: f64,
    #[serde(default = "one")]
    pub housing_mult: f64,
    #[serde(default = "one")]
    pub amenities_mult: f64,
    #[serde(default)]
    pub input_resources: Vec<RawAmount>,
    #[serde(default)]
    pub output_resources: Vec<RawAmount>,
    #[serde(default)]
    pub upkeep_resources: Vec<RawAmount>,
    #[serde(default)]
    pub housing: f64,
    #[serde(default)]
    pub amenities: f64,
}

/// Non-modifier fields of a `[building-upgrade.*]`.
#[derive(Deserialize, Debug)]
pub struct RawUpgradeHead {
    pub display_name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub apply_to: Vec<String>,
    #[serde(default)]
    pub needs_upgrades: Vec<String>,
    #[serde(default)]
    pub cost: Vec<RawAmount>,
    #[serde(default)]
    pub build_days: u32,
    #[serde(default)]
    pub requires_tech: Vec<String>,
}

pub const UPGRADE_HEAD_KEYS: &[&str] = &[
    "display_name",
    "description",
    "apply_to",
    "needs_upgrades",
    "cost",
    "build_days",
    "requires_tech",
];

/// Target fields of an adjacency bonus or technology modifier.
#[derive(Deserialize, Debug)]
pub struct RawTarget {
    pub building: Option<String>,
    pub building_class: Option<String>,
}

pub const TARGET_KEYS: &[&str] = &["building", "building_class"];

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawTech {
    pub display_name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub cost: f64,
    #[serde(default)]
    pub prerequisites: Vec<String>,
    #[serde(default)]
    pub modifiers: Vec<toml::Table>,
    #[serde(default = "one")]
    pub pop_growth_mult: f64,
    #[serde(default)]
    pub stability: f64,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawTerraform {
    pub requires_tech: Option<String>,
    pub days: u32,
    #[serde(default)]
    pub cost: Vec<RawAmount>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct RawTerraformRules {
    pub large: Option<RawTerraform>,
    pub small: Option<RawTerraform>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawRuleset {
    #[serde(default)]
    pub pop_upkeep: Vec<RawAmount>,
    pub starvation_resource: Option<String>,
    pub comfort_resource: Option<String>,
    pub research_resource: Option<String>,
    pub currency: Option<String>,
    pub pop_growth_rate: Option<f64>,
    pub min_pop_growth: Option<f64>,
    pub starvation_rate: Option<f64>,
    pub migration_rate: Option<f64>,
    pub base_housing_large: Option<f64>,
    pub base_housing_small: Option<f64>,
    pub base_amenities: Option<f64>,
    pub amenities_per_pop: Option<f64>,
    pub base_stability: Option<f64>,
    pub stability_output_factor: Option<f64>,
    pub unemployment_stability_penalty: Option<f64>,
    pub homelessness_stability_penalty: Option<f64>,
    pub comfort_stability_penalty: Option<f64>,
    pub crisis_stability_penalty: Option<f64>,
    pub logistics_loss: Option<f64>,
    pub market_fee: Option<f64>,
    pub market_reserve: Option<f64>,
    pub scarcity_appeal: Option<f64>,
    pub scarcity_target_days: Option<f64>,
    pub ai_interval_days: Option<u32>,
    #[serde(default)]
    pub empire_income: Vec<RawAmount>,
    pub expansion_resource: Option<String>,
    pub expansion_base_cost: Option<f64>,
    pub expansion_base_days: Option<f64>,
    pub hyperjump_days: Option<f64>,
    pub combat_subticks: Option<u32>,
    pub arrival_radius: Option<f64>,
    pub ship_repair_rate: Option<f64>,
    pub building_hp: Option<f64>,
    pub building_disable_damage: Option<f64>,
    pub building_repair_rate: Option<f64>,
    pub bombardment_amenities_penalty: Option<f64>,
    pub surrender_resistance: Option<f64>,
    pub siege_decay: Option<f64>,
    pub truce_days: Option<u64>,
    #[serde(default)]
    pub terraforming: RawTerraformRules,
}

// ---------------------------------------------------------------------------------------------
// Save-file state
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct RawGame {
    #[serde(default)]
    pub day: u64,
    #[serde(default)]
    pub seed: u64,
    pub ruleset: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawCrisis {
    pub resource: String,
    pub since_day: u64,
    #[serde(default)]
    pub severity: f64,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawEmpire {
    pub display_name: String,
    #[serde(default)]
    pub resource_stores: Vec<RawAmount>,
    pub capital: Option<String>,
    #[serde(default)]
    pub technologies: Vec<String>,
    pub researching: Option<String>,
    #[serde(default)]
    pub research_progress: f64,
    /// `[empire.x.ai]`: present means the empire is AI-run.
    pub ai: Option<RawAi>,
    #[serde(default = "yes")]
    pub accepts_migrants: bool,
    #[serde(default = "yes")]
    pub market_access: bool,
    pub color: Option<String>,
    #[serde(default)]
    pub crises: Vec<RawCrisis>,
    #[serde(default)]
    pub wars: Vec<RawWar>,
    #[serde(default)]
    pub truces: Vec<RawTruce>,
    #[serde(default)]
    pub ship_queue: Vec<RawShipOrder>,
    /// `[empire.x.fleets.y]` tables.
    #[serde(default)]
    pub fleets: toml::Table,
}

fn half() -> f64 {
    0.5
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawAi {
    #[serde(default = "half")]
    pub militant: f64,
    #[serde(default = "half")]
    pub expansionist: f64,
    #[serde(default = "half")]
    pub scientific: f64,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawWar {
    pub empire: String,
    #[serde(default)]
    pub since_day: u64,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawTruce {
    pub empire: String,
    pub until_day: u64,
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct RawCoords {
    pub x: f64,
    pub y: f64,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawShipOrder {
    pub ship: String,
    /// Shipyard planet as `system/planet`.
    pub planet: String,
    /// Remaining on-demand resources; defaults to the full amount.
    pub remaining: Option<Vec<RawAmount>>,
    /// Fleet (in the same empire) the ship joins when finished.
    pub fleet: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawShipInstance {
    pub ship: String,
    pub hull: Option<f64>,
    pub shield: Option<f64>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawFleetOrder {
    /// `"idle"`, `"move"` or `"bombard"`.
    #[serde(rename = "type")]
    pub kind: String,
    pub system: Option<String>,
    pub coordinates: Option<RawCoords>,
    /// Target of a bombardment, as `system/planet`.
    pub planet: Option<String>,
    pub tile: Option<usize>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawFleet {
    pub display_name: String,
    #[serde(default)]
    pub ships: Vec<RawShipInstance>,
    /// Current system; defaults to the empire's capital.
    pub system: Option<String>,
    pub coordinates: Option<RawCoords>,
    pub order: Option<RawFleetOrder>,
    #[serde(default)]
    pub jump_charge: f64,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawSystem {
    pub display_name: String,
    #[serde(default = "default_star")]
    pub star_type: String,
    #[serde(default = "default_star_count")]
    pub star_count: u32,
    pub owned_by: Option<String>,
    #[serde(default)]
    pub linked_systems: Vec<String>,
    /// Position relative to the galaxy centre.
    pub coordinates: RawCoords,
    #[serde(default)]
    pub planets: toml::Table,
}

fn default_star() -> String {
    "yellow".into()
}
fn default_star_count() -> u32 {
    1
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RawSize {
    Small,
    Large,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawPlanet {
    pub display_name: String,
    #[serde(default)]
    pub total_pop: u64,
    #[serde(default = "default_size")]
    pub size: RawSize,
    /// Defaults to true for large planets and false for small ones.
    pub habitable: Option<bool>,
    pub moon_of: Option<String>,
    /// Position relative to the star; generated if missing.
    pub coordinates: Option<RawCoords>,
    /// Defaults to the owner of the system.
    pub owned_by: Option<String>,
    /// Accumulated bombardment towards surrender.
    #[serde(default)]
    pub siege: f64,
    #[serde(default)]
    pub growth_progress: f64,
    pub terraform_days_left: Option<u32>,
    #[serde(default)]
    pub tiles: toml::Table,
}

fn default_size() -> RawSize {
    RawSize::Large
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawTile {
    #[serde(default)]
    pub resource_multipliers: Vec<RawMult>,
    /// Whether the tile is covered by water.
    #[serde(default)]
    pub water: bool,
    /// Either a bare building reference or a [`RawTileBuilding`] table.
    pub building: Option<toml::Value>,
    pub construction: Option<RawConstruction>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawTileBuilding {
    #[serde(rename = "type")]
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
#[serde(deny_unknown_fields)]
pub struct RawConstruction {
    pub building: Option<String>,
    pub upgrade: Option<String>,
    pub days_left: u32,
    /// Empire building it (defaults to the planet's owner).
    pub empire: Option<String>,
    /// What was paid, refunded if the site is cancelled by a rival claim.
    #[serde(default)]
    pub paid: Vec<RawAmount>,
}
