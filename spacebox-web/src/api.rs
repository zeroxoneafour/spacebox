//! REST handlers. Every handler runs on a blocking thread with exclusive access to the engine.
//!
//! Mutating endpoints return `{ "ok": true, ... }`; clients then call `POST /api/sync` to pull
//! the resulting changes as a diff.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use spacebox_core::generate::{self, GalaxyParams};
use spacebox_core::geometry::PlanetSize;
use spacebox_core::world::{
    AiPersonality, BuildingInstance, Construction, Empire, Fleet, FleetOrder, Planet, Ship,
};
use spacebox_core::{
    BuildingId, Command, EmpireId, FleetId, PlanetId, ResourceId, ShipId, SystemId, TechId,
    UpgradeId, World, load_assets, load_world, save,
};

use crate::engine::{Engine, Known};

pub type Shared = Arc<Mutex<Engine>>;

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<String> for ApiError {
    fn from(s: String) -> Self {
        ApiError(StatusCode::BAD_REQUEST, s)
    }
}

impl From<&str> for ApiError {
    fn from(s: &str) -> Self {
        ApiError(StatusCode::BAD_REQUEST, s.to_string())
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

/// Runs `f` with the engine locked, off the async executor.
async fn run<F>(state: Shared, f: F) -> ApiResult
where
    F: FnOnce(&mut Engine) -> Result<Value, ApiError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        // A panic in an earlier request must not take the whole server down.
        let mut eng = state.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut eng)
    })
    .await
    .map_err(|e| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("engine task failed: {e}"),
        )
    })?
    .map(Json)
}

fn ok() -> Value {
    json!({ "ok": true })
}

// ---------------------------------------------------------------------------------------------
// Id validation (the engine indexes directly, so ids from clients are checked here)
// ---------------------------------------------------------------------------------------------

fn check(id: u32, len: usize, what: &str) -> Result<u32, ApiError> {
    if (id as usize) < len {
        Ok(id)
    } else {
        Err(format!("no {what} with id {id}").into())
    }
}

