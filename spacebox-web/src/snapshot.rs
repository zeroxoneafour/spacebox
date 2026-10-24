//! JSON views of the engine for the client.
//!
//! [`state`] is the full mutable game state (diffed between ticks); [`defs`] is the static
//! catalog of definitions and planet geometry, which only changes when a world is loaded.
//! All ids are array indices into the corresponding lists.

use serde_json::{Value, json};
use spacebox_core::defs::{BuildingTarget, Defs, ResourceAmount};
use spacebox_core::geometry::PlanetSize;
use spacebox_core::world::{Construction, FleetOrder, Tile, World};
use spacebox_core::{Event, PlanetId};

/// Rounds for transport: keeps diffs from churning on float noise.
fn r(x: f64) -> Value {
    if x.is_finite() {
        json!((x * 100.0).round() / 100.0)
    } else {
        Value::Null
    }
}

fn amounts(list: &[ResourceAmount]) -> Value {
    Value::Array(
        list.iter()
            .map(|a| json!([a.resource.0, a.count]))
            .collect(),
    )
}

fn vec_r(v: &[f64]) -> Value {
    Value::Array(v.iter().map(|x| r(*x)).collect())
}

fn tile(t: &Tile) -> Value {
    let mut o = serde_json::Map::new();
    if t.water {
        o.insert("wa".into(), json!(true));
    }
    if let Some(b) = &t.building {
        o.insert("b".into(), json!(b.def.0));
        if !b.upgrades.is_empty() {
            o.insert(
                "u".into(),
                json!(b.upgrades.iter().map(|u| u.0).collect::<Vec<_>>()),
            );
        }
        if b.workers > 0 {
            o.insert("w".into(), json!(b.workers));
        }
        if let Some(c) = b.job_cap {
            o.insert("cap".into(), json!(c));
        }
        if b.pinned {
            o.insert("pin".into(), json!(true));
        }
        if !b.enabled {
            o.insert("off".into(), json!(true));
        }
        if b.damage > 0.0 {
            o.insert("dmg".into(), r(b.damage));
        }
    }
    match &t.construction {
        Some(Construction::Building {
            def,
            days_left,
            empire,
            ..
        }) => {
            o.insert(
                "c".into(),
                json!({ "building": def.0, "days": days_left, "empire": empire.map(|e| e.0) }),
            );
        }
        Some(Construction::Upgrade { upgrade, days_left }) => {
            o.insert(
                "c".into(),
                json!({ "upgrade": upgrade.0, "days": days_left }),
            );
        }
        None => {}
    }
    if !t.resource_mults.is_empty() {
        o.insert(
            "m".into(),
            Value::Array(
                t.resource_mults
                    .iter()
                    .map(|(res, m)| json!([res.0, m]))
                    .collect(),
            ),
        );
    }
    Value::Object(o)
}

