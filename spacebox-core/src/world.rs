//! Live game state.
//!
//! Layout is chosen for the tick loop: planets live in one flat `Vec`, stored contiguously per
//! system, so each phase is a single `par_iter_mut` over a slice with read-only access to the
//! level above it.

use std::ops::Range;
use std::sync::Arc;

use crate::defs::{Cost, Defs, ResourceAmount, Ruleset, ShipMods, ShipStats, TargetedModifier};
use crate::geometry::PlanetSize;
use crate::ids::*;
use crate::sim::commands::Command;
use crate::sim::events::Event;
use crate::sim::migration::{Emigrants, Move, Vacancy};
use crate::sim::planet::ProfileCache;

/// Per-resource flow vectors (indexed by `ResourceId`), all in units per day.
#[derive(Clone, Debug, Default)]
pub struct Flows {
    /// Amount actually produced.
    pub supply: Vec<f64>,
    /// Amount actually consumed (throttled by last tick's shortages).
    pub demand: Vec<f64>,
    /// Amount that would be consumed if nothing were short.
    pub demand_full: Vec<f64>,
}

impl Flows {
    pub fn new(resources: usize) -> Self {
        Flows {
            supply: vec![0.0; resources],
            demand: vec![0.0; resources],
            demand_full: vec![0.0; resources],
        }
    }

    pub fn clear(&mut self) {
        self.supply.fill(0.0);
        self.demand.fill(0.0);
        self.demand_full.fill(0.0);
    }

    pub fn add(&mut self, other: &Flows) {
        for (a, b) in self.supply.iter_mut().zip(&other.supply) {
            *a += b;
        }
        for (a, b) in self.demand.iter_mut().zip(&other.demand) {
            *a += b;
        }
        for (a, b) in self.demand_full.iter_mut().zip(&other.demand_full) {
            *a += b;
        }
    }

    pub fn net(&self, r: ResourceId) -> f64 {
        self.supply[r.idx()] - self.demand[r.idx()]
    }
}

#[derive(Clone, Debug)]
pub struct BuildingInstance {
    pub def: BuildingId,
    pub upgrades: Vec<UpgradeId>,
    /// Workers employed as of the last tick.
    pub workers: u32,
    /// Player-set limit on the number of jobs offered.
    pub job_cap: Option<u32>,
    /// If set, `job_cap` workers are assigned here before any appeal-based assignment.
    pub pinned: bool,
    /// Disabled buildings employ nobody, produce nothing and cost no upkeep.
    pub enabled: bool,
    /// Bombardment damage: 0 is intact, 1 is destroyed. Output scales with `1 - damage` and
    /// the building stops entirely past the ruleset's `building_disable_damage`.
    pub damage: f64,
}

impl BuildingInstance {
    pub fn new(def: BuildingId) -> Self {
        BuildingInstance {
            def,
            upgrades: Vec::new(),
            workers: 0,
            job_cap: None,
            pinned: false,
            enabled: true,
            damage: 0.0,
        }
    }
}

/// What a tile construction project produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectKind {
    Building(BuildingId),
    Upgrade(UpgradeId),
    /// Removing the tile's building (e.g. clearing a blocker).
    Remove,
}

/// A construction project on a tile. Its normal resources are drawn from the builder's
/// stockpile as soon as they are available, and its days tick down once per day; it completes
/// once nothing is left.
#[derive(Clone, Debug)]
pub struct Construction {
    pub kind: ProjectKind,
    /// Normal resources spent so far (refunded, except the expansion resource, if cancelled).
    pub paid: Vec<ResourceAmount>,
    pub remaining: Cost,
    /// Who is building. Completing a building on a planet the builder does not own claims it
    /// (and an unowned system along with it). `None`: the planet's owner.
    pub empire: Option<EmpireId>,
}

/// Terraforming in progress on a planet.
#[derive(Clone, Debug)]
pub struct TerraformProject {
    pub paid: Vec<ResourceAmount>,
    pub remaining: Cost,
}