fn planet(w: &World, id: u32) -> Result<PlanetId, ApiError> {
    check(id, w.planets.len(), "planet").map(PlanetId)
}
fn system(w: &World, id: u32) -> Result<SystemId, ApiError> {
    check(id, w.systems.len(), "system").map(SystemId)
}
fn empire(w: &World, id: u32) -> Result<EmpireId, ApiError> {
    check(id, w.empires.len(), "empire").map(EmpireId)
}
fn resource(w: &World, id: u32) -> Result<ResourceId, ApiError> {
    check(id, w.defs.resources.len(), "resource").map(ResourceId)
}
fn building(w: &World, id: u32) -> Result<BuildingId, ApiError> {
    check(id, w.defs.buildings.len(), "building").map(BuildingId)
}
fn upgrade(w: &World, id: u32) -> Result<UpgradeId, ApiError> {
    check(id, w.defs.upgrades.len(), "upgrade").map(UpgradeId)
}
fn tech(w: &World, id: u32) -> Result<TechId, ApiError> {
    check(id, w.defs.technologies.len(), "technology").map(TechId)
}
fn tile(w: &World, p: PlanetId, t: usize) -> Result<usize, ApiError> {
    if t < w.planet(p).tiles.len() {
        Ok(t)
    } else {
        Err(format!("{} has no tile {t}", w.planet(p).display_name).into())
    }
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

pub async fn get_defs(State(s): State<Shared>) -> ApiResult {
    run(s, |eng| {
        let mut d = (*eng.defs_json()).clone();
        d["epoch"] = json!(eng.epoch);
        Ok(d)
    })
    .await
}

pub async fn get_state(State(s): State<Shared>) -> ApiResult {
    run(s, |eng| Ok(eng.full())).await
}

#[derive(Deserialize, Default)]
pub struct TickReq {
    #[serde(default = "one")]
    days: u64,
    #[serde(flatten)]
    known: Known,
}

fn one() -> u64 {
    1
}

pub async fn tick(State(s): State<Shared>, body: Option<Json<TickReq>>) -> ApiResult {
    let req = body.map(|b| b.0).unwrap_or(TickReq {
        days: 1,
        ..Default::default()
    });
    run(s, move |eng| {
        eng.tick(req.days.min(3650));
        Ok(eng.sync(&req.known))
    })
    .await
}

pub async fn sync(State(s): State<Shared>, body: Option<Json<Known>>) -> ApiResult {
    let known = body.map(|b| b.0).unwrap_or_default();
    run(s, move |eng| Ok(eng.sync(&known))).await
}

// ---------------------------------------------------------------------------------------------
// Engine commands (normal game rules)
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Cmd {
    MovePops {
        from: u32,
        to: u32,
        count: u64,
    },
    AssignWorkers {
        planet: u32,
        tile: usize,
        workers: Option<u32>,
    },
    SetJobCap {
        planet: u32,
        tile: usize,
        cap: Option<u32>,
    },
    SetEnabled {
        planet: u32,
        tile: usize,
        enabled: bool,
    },
    Build {
        planet: u32,
        tile: usize,
        building: u32,
        /// Required when building in an unowned system (an expansion claim).
        empire: Option<u32>,
    },
    Demolish {
        planet: u32,
        tile: usize,
    },
    Upgrade {
        planet: u32,
        tile: usize,
        upgrade: u32,
    },
    Terraform {
        planet: u32,
    },
    SetResearch {
        empire: u32,
        tech: Option<u32>,
    },
    SetOwner {
        system: u32,
        owner: Option<u32>,
    },
    SetCapital {
        empire: u32,
        system: u32,
    },
    Link {
        a: u32,
        b: u32,
    },
    Unlink {
        a: u32,
        b: u32,
    },
    GrantResources {
        empire: u32,
        resource: u32,
        count: f64,
    },
    SetPlanetOwner {
        planet: u32,
        owner: Option<u32>,
    },
    BuildShip {
        planet: u32,
        ship: u32,
        fleet: Option<u32>,
    },
    CancelShipOrder {
        empire: u32,
        index: usize,
    },
    MoveFleet {
        fleet: u32,
        system: u32,
        coordinates: Option<[f64; 2]>,
    },
    Bombard {
        fleet: u32,
        planet: u32,
        tile: Option<usize>,
    },
    StopFleet {
        fleet: u32,
    },
    MergeFleets {
        into: u32,
        from: u32,
    },
    SplitFleet {
        fleet: u32,
        ships: Vec<usize>,
    },
    RenameFleet {
        fleet: u32,
        display_name: String,
    },
    DisbandFleet {
        fleet: u32,
    },
    DeclareWar {
        empire: u32,
        target: u32,
    },
    MakePeace {
        empire: u32,
        target: u32,
    },
}

fn ship(w: &World, id: u32) -> Result<ShipId, ApiError> {
    check(id, w.defs.ships.len(), "ship design").map(ShipId)
}

fn to_command(w: &World, c: Cmd) -> Result<Command, ApiError> {
    Ok(match c {
        Cmd::MovePops { from, to, count } => Command::MovePops {
            from: planet(w, from)?,
            to: planet(w, to)?,
            count,
        },
        Cmd::AssignWorkers {
            planet: p,
            tile: t,
            workers,
        } => {
            let p = planet(w, p)?;
            Command::AssignWorkers {
                planet: p,
                tile: tile(w, p, t)?,
                workers,
            }
        }
        Cmd::SetJobCap {
            planet: p,
            tile: t,
            cap,
        } => {
            let p = planet(w, p)?;
            Command::SetJobCap {
                planet: p,
                tile: tile(w, p, t)?,
                cap,
            }
        }
        Cmd::SetEnabled {
            planet: p,
            tile: t,
            enabled,
        } => {
            let p = planet(w, p)?;
            Command::SetEnabled {
                planet: p,
                tile: tile(w, p, t)?,
                enabled,
            }
        }
        Cmd::Build {
            planet: p,
            tile: t,
            building: b,
            empire: e,
        } => {
            let p = planet(w, p)?;
            Command::Build {
                planet: p,
                tile: tile(w, p, t)?,
                building: building(w, b)?,
                empire: e.map(|e| empire(w, e)).transpose()?,
            }
        }
        Cmd::Demolish { planet: p, tile: t } => {
            let p = planet(w, p)?;
            Command::Demolish {
                planet: p,
                tile: tile(w, p, t)?,
            }
        }
        Cmd::Upgrade {
            planet: p,
            tile: t,
            upgrade: u,
        } => {
            let p = planet(w, p)?;
            Command::Upgrade {
                planet: p,
                tile: tile(w, p, t)?,
                upgrade: upgrade(w, u)?,
            }
        }
        Cmd::Terraform { planet: p } => Command::Terraform {
            planet: planet(w, p)?,
        },
        Cmd::SetResearch { empire: e, tech: t } => Command::SetResearch {
            empire: empire(w, e)?,
            tech: t.map(|t| tech(w, t)).transpose()?,
        },
        Cmd::SetOwner { system: s, owner } => Command::SetOwner {
            system: system(w, s)?,
            owner: owner.map(|o| empire(w, o)).transpose()?,
        },
        Cmd::SetCapital {
            empire: e,
            system: s,
        } => Command::SetCapital {
            empire: empire(w, e)?,
            system: system(w, s)?,
        },
        Cmd::Link { a, b } => Command::Link {
            a: system(w, a)?,
            b: system(w, b)?,
        },
        Cmd::Unlink { a, b } => Command::Unlink {
            a: system(w, a)?,
            b: system(w, b)?,
        },
        Cmd::GrantResources {
            empire: e,
            resource: r,
            count,
        } => Command::GrantResources {
            empire: empire(w, e)?,
            resource: resource(w, r)?,
            count,
        },
        Cmd::SetPlanetOwner { planet: p, owner } => Command::SetPlanetOwner {
            planet: planet(w, p)?,
            owner: owner.map(|o| empire(w, o)).transpose()?,
        },
        Cmd::BuildShip {
            planet: p,
            ship: s,
            fleet,
        } => Command::BuildShip {
            planet: planet(w, p)?,
            ship: ship(w, s)?,
            fleet: fleet.map(FleetId),
        },
        Cmd::CancelShipOrder { empire: e, index } => Command::CancelShipOrder {
            empire: empire(w, e)?,
            index,
        },
        Cmd::MoveFleet {
            fleet,
            system: s,
            coordinates,
        } => Command::MoveFleet {
            fleet: FleetId(fleet),
            system: system(w, s)?,
            coordinates,
        },
        Cmd::Bombard {
            fleet,
            planet: p,
            tile,
        } => Command::Bombard {
            fleet: FleetId(fleet),
            planet: planet(w, p)?,
            tile,
        },
        Cmd::StopFleet { fleet } => Command::StopFleet {
            fleet: FleetId(fleet),
        },
        Cmd::MergeFleets { into, from } => Command::MergeFleets {
            into: FleetId(into),
            from: FleetId(from),
        },
        Cmd::SplitFleet { fleet, ships } => Command::SplitFleet {
            fleet: FleetId(fleet),
            ships,
        },
        Cmd::RenameFleet {
            fleet,
            display_name,
        } => Command::RenameFleet {
            fleet: FleetId(fleet),
            display_name,
        },
        Cmd::DisbandFleet { fleet } => Command::DisbandFleet {
            fleet: FleetId(fleet),
        },
        Cmd::DeclareWar {
            empire: a,
            target: b,
        } => Command::DeclareWar {
            empire: empire(w, a)?,
            target: empire(w, b)?,
        },
        Cmd::MakePeace {
            empire: a,
            target: b,
        } => Command::MakePeace {
            empire: empire(w, a)?,
            target: empire(w, b)?,
        },
    })
}

/// Applies an engine command immediately, with the normal game rules (costs, tech, etc.).
pub async fn command(State(s): State<Shared>, Json(cmd): Json<Cmd>) -> ApiResult {
    run(s, move |eng| {
        let cmd = to_command(&eng.world, cmd)?;
        eng.world.apply_now(cmd)?;
        Ok(ok())
    })
    .await
}

// ---------------------------------------------------------------------------------------------
// Empires
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewEmpire {
    display_name: String,
    /// AI personality; omit (or null) for a player-run empire.
    ai: Option<AiTraits>,
    color: Option<String>,
    /// System to claim as the capital.
    capital: Option<u32>,
    /// Seed storage with the generator's starting stockpile.
    #[serde(default = "yes")]
    starting_resources: bool,
}

fn yes() -> bool {
    true
}

/// `{ militant, expansionist, scientific }`, each 0..1 (missing traits default to 0.5).
#[derive(Deserialize)]
pub struct AiTraits {
    #[serde(default = "half")]
    militant: f64,
    #[serde(default = "half")]
    expansionist: f64,
    #[serde(default = "half")]
    scientific: f64,
}

fn half() -> f64 {
    0.5
}

impl AiTraits {
    fn personality(&self) -> Result<AiPersonality, ApiError> {
        for (name, v) in [
            ("militant", self.militant),
            ("expansionist", self.expansionist),
            ("scientific", self.scientific),
        ] {
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("{name} must be between 0 and 1").into());
            }
        }
        Ok(AiPersonality {
            militant: self.militant,
            expansionist: self.expansionist,
            scientific: self.scientific,
        })
    }
}