pub fn state(w: &World) -> Value {
    let n_res = w.defs.resource_count();
    let systems: Vec<Value> = w
        .systems
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({
                "id": i,
                "name": s.name,
                "display_name": s.display_name,
                "star_type": s.star_type,
                "star_count": s.star_count,
                "owner": s.owner.map(|o| o.0),
                "links": s.links.iter().map(|l| l.0).collect::<Vec<_>>(),
                "coordinates": s.coordinates,
                "planets": s.planet_ids().map(|p| p.0).collect::<Vec<_>>(),
            })
        })
        .collect();
    let planets: Vec<Value> = w
        .planets
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let s = &p.stats;
            let net: Vec<f64> = (0..n_res)
                .map(|res| p.flows.supply[res] - p.flows.demand[res])
                .collect();
            json!({
                "id": i,
                "name": p.name,
                "display_name": p.display_name,
                "system": p.system.0,
                "size": p.size.as_str(),
                "habitable": p.habitable,
                "moon_of": p.moon_of,
                "coordinates": p.coordinates,
                "owner": p.owner.map(|o| o.0),
                "siege": r(p.siege),
                "siege_threshold": r(w.rules().surrender_threshold(p.pop, s.stability)),
                "pop": p.pop,
                "terraform_days_left": p.terraform_days_left,
                "stats": {
                    "housing": r(s.housing),
                    "amenities": r(s.amenities),
                    "amenities_needed": r(s.amenities_needed),
                    "stability": r(s.stability),
                    "jobs": s.jobs,
                    "employed": s.employed,
                    "unemployed": s.unemployed,
                    "homeless": s.homeless,
                    "free_jobs": s.free_jobs,
                },
                "net": vec_r(&net),
                "tiles": p.tiles.iter().map(tile).collect::<Vec<_>>(),
            })
        })
        .collect();
    let empires: Vec<Value> = w
        .empires
        .iter()
        .enumerate()
        .map(|(i, e)| {
            // Counted here rather than taken from `e.stats`, which is only filled in by a tick.
            let owned_systems = w.systems.iter().filter(|s| s.owner.map(|o| o.idx()) == Some(i)).count();
            let mine = || w.planets.iter().filter(|p| p.owner.map(|o| o.idx()) == Some(i));
            let planets = mine().count();
            let pop: u64 = mine().map(|p| p.pop).sum();
            json!({
                "id": i,
                "name": e.name,
                "display_name": e.display_name,
                "color": e.color,
                "ai": e.ai.map(|a| json!({
                    "militant": a.militant,
                    "expansionist": a.expansionist,
                    "scientific": a.scientific,
                })),
                "accepts_migrants": e.accepts_migrants,
                "market_access": e.market_access,
                "capital": e.capital.map(|c| c.0),
                "storage": vec_r(&e.storage),
                "income": vec_r(&e.income),
                "trade": vec_r(&e.trade),
                "techs": e.techs.iter().enumerate().filter(|(_, k)| **k).map(|(t, _)| t).collect::<Vec<_>>(),
                "researching": e.researching.map(|t| t.0),
                "research_progress": r(e.research_progress),
                "crises": e.crises.iter().map(|c| json!({
                    "resource": c.resource.0,
                    "since_day": c.since_day,
                    "severity": r(c.severity),
                })).collect::<Vec<_>>(),
                "at_war": e.wars.iter().map(|w| w.enemy.0).collect::<Vec<_>>(),
                "wars": e.wars.iter().map(|w| json!({ "enemy": w.enemy.0, "since_day": w.since_day })).collect::<Vec<_>>(),
                "truces": e.truces.iter().filter(|t| t.until_day > w.day).map(|t| json!({ "with": t.with.0, "until_day": t.until_day })).collect::<Vec<_>>(),
                "ship_queue": e.ship_queue.iter().map(|o| json!({
                    "ship": o.ship.0,
                    "planet": o.planet.0,
                    "remaining": o.remaining.iter().map(|(r, c)| json!([r.0, (c * 100.0).round() / 100.0])).collect::<Vec<_>>(),
                    "fleet": o.fleet.map(|f| f.0),
                })).collect::<Vec<_>>(),
                "fleet_upkeep": vec_r(&e.fleet_upkeep),
                "stats": {
                    "pop": pop,
                    "planets": planets,
                    "systems": owned_systems,
                    "research_income": r(e.stats.research_income),
                },
            })
        })
        .collect();
    let rules = w.rules();
    let fleets: Vec<Value> = w
        .fleets
        .iter()
        .map(|f| {
            let order = match &f.order {
                FleetOrder::Idle => json!({ "type": "idle" }),
                FleetOrder::Move { system, coordinates } => {
                    json!({ "type": "move", "system": system.0, "coordinates": coordinates.map(|c| [r(c[0]), r(c[1])]) })
                }
                FleetOrder::Bombard { planet, tile } => {
                    json!({ "type": "bombard", "planet": planet.0, "tile": tile })
                }
            };
            json!({
                "id": f.id.0,
                "name": f.name,
                "display_name": f.display_name,
                "owner": f.owner.0,
                "system": f.system.0,
                "coordinates": [r(f.coordinates[0]), r(f.coordinates[1])],
                "order": order,
                "jump_charge": r(f.jump_charge),
                "jump_time": r(f.jump_time(&w.defs, rules)),
                "in_combat": f.in_combat,
                "ships": f.ships.iter().map(|s| json!([s.def.0, r(s.hull), r(s.shield)])).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "day": w.day,
        "seed": w.seed,
        "systems": systems,
        "planets": planets,
        "empires": empires,
        "fleets": fleets,
        "market": w.market.iter().map(|m| json!({ "price": r(m.price), "volume": r(m.volume) })).collect::<Vec<_>>(),
        "queued_commands": w.pending_commands().len(),
    })
}

fn target(defs: &Defs, t: &BuildingTarget) -> Value {
    match t {
        BuildingTarget::Building(b) => json!({ "building": b.0 }),
        BuildingTarget::Class(c) => json!({ "class": defs.class_name(*c) }),
    }
}

fn shape(size: PlanetSize) -> Value {
    let s = size.shape();
    json!({
        "centers": s.centers,
        "sides": s.sides,
        "neighbors": s.neighbors,
        "faces": s.faces,
    })
}

pub fn defs(d: &Defs, w: &World) -> Value {
    let qualified = |file: spacebox_core::FileId, name: &str| {
        let stem = d.files[file.idx()]
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        format!("{stem}.{name}")
    };
    let rules = w.rules();
    json!({
        "resources": d.resources.iter().enumerate().map(|(i, r)| json!({
            "id": i,
            "name": r.key.name,
            "display_name": r.display_name,
            "description": r.description,
            "base_value": r.base_value,
            "stockpile": r.stockpile,
            "tradeable": r.tradeable,
            "on_demand": r.on_demand,
            "storage_cap": r.storage_cap,
        })).collect::<Vec<_>>(),
        "jobs": d.jobs.iter().enumerate().map(|(i, j)| json!({
            "id": i,
            "name": j.key.name,
            "display_name": j.display_name,
            "appeal": j.appeal,
            "upkeep": amounts(&j.upkeep),
        })).collect::<Vec<_>>(),
        "buildings": d.buildings.iter().enumerate().map(|(i, b)| json!({
            "id": i,
            "name": qualified(b.key.file, &b.key.name),
            "display_name": b.display_name,
            "description": b.description,
            "max_workers": b.max_workers,
            "job": b.job.map(|j| j.0),
            "classes": b.classes.iter().map(|c| d.class_name(*c)).collect::<Vec<_>>(),
            "barren": b.barren,
            "blocker": b.blocker,
            "land": b.land,
            "water": b.water,
            "space": b.space,
            "remove_cost": amounts(&b.remove_cost),
            "housing": b.housing,
            "amenities": b.amenities,
            "upkeep": amounts(&b.upkeep),
            "inputs": amounts(&b.inputs),
            "outputs": amounts(&b.outputs),
            "build_cost": amounts(&b.build_cost),
            "build_days": b.build_days,
            "requires_tech": b.requires_tech.iter().map(|t| t.0).collect::<Vec<_>>(),
            "buildable": b.buildable,
            "max_per_planet": b.max_per_planet,
            "hp": b.hp.unwrap_or(rules.building_hp),
            "shipyard": d.produces_on_demand(spacebox_core::BuildingId::from_idx(i)),
            "upgrades": b.available_upgrades.iter().map(|u| u.0).collect::<Vec<_>>(),
            "adjacency": b.adjacency.iter().map(|a| json!({
                "target": target(d, &a.target),
                "input_mult": a.modifier.input_mult,
                "output_mult": a.modifier.output_mult,
                "upkeep_mult": a.modifier.upkeep_mult,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "upgrades": d.upgrades.iter().enumerate().map(|(i, u)| json!({
            "id": i,
            "name": qualified(u.key.file, &u.key.name),
            "display_name": u.display_name,
            "description": u.description,
            "needs": u.needs_upgrades.iter().map(|n| n.0).collect::<Vec<_>>(),
            "requires_tech": u.requires_tech.iter().map(|t| t.0).collect::<Vec<_>>(),
            "cost": amounts(&u.cost),
            "build_days": u.build_days,
            "input_mult": u.modifier.input_mult,
            "output_mult": u.modifier.output_mult,
            "upkeep_mult": u.modifier.upkeep_mult,
        })).collect::<Vec<_>>(),
        "technologies": d.technologies.iter().enumerate().map(|(i, t)| json!({
            "id": i,
            "name": t.key.name,
            "display_name": t.display_name,
            "description": t.description,
            "category": t.category,
            "cost": t.cost,
            "prerequisites": t.prerequisites.iter().map(|p| p.0).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "terraforming": {
            "large": rules.terraform_large.as_ref().map(|t| json!({ "tech": t.requires_tech.map(|x| x.0), "days": t.days, "cost": amounts(&t.cost) })),
            "small": rules.terraform_small.as_ref().map(|t| json!({ "tech": t.requires_tech.map(|x| x.0), "days": t.days, "cost": amounts(&t.cost) })),
        },
        "ships": d.ships.iter().enumerate().map(|(i, s)| json!({
            "id": i,
            "name": qualified(s.key.file, &s.key.name),
            "display_name": s.display_name,
            "description": s.description,
            "requires_tech": s.requires_tech.iter().map(|t| t.0).collect::<Vec<_>>(),
            "build_cost": amounts(&s.build_cost),
            "upkeep": amounts(&s.upkeep),
            "hull": s.max_hull,
            "shield": s.max_shield,
            "shield_regen": s.shield_regen,
            "speed": s.speed,
            "hyperjump_days": rules.hyperjump_days * s.hyperjump_mult,
            "range": s.max_range,
            "weapons": s.weapons.iter().map(|w| w.0).collect::<Vec<_>>(),
            "outfits": s.outfits.iter().map(|o| o.0).collect::<Vec<_>>(),
            "dps": s.armament.iter().map(|a| (a.hull_damage + a.shield_damage) / a.cooldown).sum::<f64>(),
        })).collect::<Vec<_>>(),
        "weapons": d.weapons.iter().enumerate().map(|(i, x)| json!({
            "id": i, "display_name": x.display_name, "shield_damage": x.shield_damage,
            "hull_damage": x.hull_damage, "fire_rate": x.fire_rate, "range": x.range,
        })).collect::<Vec<_>>(),
        "outfits": d.outfits.iter().enumerate().map(|(i, x)| json!({
            "id": i, "display_name": x.display_name, "description": x.description,
        })).collect::<Vec<_>>(),
        "rules": {
            "expansion_resource": rules.expansion_resource.map(|r| r.0),
            "expansion_base_cost": rules.expansion_base_cost,
            "expansion_base_days": rules.expansion_base_days,
            "hyperjump_days": rules.hyperjump_days,
            "arrival_radius": rules.arrival_radius,
            "building_disable_damage": rules.building_disable_damage,
        },
        "geometry": { "small": shape(PlanetSize::Small), "large": shape(PlanetSize::Large) },
    })
}

pub fn event(seq: u64, e: &Event) -> Value {
    json!({
        "seq": seq,
        "day": e.day,
        "kind": format!("{:?}", e.kind),
        "empire": e.empire.map(|x| x.0),
        "planet": e.planet.map(|p: PlanetId| p.0),
        "message": e.message,
    })
}