impl Construction {
    pub fn building(&self) -> Option<BuildingId> {
        match self.kind {
            ProjectKind::Building(b) => Some(b),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Tile {
    /// Output multipliers for buildings on this tile.
    pub resource_mults: Vec<(ResourceId, f64)>,
    /// Covered by water: only `water`-class buildings (and unrestricted ones) can stand here,
    /// never `land`-class ones.
    pub water: bool,
    pub building: Option<BuildingInstance>,
    pub construction: Option<Construction>,
}

impl Tile {
    pub fn is_free(&self) -> bool {
        self.building.is_none() && self.construction.is_none()
    }

    pub fn mult(&self, r: ResourceId) -> f64 {
        self.resource_mults
            .iter()
            .filter(|(res, _)| *res == r)
            .map(|(_, m)| m)
            .product()
    }
}

/// Derived per-planet numbers from the last tick, for display and AI decisions.
#[derive(Clone, Debug, Default)]
pub struct PlanetStats {
    pub housing: f64,
    pub amenities: f64,
    pub amenities_needed: f64,
    pub stability: f64,
    pub jobs: u64,
    pub employed: u64,
    pub unemployed: u64,
    pub homeless: u64,
    pub free_jobs: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PlanetFlags {
    pub starving: bool,
    pub unrest: bool,
    /// Day of the last famine announcement (to avoid repeating it).
    pub famine_announced: Option<u64>,
    /// Set by the space phase when fleets bombarded the planet this tick.
    pub bombarded: bool,
}

#[derive(Clone, Debug)]
pub struct Planet {
    pub name: String,
    pub display_name: String,
    pub system: SystemId,
    pub size: PlanetSize,
    pub habitable: bool,
    /// Fixed position relative to the system's star.
    pub coordinates: [f64; 2],
    pub owner: Option<EmpireId>,
    /// Bombardment accumulated towards surrender.
    pub siege: f64,
    /// Pops per species, sorted by species, without zero entries. Use the helper methods to
    /// change it.
    pub population: Vec<(SpeciesId, u64)>,
    /// Terraforming in progress.
    pub terraform: Option<TerraformProject>,
    pub tiles: Vec<Tile>,
    pub stats: PlanetStats,
    pub flows: Flows,
    /// Economic pool and ownership group within the system (see [`World::refresh_topology`]).
    pub pool: PoolId,
    pub(crate) group: u8,
    pub(crate) emigrants: u64,
    pub(crate) vacancy: Option<Vacancy>,
    pub(crate) cache: ProfileCache,
    pub(crate) dirty: bool,
    /// Whether any tile may have a construction project (a cheap pre-check).
    pub(crate) constructing: bool,
    pub(crate) flags: PlanetFlags,
    pub(crate) events: Vec<Event>,
    /// Construction sites completed this tick by an empire that does not own the planet.
    pub(crate) claims: Vec<EmpireId>,
}

impl Planet {
    pub fn new(
        name: impl Into<String>,
        display_name: impl Into<String>,
        size: PlanetSize,
        habitable: bool,
        resources: usize,
    ) -> Self {
        Planet {
            name: name.into(),
            display_name: display_name.into(),
            system: SystemId(0),
            size,
            habitable,
            coordinates: [0.0, 0.0],
            owner: None,
            siege: 0.0,
            population: Vec::new(),
            terraform: None,
            tiles: vec![Tile::default(); size.tile_count()],
            stats: PlanetStats {
                stability: 50.0,
                ..Default::default()
            },
            flows: Flows::new(resources),
            pool: PoolId(0),
            group: 0,
            emigrants: 0,
            vacancy: None,
            cache: ProfileCache::default(),
            dirty: true,
            constructing: true,
            flags: PlanetFlags::default(),
            events: Vec::new(),
            claims: Vec::new(),
        }
    }

    pub fn building_count(&self, def: BuildingId) -> usize {
        self.tiles
            .iter()
            .filter(|t| {
                t.building.as_ref().is_some_and(|b| b.def == def)
                    || t.construction.as_ref().and_then(|c| c.building()) == Some(def)
            })
            .count()
    }

    /// Total pops of every species.
    pub fn pop(&self) -> u64 {
        self.population.iter().map(|(_, n)| n).sum()
    }

    pub fn species_count(&self, species: SpeciesId) -> u64 {
        self.population
            .iter()
            .find(|(s, _)| *s == species)
            .map_or(0, |(_, n)| *n)
    }

    /// The most numerous species (lowest id on ties).
    pub fn main_species(&self) -> Option<SpeciesId> {
        self.population
            .iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            .map(|(s, _)| *s)
    }

    pub fn add_pops(&mut self, species: SpeciesId, n: u64) {
        if n == 0 {
            return;
        }
        match self.population.binary_search_by_key(&species, |(s, _)| *s) {
            Ok(i) => self.population[i].1 += n,
            Err(i) => self.population.insert(i, (species, n)),
        }
    }

    /// Removes up to `n` pops of one species; returns how many were removed.
    pub fn remove_species(&mut self, species: SpeciesId, n: u64) -> u64 {
        let Ok(i) = self.population.binary_search_by_key(&species, |(s, _)| *s) else {
            return 0;
        };
        let taken = n.min(self.population[i].1);
        self.population[i].1 -= taken;
        if self.population[i].1 == 0 {
            self.population.remove(i);
        }
        taken
    }

    /// Removes up to `n` pops spread over the species in proportion to their numbers
    /// (largest remainders first), returning what was removed per species.
    pub fn remove_pops(&mut self, n: u64) -> Vec<(SpeciesId, u64)> {
        let total = self.pop();
        let n = n.min(total);
        if n == 0 {
            return Vec::new();
        }
        let mut shares: Vec<(SpeciesId, u64, u64)> = self
            .population
            .iter()
            .map(|(s, c)| {
                let exact = *c as u128 * n as u128;
                (
                    *s,
                    (exact / total as u128) as u64,
                    (exact % total as u128) as u64,
                )
            })
            .collect();
        let mut left = n - shares.iter().map(|x| x.1).sum::<u64>();
        let mut order: Vec<usize> = (0..shares.len()).collect();
        order.sort_by(|&a, &b| shares[b].2.cmp(&shares[a].2).then(a.cmp(&b)));
        for i in order {
            if left == 0 {
                break;
            }
            let count = self.population[i].1;
            if shares[i].1 < count {
                shares[i].1 += 1;
                left -= 1;
            }
        }
        let removed: Vec<(SpeciesId, u64)> = shares
            .iter()
            .filter(|x| x.1 > 0)
            .map(|x| (x.0, x.1))
            .collect();
        for (s, c) in &removed {
            self.remove_species(*s, *c);
        }
        removed
    }

    pub fn free_tiles(&self) -> impl Iterator<Item = usize> + '_ {
        self.tiles
            .iter()
            .enumerate()
            .filter(|(_, t)| t.is_free())
            .map(|(i, _)| i)
    }

    pub fn is_constructing(&self) -> bool {
        self.tiles.iter().any(|t| t.construction.is_some())
    }

    /// Marks cached building profiles as stale (call after changing tiles directly).
    pub fn invalidate(&mut self) {
        self.dirty = true;
        self.constructing = true;
    }
}

#[derive(Clone, Debug)]
pub struct System {
    pub name: String,
    pub display_name: String,
    pub star_type: String,
    pub star_count: u32,
    /// Only changes when the owner no longer holds any planet in the system.
    pub owner: Option<EmpireId>,
    pub links: Vec<SystemId>,
    /// Position relative to the galaxy centre.
    pub coordinates: [f64; 2],
    pub(crate) planet_start: u32,
    pub(crate) planet_end: u32,
    /// Planets grouped by owner, one group per owner present.
    pub groups: Vec<SystemGroup>,
}

/// The planets of one owner within a system, which balance resources and migrants locally
/// before passing the remainder up to their empire.
#[derive(Clone, Debug)]
pub struct SystemGroup {
    pub owner: Option<EmpireId>,
    pub pool: PoolId,
    pub flows: Flows,
    pub(crate) emigrants: Vec<Emigrants>,
    pub(crate) vacancies: Vec<Vacancy>,
    pub(crate) moves: Vec<Move>,
}

impl SystemGroup {
    pub(crate) fn new(owner: Option<EmpireId>, pool: PoolId, resources: usize) -> Self {
        SystemGroup {
            owner,
            pool,
            flows: Flows::new(resources),
            emigrants: Vec::new(),
            vacancies: Vec::new(),
            moves: Vec::new(),
        }
    }
}

impl System {
    pub fn new(name: impl Into<String>, display_name: impl Into<String>) -> Self {
        System {
            name: name.into(),
            display_name: display_name.into(),
            star_type: "yellow".into(),
            star_count: 1,
            owner: None,
            links: Vec::new(),
            coordinates: [0.0, 0.0],
            planet_start: 0,
            planet_end: 0,
            groups: Vec::new(),
        }
    }

    pub fn planet_range(&self) -> Range<usize> {
        self.planet_start as usize..self.planet_end as usize
    }

    pub fn planet_ids(&self) -> impl Iterator<Item = PlanetId> {
        self.planet_range().map(PlanetId::from_idx)
    }
}

/// A technology being researched (`current-research`).
#[derive(Clone, Debug)]
pub struct ResearchProject {
    pub tech: TechId,
    /// Resources still needed.
    pub remaining: Cost,
}

/// A ship under construction at a planet (`current-shipbuilding`).
#[derive(Clone, Debug)]
pub struct ShipProject {
    pub ship: ShipId,
    /// Where the ship is built. Only this planet's transient output (shipbuilding) goes into
    /// it, and it launches in orbit of the planet.
    pub planet: PlanetId,
    /// Normal resources spent so far (refunded if cancelled).
    pub paid: Vec<ResourceAmount>,
    pub remaining: Cost,
}

#[derive(Clone, Debug)]
pub struct Ship {
    pub def: ShipId,
    pub hull: f64,
    pub shield: f64,
    /// Position relative to the fleet's position (see [`Fleet::coordinates`]).
    pub offset: [f64; 2],
    /// Hyperdrive charge, 0 to 1. Charges only while the fleet waits at the system edge to
    /// carry out a jump order.
    pub jump_charge: f64,
    /// Days until each weapon can fire again.
    pub(crate) cooldown: Vec<f64>,
}

impl Ship {
    /// A freshly built ship at full strength (with `stats` from [`Empire::ship_stats`]).
    pub fn new(def: ShipId, stats: &ShipStats) -> Self {
        Ship {
            def,
            hull: stats.max_hull,
            shield: stats.max_shield,
            offset: [0.0, 0.0],
            jump_charge: 0.0,
            cooldown: vec![0.0; stats.armament.len()],
        }
    }
}

/// Spacing between ships in a fleet formation.
const FORMATION_GAP: f64 = 1.0;

/// Formation offset of the `i`th ship in a fleet: rows of five behind the leader.
pub fn formation_offset(i: usize) -> [f64; 2] {
    let (row, col) = ((i / 5) as f64, (i % 5) as f64 - 2.0);
    [col * FORMATION_GAP, -row * FORMATION_GAP]
}

#[derive(Clone, Debug, PartialEq)]
pub enum FleetOrder {
    /// Fly to a point in the current system.
    Move { coordinates: [f64; 2] },
    /// Fly to the edge of the system facing `system` (an adjacent one), charge the
    /// hyperdrives there and jump.
    Jump { system: SystemId },
    /// Bombard a planet in the current system (a specific tile, or any building) until it
    /// surrenders.
    Bombard {
        planet: PlanetId,
        tile: Option<usize>,
    },
}

#[derive(Clone, Debug)]
pub struct Fleet {
    pub id: FleetId,
    /// Unique key within the owning empire (used in save files).
    pub name: String,
    pub display_name: String,
    pub owner: EmpireId,
    pub system: SystemId,
    /// Position of the fleet relative to the system's star. Ships keep their formation
    /// offsets around it; saves store each ship's absolute coordinates.
    pub coordinates: [f64; 2],
    pub ships: Vec<Ship>,
    /// The order queue: the first order is being carried out; empty means idle.
    pub orders: Vec<FleetOrder>,
    /// Whether enemy fleets are present. Orders are suspended while in combat.
    pub in_combat: bool,
}

impl Fleet {
    pub fn new(
        name: impl Into<String>,
        display_name: impl Into<String>,
        owner: EmpireId,
        system: SystemId,
    ) -> Self {
        Fleet {
            id: FleetId(0),
            name: name.into(),
            display_name: display_name.into(),
            owner,
            system,
            coordinates: [0.0, 0.0],
            ships: Vec::new(),
            orders: Vec::new(),
            in_combat: false,
        }
    }

    pub fn is_idle(&self) -> bool {
        self.orders.is_empty()
    }

    /// The order being carried out.
    pub fn order(&self) -> Option<&FleetOrder> {
        self.orders.first()
    }

    /// The last system the fleet's jump orders lead to (its own system if none).
    pub fn destination(&self) -> SystemId {
        self.orders
            .iter()
            .rev()
            .find_map(|o| match o {
                FleetOrder::Jump { system } => Some(*system),
                _ => None,
            })
            .unwrap_or(self.system)
    }

    /// Fleet slots used (each ship takes its design's `fleet-size`).
    pub fn size(&self, defs: &Defs) -> usize {
        self.ships
            .iter()
            .map(|s| defs.ship(s.def).fleet_size as usize)
            .sum()
    }

    /// The fleet moves at the pace of its slowest ship. `stats` is the owner's
    /// [`Empire::ship_stats`].
    pub fn speed(&self, stats: &[ShipStats]) -> f64 {
        self.ships
            .iter()
            .map(|s| stats[s.def.idx()].speed)
            .fold(f64::INFINITY, f64::min)
            .min(1e9)
    }

    /// Days a ship of this design takes to charge its hyperdrive from 0 to 1.
    pub fn charge_days(stats: &ShipStats, rules: &Ruleset) -> f64 {
        rules.hyperjump_days * stats.hyperjump_mult
    }

    /// Whether every ship's hyperdrive is fully charged.
    pub fn charged(&self) -> bool {
        self.ships.iter().all(|s| s.jump_charge >= 1.0 - 1e-9)
    }

    /// The least-charged ship's charge, 0 to 1 (what the fleet waits for).
    pub fn jump_progress(&self) -> f64 {
        self.ships.iter().map(|s| s.jump_charge).fold(1.0, f64::min)
    }

    pub fn max_range(&self, stats: &[ShipStats]) -> f64 {
        self.ships
            .iter()
            .map(|s| stats[s.def.idx()].max_range)
            .fold(0.0, f64::max)
    }

    /// Absolute position of ship `i` relative to the system's star.
    pub fn ship_coordinates(&self, i: usize) -> [f64; 2] {
        let o = self.ships[i].offset;
        [self.coordinates[0] + o[0], self.coordinates[1] + o[1]]
    }

    /// Lines ships up in formation around the fleet position.
    pub fn reform(&mut self) {
        for (i, s) in self.ships.iter_mut().enumerate() {
            s.offset = formation_offset(i);
        }
    }
}

/// How an AI empire behaves. Every trait is on a 0..=1 scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AiPersonality {
    /// Fleet size, willingness to declare war, how early it attacks and how long it fights.
    pub militant: f64,
    /// How eagerly, how far and how many systems at once it claims.
    pub expansionist: f64,
    /// How highly it prioritises research.
    pub scientific: f64,
}

impl Default for AiPersonality {
    fn default() -> Self {
        AiPersonality {
            militant: 0.5,
            expansionist: 0.5,
            scientific: 0.5,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct War {
    pub enemy: EmpireId,
    pub since_day: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Truce {
    pub with: EmpireId,
    pub until_day: u64,
}

#[derive(Clone, Debug)]
pub struct Crisis {
    pub resource: ResourceId,
    pub since_day: u64,
    /// Fraction of demand going unmet, 0..1.
    pub severity: f64,
}

/// Empire-wide effects derived from known technologies.
#[derive(Clone, Debug, Default)]
pub struct EmpireModifiers {
    pub buildings: Vec<TargetedModifier>,
    pub pop_growth_mult: f64,
    pub fleet_size_add: u32,
    pub ships: ShipMods,
}

#[derive(Clone, Debug, Default)]
pub struct EmpireStats {
    pub pop: u64,
    pub planets: u32,
    pub systems: u32,
}

#[derive(Clone, Debug)]
pub struct Empire {
    pub name: String,
    pub display_name: String,
    pub color: Option<String>,
    /// Stockpiles of normal resources (transient ones are always 0).
    pub storage: Vec<f64>,
    pub capital: Option<SystemId>,
    pub dominant_species: Option<SpeciesId>,
    /// Indexed by `TechId`.
    pub techs: Vec<bool>,
    /// Technologies being researched, funded in order.
    pub research: Vec<ResearchProject>,
    /// Ships under construction, funded in order.
    pub shipbuilding: Vec<ShipProject>,
    /// Wars this empire is fighting (kept symmetric between the two sides).
    pub wars: Vec<War>,
    /// Recent peace treaties; AI empires will not declare war on a truce partner.
    pub truces: Vec<Truce>,
    /// Daily upkeep of all ships, per resource (computed each tick).
    pub fleet_upkeep: Vec<f64>,
    /// The governor's personality, if this empire is AI-run.
    pub ai: Option<AiPersonality>,
    pub accepts_migrants: bool,
    pub market_access: bool,
    /// Stockpiled resources that ran out (recomputed every tick, not saved).
    pub crises: Vec<Crisis>,
    /// Net change in storage last tick, per resource (including trade and projects). For
    /// transient resources: last tick's production.
    pub income: Vec<f64>,
    /// Transient production last tick that no project could use.
    pub wasted: Vec<f64>,
    /// Net galactic market purchases last tick (negative = sold).
    pub trade: Vec<f64>,
    pub core_pool: Option<PoolId>,
    pub modifiers: EmpireModifiers,
    /// Effective stats of every ship design with this empire's technologies, indexed by
    /// `ShipId`.
    pub ship_stats: Vec<ShipStats>,
    pub stats: EmpireStats,
    pub(crate) modifier_version: u64,
    pub(crate) crisis_penalty: f64,
    /// Satisfaction ratios of the core pool computed during settlement.
    pub(crate) sat: Vec<f64>,
    pub(crate) scarcity: Vec<f64>,
    pub(crate) emigrants: Vec<Emigrants>,
    pub(crate) vacancies: Vec<Vacancy>,
    pub(crate) moves: Vec<Move>,
    pub(crate) events: Vec<Event>,
    /// Ships finished this tick, launched after settlement.
    pub(crate) completed_ships: Vec<ShipProject>,
}

impl Empire {
    pub fn new(name: impl Into<String>, display_name: impl Into<String>, defs: &Defs) -> Self {
        let r = defs.resource_count();
        Empire {
            name: name.into(),
            display_name: display_name.into(),
            color: None,
            storage: vec![0.0; r],
            capital: None,
            dominant_species: None,
            techs: vec![false; defs.technologies.len()],
            research: Vec::new(),
            shipbuilding: Vec::new(),
            wars: Vec::new(),
            truces: Vec::new(),
            fleet_upkeep: vec![0.0; r],
            ai: None,
            accepts_migrants: true,
            market_access: true,
            crises: Vec::new(),
            income: vec![0.0; r],
            wasted: vec![0.0; r],
            trade: vec![0.0; r],
            core_pool: None,
            modifiers: EmpireModifiers {
                pop_growth_mult: 1.0,
                ..Default::default()
            },
            ship_stats: defs.ships.iter().map(|s| s.base_stats.clone()).collect(),
            stats: EmpireStats::default(),
            modifier_version: 1,
            crisis_penalty: 0.0,
            sat: vec![1.0; r],
            scarcity: vec![0.0; r],
            emigrants: Vec::new(),
            vacancies: Vec::new(),
            moves: Vec::new(),
            events: Vec::new(),
            completed_ships: Vec::new(),
        }
    }

    pub fn knows(&self, tech: TechId) -> bool {
        self.techs[tech.idx()]
    }

    pub fn at_war_with(&self, other: EmpireId) -> bool {
        self.wars.iter().any(|w| w.enemy == other)
    }

    /// Whether a truce with `other` is still in force on `day`.
    pub fn in_truce(&self, other: EmpireId, day: u64) -> bool {
        self.truces
            .iter()
            .any(|t| t.with == other && t.until_day > day)
    }

    pub fn has_crisis(&self, r: ResourceId) -> bool {
        self.crises.iter().any(|c| c.resource == r)
    }

    pub fn is_researching(&self, tech: TechId) -> bool {
        self.research.iter().any(|p| p.tech == tech)
    }

    /// Most ships one of this empire's fleets can hold.
    pub fn max_fleet_size(&self, rules: &Ruleset) -> usize {
        (rules.base_fleet_size + self.modifiers.fleet_size_add).max(1) as usize
    }

    /// Recomputes `modifiers` and `ship_stats` from known technologies. Call after editing
    /// `techs` directly.
    pub fn rebuild_modifiers(&mut self, defs: &Defs) {
        let mut m = EmpireModifiers {
            pop_growth_mult: 1.0,
            ..Default::default()
        };
        for (i, known) in self.techs.iter().enumerate() {
            if *known {
                let t = &defs.technologies[i];
                m.buildings.extend(t.buildings.iter().cloned());
                m.pop_growth_mult *= t.pop_growth_mult;
                m.fleet_size_add += t.fleet_size_add;
                m.ships.combine(&t.ships);
            }
        }
        self.ship_stats = defs.ships.iter().map(|s| s.stats(defs, &m.ships)).collect();
        self.modifiers = m;
        self.modifier_version += 1;
    }
}

/// A connected group of systems that share an economy.
///
/// An empire is *present* in a system if it owns the system or any planet in it. Its core
/// pool is the set of present systems connected to its capital through hyperlanes between
/// present systems; it has access to empire storage and the galactic market. Present systems
/// cut off from the capital form isolated pools, and the unowned planets of each system form
/// their own pool.
#[derive(Clone, Debug)]
pub struct Pool {
    pub empire: Option<EmpireId>,
    pub systems: Vec<SystemId>,
    /// `(system, group index)` pairs whose planets belong to this pool.
    pub members: Vec<(SystemId, u8)>,
    pub planets: Vec<PlanetId>,
    pub core: bool,
    pub hub: Option<SystemId>,
    /// Fraction of full demand that could be met last tick, per resource.
    pub sat: Vec<f64>,
    /// 0 when well stocked, 1 when stock is gone, up to 2 when demand goes unmet. Raises the
    /// appeal of jobs producing the resource.
    pub scarcity: Vec<f64>,
    /// Net production after logistics losses.
    pub net: Vec<f64>,
    pub demand: Vec<f64>,
    pub demand_full: Vec<f64>,
}

impl Pool {
    pub(crate) fn new(empire: Option<EmpireId>, resources: usize) -> Self {
        Pool {
            empire,
            systems: Vec::new(),
            members: Vec::new(),
            planets: Vec::new(),
            core: false,
            hub: None,
            sat: vec![1.0; resources],
            scarcity: vec![0.0; resources],
            net: vec![0.0; resources],
            demand: vec![0.0; resources],
            demand_full: vec![0.0; resources],
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MarketEntry {
    /// Clearing price last tick, in currency per unit.
    pub price: f64,
    /// Units traded last tick.
    pub volume: f64,
}

#[derive(Debug)]
pub struct World {
    pub defs: Arc<Defs>,
    pub ruleset: RulesetId,
    pub day: u64,
    pub seed: u64,
    pub empires: Vec<Empire>,
    pub systems: Vec<System>,
    pub planets: Vec<Planet>,
    pub pools: Vec<Pool>,
    pub fleets: Vec<Fleet>,
    pub market: Vec<MarketEntry>,
    /// The file the world was loaded from, if any.
    pub root_file: Option<FileId>,
    pub(crate) next_fleet_id: u32,
    pub(crate) queue: Vec<Command>,
    pub(crate) events: Vec<Event>,
    pub(crate) topology_dirty: bool,
    pub(crate) empire_systems: Vec<Vec<SystemId>>,
    pub(crate) empire_planets: Vec<Vec<PlanetId>>,
    /// Systems where each empire owns the system or any planet.
    pub(crate) empire_presence: Vec<Vec<SystemId>>,
    pub(crate) galaxy_moves: Vec<Move>,
}

impl World {
    pub fn new(defs: Arc<Defs>, ruleset: RulesetId) -> Self {
        let r = defs.resource_count();
        World {
            defs,
            ruleset,
            day: 0,
            seed: 0,
            empires: Vec::new(),
            systems: Vec::new(),
            planets: Vec::new(),
            pools: Vec::new(),
            fleets: Vec::new(),
            market: vec![MarketEntry::default(); r],
            root_file: None,
            next_fleet_id: 1,
            queue: Vec::new(),
            events: Vec::new(),
            topology_dirty: true,
            empire_systems: Vec::new(),
            empire_planets: Vec::new(),
            empire_presence: Vec::new(),
            galaxy_moves: Vec::new(),
        }
    }

    pub fn rules(&self) -> &Ruleset {
        self.defs.ruleset(self.ruleset)
    }

    pub fn add_empire(&mut self, mut empire: Empire) -> EmpireId {
        empire.rebuild_modifiers(&self.defs);
        self.empires.push(empire);
        self.topology_dirty = true;
        EmpireId::from_idx(self.empires.len() - 1)
    }

    /// Adds a system together with its planets (planets are stored contiguously per system).
    pub fn add_system(&mut self, mut system: System, planets: Vec<Planet>) -> SystemId {
        let id = SystemId::from_idx(self.systems.len());
        system.planet_start = self.planets.len() as u32;
        for mut p in planets {
            p.system = id;
            self.planets.push(p);
        }
        system.planet_end = self.planets.len() as u32;
        self.systems.push(system);
        self.topology_dirty = true;
        id
    }

    /// Splits the flat planet list back into per-system lists, tagging each planet with its
    /// current id.
    fn take_layout(&mut self) -> Vec<(System, Vec<(PlanetId, Planet)>)> {
        let mut planets = std::mem::take(&mut self.planets).into_iter().enumerate();
        std::mem::take(&mut self.systems)
            .into_iter()
            .map(|s| {
                let n = s.planet_range().len();
                let ps = planets
                    .by_ref()
                    .take(n)
                    .map(|(i, p)| (PlanetId::from_idx(i), p))
                    .collect();
                (s, ps)
            })
            .collect()
    }

    /// Rebuilds the flat layout after a structural edit and remaps every stored planet and
    /// system id. `system_map` gives each old system's new id (`None` if removed). Queued
    /// commands refer to old ids, so they are dropped.
    fn put_layout(
        &mut self,
        layout: Vec<(System, Vec<(PlanetId, Planet)>)>,
        old_planets: usize,
        system_map: impl Fn(SystemId) -> Option<SystemId>,
    ) {
        let mut planet_map: Vec<Option<PlanetId>> = vec![None; old_planets];
        let mut next = 0usize;
        for (_, ps) in &layout {
            for (old, _) in ps {
                if old.idx() < old_planets {
                    planet_map[old.idx()] = Some(PlanetId::from_idx(next));
                }
                next += 1;
            }
        }
        for (mut s, ps) in layout {
            s.links = s.links.iter().filter_map(|l| system_map(*l)).collect();
            self.add_system(s, ps.into_iter().map(|(_, p)| p).collect());
        }
        let pmap = |p: PlanetId| planet_map.get(p.idx()).copied().flatten();
        for e in &mut self.empires {
            e.capital = e.capital.and_then(&system_map);
            e.shipbuilding.retain_mut(|o| match pmap(o.planet) {
                Some(p) => {
                    o.planet = p;
                    true
                }
                None => false,
            });
        }
        self.fleets.retain_mut(|f| match system_map(f.system) {
            Some(s) => {
                f.system = s;
                true
            }
            None => false,
        });
        for f in &mut self.fleets {
            // Orders that refer to removed systems or planets end the queue there.
            let mut kept = Vec::new();
            for o in std::mem::take(&mut f.orders) {
                let o = match o {
                    FleetOrder::Move { coordinates } => Some(FleetOrder::Move { coordinates }),
                    FleetOrder::Jump { system } => {
                        system_map(system).map(|system| FleetOrder::Jump { system })
                    }
                    FleetOrder::Bombard { planet, tile } => {
                        pmap(planet).map(|planet| FleetOrder::Bombard { planet, tile })
                    }
                };
                match o {
                    Some(o) => kept.push(o),
                    None => break,
                }
            }
            f.orders = kept;
        }
        self.queue.clear();
        self.galaxy_moves.clear();
        self.refresh_topology();
    }

    /// Removes a system with its planets and fleets. Later system and planet ids shift down.
    pub fn remove_system(&mut self, id: SystemId) -> Result<(), String> {
        if id.idx() >= self.systems.len() {
            return Err(format!("no system with id {}", id.0));
        }
        let n = self.planets.len();
        let mut layout = self.take_layout();
        layout.remove(id.idx());
        self.put_layout(layout, n, |s: SystemId| match s.cmp(&id) {
            std::cmp::Ordering::Less => Some(s),
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(SystemId(s.0 - 1)),
        });
        Ok(())
    }

    /// Adds a planet to an existing system. Planet ids after it shift up by one.
    pub fn add_planet(&mut self, system: SystemId, mut planet: Planet) -> Result<PlanetId, String> {
        if system.idx() >= self.systems.len() {
            return Err(format!("no system with id {}", system.0));
        }
        planet.dirty = true;
        let n = self.planets.len();
        let mut layout = self.take_layout();
        layout[system.idx()].1.push((PlanetId(u32::MAX), planet));
        self.put_layout(layout, n, Some);
        Ok(PlanetId(self.systems[system.idx()].planet_end - 1))
    }

    /// Removes a planet. Planet ids after it shift down by one.
    pub fn remove_planet(&mut self, planet: PlanetId) -> Result<(), String> {
        if planet.idx() >= self.planets.len() {
            return Err(format!("no planet with id {}", planet.0));
        }
        let sys = self.planets[planet.idx()].system;
        let offset = planet.idx() - self.systems[sys.idx()].planet_start as usize;
        let n = self.planets.len();
        let mut layout = self.take_layout();
        layout[sys.idx()].1.remove(offset);
        self.put_layout(layout, n, Some);
        Ok(())
    }

    /// Forces ownership lists and economic pools to be recomputed before the next tick.
    pub fn mark_topology_dirty(&mut self) {
        self.topology_dirty = true;
    }

    /// Adds an undirected hyperlane.
    pub fn link(&mut self, a: SystemId, b: SystemId) {
        if a == b {
            return;
        }
        if !self.systems[a.idx()].links.contains(&b) {
            self.systems[a.idx()].links.push(b);
        }
        if !self.systems[b.idx()].links.contains(&a) {
            self.systems[b.idx()].links.push(a);
        }
        self.topology_dirty = true;
    }

    pub fn planet(&self, id: PlanetId) -> &Planet {
        &self.planets[id.idx()]
    }
    pub fn planet_mut(&mut self, id: PlanetId) -> &mut Planet {
        &mut self.planets[id.idx()]
    }
    pub fn system(&self, id: SystemId) -> &System {
        &self.systems[id.idx()]
    }
    pub fn empire(&self, id: EmpireId) -> &Empire {
        &self.empires[id.idx()]
    }

    pub fn planet_owner(&self, id: PlanetId) -> Option<EmpireId> {
        self.planets[id.idx()].owner
    }

    /// Effective stats of a ship design for an empire.
    pub fn ship_stats(&self, empire: EmpireId, ship: ShipId) -> &ShipStats {
        &self.empires[empire.idx()].ship_stats[ship.idx()]
    }

    pub fn at_war(&self, a: EmpireId, b: EmpireId) -> bool {
        a != b && self.empires[a.idx()].at_war_with(b)
    }

    /// Declares war (`true`) or makes peace between two empires. Peace starts a truce of the
    /// ruleset's `truce_days` on both sides.
    pub fn set_war(&mut self, a: EmpireId, b: EmpireId, war: bool) {
        if a == b {
            return;
        }
        let day = self.day;
        let truce = self.rules().truce_days;
        for (x, y) in [(a, b), (b, a)] {
            let e = &mut self.empires[x.idx()];
            e.wars.retain(|w| w.enemy != y);
            e.truces.retain(|t| t.with != y && t.until_day > day);
            if war {
                e.wars.push(War {
                    enemy: y,
                    since_day: day,
                });
                e.wars.sort_by_key(|w| w.enemy);
            } else {
                e.truces.push(Truce {
                    with: y,
                    until_day: day + truce,
                });
            }
        }
    }

    pub fn fleet(&self, id: FleetId) -> Option<&Fleet> {
        self.fleets.iter().find(|f| f.id == id)
    }

    pub fn fleet_mut(&mut self, id: FleetId) -> Option<&mut Fleet> {
        self.fleets.iter_mut().find(|f| f.id == id)
    }

    /// Adds a fleet, assigning it a fresh id (and a unique name within its empire).
    pub fn add_fleet(&mut self, mut fleet: Fleet) -> FleetId {
        let id = FleetId(self.next_fleet_id);
        self.next_fleet_id += 1;
        fleet.id = id;
        let taken = |n: &str| {
            self.fleets
                .iter()
                .any(|f| f.owner == fleet.owner && f.name == n)
        };
        if fleet.name.is_empty() || taken(&fleet.name) {
            let mut k = id.0;
            while taken(&format!("fleet-{k}")) {
                k += 1;
            }
            fleet.name = format!("fleet-{k}");
        }
        self.fleets.push(fleet);
        id
    }

    pub fn remove_fleet(&mut self, id: FleetId) -> bool {
        let before = self.fleets.len();
        self.fleets.retain(|f| f.id != id);
        before != self.fleets.len()
    }

    /// Changes a planet's owner and applies the system ownership rule: a system only changes
    /// hands once its owner holds none of its planets.
    pub fn set_planet_owner(&mut self, planet: PlanetId, owner: Option<EmpireId>) {
        let p = &mut self.planets[planet.idx()];
        p.owner = owner;
        p.siege = 0.0;
        p.dirty = true;
        let sys = p.system;
        self.update_system_owner(sys);
        self.topology_dirty = true;
    }

    /// Re-evaluates who owns a system after its planets changed hands.
    pub fn update_system_owner(&mut self, system: SystemId) {
        let s = &self.systems[system.idx()];
        let owners: Vec<Option<EmpireId>> = s
            .planet_ids()
            .map(|p| self.planets[p.idx()].owner)
            .collect();
        if owners.is_empty() || (s.owner.is_some() && owners.contains(&s.owner)) {
            return;
        }
        let mut counts: Vec<(EmpireId, usize)> = Vec::new();
        for o in owners.into_iter().flatten() {
            match counts.iter_mut().find(|(e, _)| *e == o) {
                Some((_, n)) => *n += 1,
                None => counts.push((o, 1)),
            }
        }
        let new = counts
            .iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            .map(|(e, _)| *e);
        self.systems[system.idx()].owner = new;
        self.topology_dirty = true;
    }

    /// Breadth-first hyperlane distances from a set of systems (`u32::MAX` if unreachable).
    pub fn distances_from(&self, starts: impl IntoIterator<Item = SystemId>) -> Vec<u32> {
        let mut dist = vec![u32::MAX; self.systems.len()];
        let mut queue = std::collections::VecDeque::new();
        for s in starts {
            if dist[s.idx()] != 0 {
                dist[s.idx()] = 0;
                queue.push_back(s);
            }
        }
        while let Some(s) = queue.pop_front() {
            for &n in &self.systems[s.idx()].links {
                if dist[n.idx()] == u32::MAX {
                    dist[n.idx()] = dist[s.idx()] + 1;
                    queue.push_back(n);
                }
            }
        }
        dist
    }

    /// Hyperlane jumps from `empire`'s borders (its owned systems) to every system.
    pub fn jumps_from_borders(&self, empire: EmpireId) -> Vec<u32> {
        let owned = self
            .systems
            .iter()
            .enumerate()
            .filter(|(_, s)| s.owner == Some(empire))
            .map(|(i, _)| SystemId::from_idx(i));
        self.distances_from(owned)
    }

    /// The systems along a shortest hyperlane route from `from` to `to` (excluding `from`):
    /// empty if they are the same, `None` if unreachable.
    pub fn route(&self, from: SystemId, to: SystemId) -> Option<Vec<SystemId>> {
        let dist = self.distances_from([to]);
        if dist[from.idx()] == u32::MAX {
            return None;
        }
        let mut path = Vec::new();
        let mut at = from;
        while at != to {
            at = self.systems[at.idx()]
                .links
                .iter()
                .copied()
                .filter(|n| dist[n.idx()] < dist[at.idx()])
                .min_by_key(|n| (dist[n.idx()], *n))?;
            path.push(at);
        }
        Some(path)
    }

    /// Jump orders along the shortest route to `to` (empty if already there).
    pub fn route_orders(&self, from: SystemId, to: SystemId) -> Option<Vec<FleetOrder>> {
        self.route(from, to).map(|p| {
            p.into_iter()
                .map(|system| FleetOrder::Jump { system })
                .collect()
        })
    }

    /// The first hop on a shortest hyperlane route, or `None` if unreachable or already there.
    pub fn next_hop(&self, from: SystemId, to: SystemId) -> Option<SystemId> {
        if from == to {
            return None;
        }
        let dist = self.distances_from([to]);
        self.systems[from.idx()]
            .links
            .iter()
            .copied()
            .filter(|n| dist[n.idx()] != u32::MAX)
            .min_by_key(|n| (dist[n.idx()], *n))
            .filter(|n| dist[n.idx()] < dist[from.idx()])
    }

    /// Planets owned by `empire` (valid after the first tick or [`World::refresh_topology`]).
    pub fn empire_planets(&self, empire: EmpireId) -> &[PlanetId] {
        self.empire_planets
            .get(empire.idx())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn empire_by_name(&self, name: &str) -> Option<EmpireId> {
        self.empires
            .iter()
            .position(|e| e.name == name)
            .map(EmpireId::from_idx)
    }

    pub fn system_by_name(&self, name: &str) -> Option<SystemId> {
        self.systems
            .iter()
            .position(|s| s.name == name)
            .map(SystemId::from_idx)
    }

    /// Looks up a planet as `system/planet` or by bare planet name.
    pub fn planet_by_name(&self, name: &str) -> Option<PlanetId> {
        if let Some((sys, planet)) = name.split_once('/') {
            let s = self.system(self.system_by_name(sys)?);
            s.planet_ids().find(|p| self.planet(*p).name == planet)
        } else {
            self.planets
                .iter()
                .position(|p| p.name == name)
                .map(PlanetId::from_idx)
        }
    }

    /// Systems owned by `empire` (valid after the first tick or [`World::refresh_topology`]).
    pub fn empire_systems(&self, empire: EmpireId) -> &[SystemId] {
        self.empire_systems
            .get(empire.idx())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Queues a command to be applied at the start of the next tick.
    pub fn queue(&mut self, command: Command) {
        self.queue.push(command);
    }

    pub fn pending_commands(&self) -> &[Command] {
        &self.queue
    }

    /// Takes all events produced since the last call.
    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
}