/// Distinguishes a missing field (`None`) from an explicit `null` (`Some(None)`).
fn double<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

pub async fn create_empire(State(s): State<Shared>, Json(req): Json<NewEmpire>) -> ApiResult {
    run(s, move |eng| {
        let w = &mut eng.world;
        let capital = req.capital.map(|c| system(w, c)).transpose()?;
        let mut n = w.empires.len() + 1;
        while w.empire_by_name(&format!("empire-{n}")).is_some() {
            n += 1;
        }
        let mut e = Empire::new(format!("empire-{n}"), req.display_name, &w.defs);
        e.ai = req.ai.as_ref().map(AiTraits::personality).transpose()?;
        e.color = req.color;
        if req.starting_resources {
            for (name, amount) in [
                ("food", 800.0),
                ("minerals", 1500.0),
                ("energy", 1000.0),
                ("alloy", 300.0),
                ("consumer-goods", 400.0),
                ("influence", 200.0),
            ] {
                if let Some(r) = w.defs.find_resource(name) {
                    e.storage[r.idx()] = amount;
                }
            }
        }
        let id = w.add_empire(e);
        if let Some(c) = capital {
            w.apply_now(Command::SetOwner {
                system: c,
                owner: Some(id),
            })?;
            w.apply_now(Command::SetCapital {
                empire: id,
                system: c,
            })?;
        }
        w.refresh_topology();
        Ok(json!({ "ok": true, "id": id.0 }))
    })
    .await
}

#[derive(Deserialize)]
pub struct EmpirePatch {
    display_name: Option<String>,
    /// A personality makes the empire AI-run; `null` hands it to the player.
    #[serde(default, deserialize_with = "double")]
    ai: Option<Option<AiTraits>>,
    color: Option<String>,
    accepts_migrants: Option<bool>,
    market_access: Option<bool>,
}

pub async fn patch_empire(
    State(s): State<Shared>,
    Path(id): Path<u32>,
    Json(p): Json<EmpirePatch>,
) -> ApiResult {
    run(s, move |eng| {
        let id = empire(&eng.world, id)?;
        let e = &mut eng.world.empires[id.idx()];
        if let Some(v) = p.display_name {
            e.display_name = v;
        }
        if let Some(v) = p.ai {
            e.ai = v.as_ref().map(AiTraits::personality).transpose()?;
        }
        if let Some(v) = p.color {
            e.color = Some(v);
        }
        if let Some(v) = p.accepts_migrants {
            e.accepts_migrants = v;
        }
        if let Some(v) = p.market_access {
            e.market_access = v;
        }
        Ok(ok())
    })
    .await
}

#[derive(Deserialize)]
pub struct TechReq {
    tech: u32,
    known: bool,
}

/// Grants or revokes a technology without research.
pub async fn set_tech(
    State(s): State<Shared>,
    Path(id): Path<u32>,
    Json(req): Json<TechReq>,
) -> ApiResult {
    run(s, move |eng| {
        let w = &mut eng.world;
        let id = empire(w, id)?;
        let t = tech(w, req.tech)?;
        let defs = w.defs.clone();
        let e = &mut w.empires[id.idx()];
        e.techs[t.idx()] = req.known;
        if e.researching == Some(t) && req.known {
            e.researching = None;
        }
        e.rebuild_modifiers(&defs);
        Ok(ok())
    })
    .await
}

#[derive(Deserialize)]
pub struct StorageReq {
    resource: u32,
    count: f64,
}

/// Sets a stockpile to an exact amount.
pub async fn set_storage(
    State(s): State<Shared>,
    Path(id): Path<u32>,
    Json(req): Json<StorageReq>,
) -> ApiResult {
    run(s, move |eng| {
        let w = &mut eng.world;
        let id = empire(w, id)?;
        let r = resource(w, req.resource)?;
        if !req.count.is_finite() {
            return Err("count must be finite".into());
        }
        w.empires[id.idx()].storage[r.idx()] = req.count.max(0.0);
        Ok(ok())
    })
    .await
}

// ---------------------------------------------------------------------------------------------
// Systems and planets
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewSystem {
    display_name: Option<String>,
    /// Galactic coordinates (kept within 100 of the centre if omitted).
    coordinates: Option<[f64; 2]>,
    owner: Option<u32>,
    #[serde(default)]
    links: Vec<u32>,
    /// Link to the nearest existing system when `links` is empty.
    #[serde(default = "yes")]
    link_nearest: bool,
    /// Fill the system with random planets (otherwise it starts empty).
    #[serde(default = "yes")]
    random_planets: bool,
}

pub async fn create_system(State(s): State<Shared>, Json(req): Json<NewSystem>) -> ApiResult {
    run(s, move |eng| {
        let owner = req.owner.map(|o| empire(&eng.world, o)).transpose()?;
        let links = req
            .links
            .iter()
            .map(|l| system(&eng.world, *l))
            .collect::<Result<Vec<_>, _>>()?;
        let pos = req.coordinates.unwrap_or_else(|| {
            // Somewhere around the rim of the existing galaxy, but within 100 of the centre.
            let radius = eng
                .world
                .systems
                .iter()
                .map(|s| s.coordinates[0].hypot(s.coordinates[1]))
                .fold(10.0, f64::max)
                .min(90.0);
            let a = eng.rng.f64() * std::f64::consts::TAU;
            let r = radius * eng.rng.range_f(0.7, 1.1);
            [
                (r * a.cos() * 100.0).round() / 100.0,
                (r * a.sin() * 100.0).round() / 100.0,
            ]
        });
        let mut used = eng.used_system_names();
        let defs = eng.world.defs.clone();
        let (mut sys, mut planets) = generate::random_system(&defs, &mut eng.rng, &mut used, pos);
        if let Some(name) = req.display_name.filter(|n| !n.trim().is_empty()) {
            sys.display_name = name;
        }
        let mut key = generate::slug(&sys.display_name);
        while eng.world.system_by_name(&key).is_some() {
            key.push('x');
        }
        sys.name = key;
        if !req.random_planets {
            planets.clear();
        }
        let w = &mut eng.world;
        let nearest = (req.link_nearest && links.is_empty())
            .then(|| {
                w.systems
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        (
                            i,
                            (s.coordinates[0] - pos[0]).hypot(s.coordinates[1] - pos[1]),
                        )
                    })
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _)| SystemId::from_idx(i))
            })
            .flatten();
        let id = w.add_system(sys, planets);
        for l in links.into_iter().chain(nearest) {
            w.link(id, l);
        }
        if let Some(o) = owner {
            w.apply_now(Command::SetOwner {
                system: id,
                owner: Some(o),
            })?;
        }
        w.refresh_topology();
        Ok(json!({ "ok": true, "id": id.0 }))
    })
    .await
}

#[derive(Deserialize)]
pub struct SystemPatch {
    display_name: Option<String>,
    star_type: Option<String>,
    star_count: Option<u32>,
    coordinates: Option<[f64; 2]>,
}

pub async fn patch_system(
    State(s): State<Shared>,
    Path(id): Path<u32>,
    Json(p): Json<SystemPatch>,
) -> ApiResult {
    run(s, move |eng| {
        let id = system(&eng.world, id)?;
        let sys = &mut eng.world.systems[id.idx()];
        if let Some(v) = p.display_name {
            sys.display_name = v;
        }
        if let Some(v) = p.star_type {
            sys.star_type = v;
        }
        if let Some(v) = p.star_count {
            sys.star_count = v.max(1);
        }
        if let Some(v) = p.coordinates {
            sys.coordinates = v;
        }
        Ok(ok())
    })
    .await
}

pub async fn delete_system(State(s): State<Shared>, Path(id): Path<u32>) -> ApiResult {
    run(s, move |eng| {
        let id = system(&eng.world, id)?;
        eng.world.remove_system(id)?;
        Ok(ok())
    })
    .await
}

#[derive(Deserialize)]
pub struct NewPlanet {
    display_name: String,
    #[serde(default = "large")]
    size: String,
    habitable: Option<bool>,
    #[serde(default)]
    pop: u64,
    moon_of: Option<String>,
    /// Position relative to the star (placed automatically if omitted).
    coordinates: Option<[f64; 2]>,
    /// Generate natural terrain (seas and blockers) like a newly generated planet.
    #[serde(default = "yes")]
    terrain: bool,
}

fn large() -> String {
    "large".into()
}

pub async fn create_planet(
    State(s): State<Shared>,
    Path(id): Path<u32>,
    Json(req): Json<NewPlanet>,
) -> ApiResult {
    run(s, move |eng| {
        let w = &mut eng.world;
        let sid = system(w, id)?;
        let size = match req.size.as_str() {
            "small" => PlanetSize::Small,
            "large" => PlanetSize::Large,
            other => return Err(format!("unknown planet size '{other}'").into()),
        };
        let base = format!(
            "{}-{}",
            w.system(sid).name,
            generate::slug(&req.display_name)
        );
        let mut key = base.clone();
        let mut n = 2;
        while w.system(sid).planet_ids().any(|p| w.planet(p).name == key) {
            key = format!("{base}-{n}");
            n += 1;
        }
        let habitable = req.habitable.unwrap_or(size == PlanetSize::Large);
        let mut p = Planet::new(
            key,
            req.display_name,
            size,
            habitable,
            w.defs.resource_count(),
        );
        p.pop = req.pop;
        p.moon_of = req.moon_of.filter(|m| !m.is_empty());
        p.owner = w.system(sid).owner;
        p.coordinates = req.coordinates.unwrap_or_else(|| {
            spacebox_core::loader::default_planet_coordinates(w.system(sid).planet_range().len())
        });
        if req.terrain {
            let defs = w.defs.clone();
            generate::generate_terrain(&defs, &mut eng.rng, &mut p);
        }
        let id = w.add_planet(sid, p)?;
        Ok(json!({ "ok": true, "id": id.0 }))
    })
    .await
}

#[derive(Deserialize)]
pub struct PlanetPatch {
    display_name: Option<String>,
    /// Set the population exactly.
    pop: Option<u64>,
    /// Add (or with a negative number, remove) pops.
    add_pops: Option<i64>,
    habitable: Option<bool>,
    moon_of: Option<String>,
    /// Instantly finish (0) or set the remaining days of terraforming.
    terraform_days_left: Option<u32>,
    coordinates: Option<[f64; 2]>,
    /// Clear siege progress and repair all bombardment damage.
    #[serde(default)]
    repair: bool,
}

pub async fn patch_planet(
    State(s): State<Shared>,
    Path(id): Path<u32>,
    Json(p): Json<PlanetPatch>,
) -> ApiResult {
    run(s, move |eng| {
        let id = planet(&eng.world, id)?;
        let planet = eng.world.planet_mut(id);
        if let Some(v) = p.display_name {
            planet.display_name = v;
        }
        if let Some(v) = p.pop {
            planet.pop = v;
        }
        if let Some(d) = p.add_pops {
            planet.pop = (planet.pop as i64 + d).max(0) as u64;
        }
        if let Some(v) = p.habitable {
            planet.habitable = v;
        }
        if let Some(v) = p.moon_of {
            planet.moon_of = (!v.is_empty()).then_some(v);
        }
        if let Some(d) = p.terraform_days_left {
            if d == 0 {
                planet.terraform_days_left = None;
                planet.habitable = true;
            } else {
                planet.terraform_days_left = Some(d);
            }
        }
        if let Some(c) = p.coordinates {
            planet.coordinates = c;
        }
        if p.repair {
            planet.siege = 0.0;
            for t in &mut planet.tiles {
                if let Some(b) = &mut t.building {
                    b.damage = 0.0;
                }
            }
        }
        planet.invalidate();
        Ok(ok())
    })
    .await
}

pub async fn delete_planet(State(s): State<Shared>, Path(id): Path<u32>) -> ApiResult {
    run(s, move |eng| {
        let id = planet(&eng.world, id)?;
        eng.world.remove_planet(id)?;
        Ok(ok())
    })
    .await
}

#[derive(Deserialize)]
pub struct TileEdit {
    /// Building to place instantly (free, ignoring tech and barren rules); `null` clears.
    #[serde(default, deserialize_with = "double_option")]
    building: Option<Option<u32>>,
    upgrades: Option<Vec<u32>>,
    /// Cancel any construction in progress on the tile.
    #[serde(default)]
    cancel_construction: bool,
    /// Replace the tile's resource multipliers: `[[resource, mult], ...]`.
    multipliers: Option<Vec<(u32, f64)>>,
    /// Flood or drain the tile.
    water: Option<bool>,
}

fn double_option<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<u32>>, D::Error> {
    Option::<u32>::deserialize(d).map(Some)
}

/// Game-master tile editing: places buildings and upgrades directly.
pub async fn edit_tile(
    State(s): State<Shared>,
    Path((pid, t)): Path<(u32, usize)>,
    Json(req): Json<TileEdit>,
) -> ApiResult {
    run(s, move |eng| {
        let w = &mut eng.world;
        let pid = planet(w, pid)?;
        let t = tile(w, pid, t)?;
        let mut warning = None;
        let new_building = match req.building {
            Some(Some(b)) => Some(Some(building(w, b)?)),
            Some(None) => Some(None),
            None => None,
        };
        let upgrades = req
            .upgrades
            .map(|us| {
                us.into_iter()
                    .map(|u| upgrade(w, u))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let multipliers = req
            .multipliers
            .map(|ms| {
                ms.into_iter()
                    .map(|(r, m)| Ok((resource(w, r)?, m)))
                    .collect::<Result<Vec<_>, ApiError>>()
            })
            .transpose()?;
        let defs = w.defs.clone();
        let p = w.planet_mut(pid);
        if let Some(nb) = new_building {
            p.tiles[t].building = nb.map(BuildingInstance::new);
            p.tiles[t].construction = None;
            if let Some(b) = nb
                && !p.habitable
                && !defs.building(b).barren
            {
                warning = Some(format!(
                    "{} normally cannot be built on barren planets",
                    defs.building(b).display_name
                ));
            }
        }
        if req.cancel_construction {
            p.tiles[t].construction = None;
        }
        if let Some(us) = upgrades {
            let Some(inst) = p.tiles[t].building.as_mut() else {
                return Err("tile has no building to upgrade".into());
            };
            let def = defs.building(inst.def);
            if let Some(bad) = us.iter().find(|u| !def.available_upgrades.contains(u)) {
                return Err(format!(
                    "{} cannot be applied to {}",
                    defs.upgrade(*bad).display_name,
                    def.display_name
                )
                .into());
            }
            if matches!(p.tiles[t].construction, Some(Construction::Upgrade { .. })) {
                p.tiles[t].construction = None;
            }
            p.tiles[t].building.as_mut().unwrap().upgrades = us;
        }
        if let Some(ms) = multipliers {
            p.tiles[t].resource_mults = ms;
        }
        if let Some(wet) = req.water {
            p.tiles[t].water = wet;
        }
        if let Some(b) = p.tiles[t].building.as_ref().map(|b| defs.building(b.def))
            && ((b.land && p.tiles[t].water) || (b.water && !p.tiles[t].water))
        {
            warning = Some(format!(
                "{} normally needs a {} tile",
                b.display_name,
                if b.water { "water" } else { "land" }
            ));
        }
        p.invalidate();
        Ok(json!({ "ok": true, "warning": warning }))
    })
    .await
}

// ---------------------------------------------------------------------------------------------
// Whole-world operations
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct Regenerate {
    #[serde(default = "default_systems")]
    systems: usize,
    #[serde(default = "default_empires")]
    empires: usize,
    seed: Option<u64>,
    #[serde(default = "yes")]
    ai: bool,
}

fn default_systems() -> usize {
    80
}
fn default_empires() -> usize {
    5
}

pub async fn regenerate(State(s): State<Shared>, Json(req): Json<Regenerate>) -> ApiResult {
    run(s, move |eng| {
        if req.systems == 0 || req.systems > 20_000 {
            return Err("systems must be between 1 and 20000".into());
        }
        let (defs, _) = load_assets(eng.data_dir.join("base.toml")).map_err(|e| e.to_string())?;
        let seed = req.seed.unwrap_or_else(|| eng.rng.next_u64() % 1_000_000);
        let params = GalaxyParams {
            seed,
            systems: req.systems,
            empires: req.empires,
            ai: req.ai,
        };
        let world = generate::generate(defs, &params).map_err(|e| e.to_string())?;
        eng.replace(world);
        Ok(json!({ "ok": true, "seed": seed }))
    })
    .await
}

#[derive(Deserialize)]
pub struct LoadReq {
    /// Path relative to the data directory, e.g. `scenarios/sol.toml`.
    path: String,
}

pub async fn load(State(s): State<Shared>, Json(req): Json<LoadReq>) -> ApiResult {
    run(s, move |eng| {
        let path = eng.data_path(&req.path)?;
        let (world, warnings) = load_world(&path).map_err(|e| e.to_string())?;
        eng.replace(world);
        Ok(json!({ "ok": true, "warnings": warnings }))
    })
    .await
}

#[derive(Deserialize)]
pub struct SaveReq {
    name: String,
}

pub async fn save_world(State(s): State<Shared>, Json(req): Json<SaveReq>) -> ApiResult {
    run(s, move |eng| {
        let name = generate::slug(req.name.trim_end_matches(".toml"));
        if name.is_empty() {
            return Err("a save needs a name".into());
        }
        let dir = eng.data_dir.join("saves");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{name}.toml"));
        save::save(&eng.world, &path).map_err(|e| e.to_string())?;
        Ok(json!({ "ok": true, "path": format!("saves/{name}.toml") }))
    })
    .await
}

/// Loadable files: scenarios and saves.
pub async fn list_files(State(s): State<Shared>) -> ApiResult {
    run(s, |eng| {
        let mut files = Vec::new();
        for dir in ["scenarios", "saves"] {
            if let Ok(entries) = std::fs::read_dir(eng.data_dir.join(dir)) {
                let mut names: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.ends_with(".toml"))
                    .collect();
                names.sort();
                files.extend(names.into_iter().map(|n| format!("{dir}/{n}")));
            }
        }
        Ok(json!({ "files": files }))
    })
    .await
}

// ---------------------------------------------------------------------------------------------
// Fleets (game-master tools; orders go through /api/command)
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ShipCount {
    ship: u32,
    #[serde(default = "one")]
    count: u64,
}

#[derive(Deserialize)]
pub struct NewFleet {
    empire: u32,
    system: u32,
    display_name: Option<String>,
    coordinates: Option<[f64; 2]>,
    #[serde(default)]
    ships: Vec<ShipCount>,
}

/// Spawns a fleet instantly, for free.
pub async fn create_fleet(State(s): State<Shared>, Json(req): Json<NewFleet>) -> ApiResult {
    run(s, move |eng| {
        let w = &mut eng.world;
        let owner = empire(w, req.empire)?;
        let sys = system(w, req.system)?;
        let mut fleet = Fleet::new(
            String::new(),
            req.display_name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| format!("{} Task Force", w.system(sys).display_name)),
            owner,
            sys,
        );
        let r = w.rules().arrival_radius;
        fleet.coordinates = req.coordinates.unwrap_or([r * 0.5, 0.0]);
        for sc in req.ships {
            let def = ship(w, sc.ship)?;
            for _ in 0..sc.count.min(500) {
                fleet.ships.push(Ship::new(def, &w.defs));
            }
        }
        if fleet.ships.is_empty() {
            return Err("a fleet needs at least one ship".into());
        }
        let id = w.add_fleet(fleet);
        Ok(json!({ "ok": true, "id": id.0 }))
    })
    .await
}

#[derive(Deserialize)]
pub struct FleetPatch {
    display_name: Option<String>,
    owner: Option<u32>,
    /// Teleport to another system.
    system: Option<u32>,
    coordinates: Option<[f64; 2]>,
    /// Restore every ship's hull and shields.
    #[serde(default)]
    repair: bool,
}

pub async fn patch_fleet(
    State(s): State<Shared>,
    Path(id): Path<u32>,
    Json(p): Json<FleetPatch>,
) -> ApiResult {
    run(s, move |eng| {
        let w = &mut eng.world;
        let owner = p.owner.map(|o| empire(w, o)).transpose()?;
        let sys = p.system.map(|s| system(w, s)).transpose()?;
        let defs = w.defs.clone();
        let f = w
            .fleet_mut(FleetId(id))
            .ok_or_else(|| ApiError::from(format!("no fleet with id {id}")))?;
        if let Some(v) = p.display_name {
            f.display_name = v;
        }
        if let Some(o) = owner {
            f.owner = o;
            f.order = FleetOrder::Idle;
        }
        if let Some(s) = sys {
            f.system = s;
            f.jump_charge = 0.0;
            f.order = FleetOrder::Idle;
        }
        if let Some(c) = p.coordinates {
            f.coordinates = c;
        }
        if p.repair {
            for s in &mut f.ships {
                let d = defs.ship(s.def);
                s.hull = d.max_hull;
                s.shield = d.max_shield;
            }
        }
        Ok(ok())
    })
    .await
}

pub async fn delete_fleet(State(s): State<Shared>, Path(id): Path<u32>) -> ApiResult {
    run(s, move |eng| {
        if eng.world.remove_fleet(FleetId(id)) {
            Ok(ok())
        } else {
            Err(format!("no fleet with id {id}").into())
        }
    })
    .await
}
