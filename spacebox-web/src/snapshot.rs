//! JSON views of the engine for the client.
//!
//! [`state`] is the full mutable game state (diffed between ticks); [`defs`] is the static
//! catalog of definitions and planet geometry, which only changes when a world is loaded.
//! All ids are array indices into the corresponding lists.

use std::sync::Arc;

use rayon::prelude::*;
use serde_json::{Value, json};
use spacebox_core::defs::{Cost, Defs, Modifier, ResourceAmount, TargetedModifier, Targets, Yield};
use spacebox_core::geometry::PlanetSize;
use spacebox_core::world::{FleetOrder, ProjectKind, Side, Tile, World};
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
            .map(|a| json!([a.resource.0, (a.count * 100.0).round() / 100.0]))
            .collect(),
    )
}

/// A project cost or what remains of one: `{ resources: [[id, count]], days }`.
fn cost(c: &Cost) -> Value {
    json!({ "resources": amounts(&c.resources), "days": c.days })
}

/// A production list: `{ resources: [[id, count]], amenities }`.
fn yields(y: &Yield) -> Value {
    json!({ "resources": amounts(&y.resources), "amenities": y.amenities })
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
    if let Some(c) = &t.construction {
        let mut v = match c.kind {
            ProjectKind::Building(b) => json!({ "kind": "building", "building": b.0 }),
            ProjectKind::Upgrade(u) => json!({ "kind": "upgrade", "upgrade": u.0 }),
            ProjectKind::Remove => json!({ "kind": "remove" }),
        };
        v["paid"] = amounts(&c.paid);
        v["remaining"] = cost(&c.remaining);
        v["empire"] = json!(c.empire.map(|e| e.0));
        o.insert("c".into(), v);
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

/// One version of the game state: small top-level fields, plus every system, planet, empire
/// and fleet serialized on its own. Entities unchanged since the previous version share its
/// text, so keeping several versions costs little more than one.
pub struct Snapshot {
    pub top: Value,
    pub lists: [(&'static str, Vec<Arc<str>>); 4],
}

impl Snapshot {
    /// Whether nothing changed (cheap: unchanged entities share their text).
    pub fn same(&self, other: &Snapshot) -> bool {
        self.top == other.top
            && self.lists.iter().zip(&other.lists).all(|((_, a), (_, b))| {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| Arc::ptr_eq(x, y) || x == y)
            })
    }

    /// The full state as JSON text.
    pub fn to_json(&self) -> String {
        let mut out = self.top.to_string();
        out.pop(); // the closing brace
        for (name, items) in &self.lists {
            let len: usize = items.iter().map(|i| i.len() + 1).sum();
            out.reserve(len + name.len() + 6);
            out.push_str(&format!(",\"{name}\":["));
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(item);
            }
            out.push(']');
        }
        out.push('}');
        out
    }
}

/// Serializes each item (in parallel), reusing the previous version's text where unchanged.
fn list<T: Sync>(
    items: &[T],
    prev: Option<&[Arc<str>]>,
    f: impl Fn(usize, &T) -> Value + Sync,
) -> Vec<Arc<str>> {
    items
        .par_iter()
        .enumerate()
        .map(|(i, item)| {
            let text = f(i, item).to_string();
            match prev.and_then(|p| p.get(i)) {
                Some(old) if **old == *text => old.clone(),
                _ => Arc::from(text),
            }
        })
        .collect()
}

pub fn state(w: &World, prev: Option<&Snapshot>) -> Snapshot {
    let prev_list = |k: usize| prev.map(|p| p.lists[k].1.as_slice());
    let n_res = w.defs.resource_count();
    let rules = w.rules();
    let systems = list(&w.systems, prev_list(0), |i, s| {
        json!({
            "id": i,
            "name": s.name,
            "display_name": s.display_name,
            "star_type": s.star_type,
            "star_count": s.star_count,
            "owner": s.owner.map(|o| o.0),
            "occupier": s.occupier.map(|o| o.0),
            "links": s.links.iter().map(|l| l.0).collect::<Vec<_>>(),
            "coordinates": s.coordinates,
            "planets": s.planet_ids().map(|p| p.0).collect::<Vec<_>>(),
        })
    });
    let planets = list(&w.planets, prev_list(1), |i, p| {
        let s = &p.stats;
        let net: Vec<f64> = (0..n_res).map(|res| p.flows.net(res)).collect();
        json!({
            "id": i,
            "name": p.name,
            "display_name": p.display_name,
            "system": p.system.0,
            "size": p.size.as_str(),
            "habitable": p.habitable,
            "coordinates": p.coordinates,
            "owner": p.owner.map(|o| o.0),
            "occupier": p.occupier.map(|o| o.0),
            "siege": r(p.siege),
            "siege_threshold": r(rules.surrender_threshold(p.pop(), s.stability)),
            "pop": p.pop(),
            "population": p.population.iter().map(|(sp, n)| json!([sp.0, n])).collect::<Vec<_>>(),
            "terraform": p.terraform.as_ref().map(|t| json!({ "paid": amounts(&t.paid), "remaining": cost(&t.remaining) })),
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
    });
    let empires = list(&w.empires, prev_list(2), |i, e| {
        // Counted here rather than taken from `e.stats`, which is only filled in by a tick.
        let id = spacebox_core::EmpireId::from_idx(i);
        let owned_systems = w.empire_systems(id).len();
        let planets = w.empire_planets(id).len();
        let pop: u64 = w
            .empire_planets(id)
            .iter()
            .map(|p| w.planet(*p).pop())
            .sum();
        json!({
            "id": i,
            "name": e.name,
            "display_name": e.display_name,
            "color": e.color,
            "dominant_species": e.dominant_species.map(|s| s.0),
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
            "wasted": vec_r(&e.wasted),
            "trade": vec_r(&e.trade),
            "techs": e.techs.iter().enumerate().filter(|(_, k)| **k).map(|(t, _)| t).collect::<Vec<_>>(),
            "research": e.research.iter().map(|p| json!({
                "tech": p.tech.0,
                "paid": amounts(&p.paid),
                "remaining": cost(&p.remaining),
            })).collect::<Vec<_>>(),
            "shipbuilding": e.shipbuilding.iter().map(|p| json!({
                "ship": p.ship.0,
                "planet": p.planet.0,
                "paid": amounts(&p.paid),
                "remaining": cost(&p.remaining),
            })).collect::<Vec<_>>(),
            "crises": e.crises.iter().map(|c| json!({
                "resource": c.resource.0,
                "since_day": c.since_day,
                "severity": r(c.severity),
            })).collect::<Vec<_>>(),
            "at_war": (0..w.empires.len())
                .filter(|o| w.at_war(id, spacebox_core::EmpireId::from_idx(*o)))
                .collect::<Vec<_>>(),
            "total_war": e.total_war,
            "humiliated": e.humiliated,
            "claims": e.claims.iter().map(|c| c.0).collect::<Vec<_>>(),
            "wars": e.wars.iter().map(|(war, side)| json!({ "war": war.0, "side": side.as_str() })).collect::<Vec<_>>(),
            "truces": e.truces.iter().filter(|t| w.truces[t.idx()].valid_until > w.day).map(|t| t.0).collect::<Vec<_>>(),
            "alliances": e.alliances.iter().map(|a| a.0).collect::<Vec<_>>(),
            "fleet_upkeep": vec_r(&e.fleet_upkeep),
            "max_fleet_size": e.max_fleet_size(rules),
            // Effective ship stats with this empire's technologies, per design.
            "ship_stats": e.ship_stats.iter().map(|s| json!({
                "hull": r(s.max_hull),
                "shield": r(s.max_shield),
                "speed": r(s.speed),
                "range": r(s.max_range),
            })).collect::<Vec<_>>(),
            "stats": {
                "pop": pop,
                "planets": planets,
                "systems": owned_systems,
            },
        })
    });
    let fleets = list(&w.fleets, prev_list(3), |_, f| {
        let orders: Vec<Value> = f
            .orders
            .iter()
            .map(|o| match o {
                FleetOrder::Move { coordinates: c } => {
                    json!({ "order": "move", "coordinates": [r(c[0]), r(c[1])] })
                }
                FleetOrder::Jump { system } => json!({ "order": "jump", "system": system.0 }),
                FleetOrder::Bombard { planet, tile } => {
                    json!({ "order": "bombard", "planet": planet.0, "tile": tile })
                }
            })
            .collect();
        json!({
            "id": f.id.0,
            "name": f.name,
            "display_name": f.display_name,
            "owner": f.owner.0,
            "system": f.system.0,
            "coordinates": [r(f.coordinates[0]), r(f.coordinates[1])],
            "orders": orders,
            "destination": f.destination().0,
            "jump_progress": r(f.jump_progress()),
            "in_combat": f.in_combat,
            "size": f.size(&w.defs),
            // [design, hull, shield, x, y, jump charge] with absolute system coordinates.
            "ships": f.ships.iter().enumerate().map(|(i, s)| {
                let c = f.ship_coordinates(i);
                json!([s.def.0, r(s.hull), r(s.shield), r(c[0]), r(c[1]), r(s.jump_charge)])
            }).collect::<Vec<_>>(),
        })
    });
    Snapshot {
        top: json!({
            "day": w.day,
            "seed": w.seed,
            "market": w.market.iter().map(|m| json!({ "price": r(m.price), "volume": r(m.volume) })).collect::<Vec<_>>(),
            "queued_commands": w.pending_commands().len(),
            "wars": w.wars.iter().enumerate().map(|(i, war)| {
                let id = spacebox_core::WarId::from_idx(i);
                json!({
                    "id": i,
                    "name": war.name,
                    "display_name": war.display_name,
                    "goals": war.goals.map(|g| g.map(|g| g.as_str())),
                    "starting_day": war.starting_day,
                    "exhaustion": war.exhaustion.map(r),
                    "victory": war.victory.map(|v| v.as_str()),
                    "proposals": war.proposals.map(|p| p.map(|p| p.as_str())),
                    "offense": w.members(id, Side::Offense).iter().map(|e| e.0).collect::<Vec<_>>(),
                    "defense": w.members(id, Side::Defense).iter().map(|e| e.0).collect::<Vec<_>>(),
                })
            }).collect::<Vec<_>>(),
            "alliances": w.alliances.iter().enumerate().map(|(i, a)| json!({
                "id": i,
                "name": a.name,
                "display_name": a.display_name,
            })).collect::<Vec<_>>(),
            "truces": w.truces.iter().enumerate().filter(|(_, t)| t.valid_until > w.day).map(|(i, t)| json!({
                "id": i,
                "name": t.name,
                "display_name": t.display_name,
                "valid_until": t.valid_until,
            })).collect::<Vec<_>>(),
        }),
        lists: [
            ("systems", systems),
            ("planets", planets),
            ("empires", empires),
            ("fleets", fleets),
        ],
    }
}

/// A tag as `file-stem.name` (reserved tags: just the name).
fn tag(defs: &Defs, t: spacebox_core::TagId) -> String {
    let t = defs.tag(t);
    match t.file {
        Some(f) => format!(
            "{}.{}",
            defs.files[f.idx()]
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            t.name
        ),
        None => t.name.clone(),
    }
}

fn targets(defs: &Defs, t: &Targets) -> Value {
    json!({
        "buildings": t.buildings.iter().map(|b| b.0).collect::<Vec<_>>(),
        "building_tags": t.tags.iter().map(|x| tag(defs, *x)).collect::<Vec<_>>(),
    })
}

/// The non-default parts of a modifier, for display.
fn modifier(m: &Modifier) -> Value {
    let mut o = serde_json::Map::new();
    let mut mult = |k: &str, v: f64| {
        if v != 1.0 {
            o.insert(k.into(), json!(v));
        }
    };
    mult("upkeep_mult", m.upkeep_mult);
    mult("input_mult", m.input_mult);
    mult("output_mult", m.output_mult);
    mult("passive_mult", m.passive_mult);
    mult("max_workers_mult", m.max_workers_mult);
    mult("housing_mult", m.housing_mult);
    if !m.upkeep_add.is_empty() {
        o.insert("upkeep_add".into(), amounts(&m.upkeep_add));
    }
    if !m.input_add.is_empty() {
        o.insert("input_add".into(), amounts(&m.input_add));
    }
    if !m.output_add.is_empty() {
        o.insert("output_add".into(), yields(&m.output_add));
    }
    if !m.passive_add.is_empty() {
        o.insert("passive_add".into(), yields(&m.passive_add));
    }
    if m.max_workers_add != 0.0 {
        o.insert("max_workers_add".into(), json!(m.max_workers_add));
    }
    if m.housing_add != 0.0 {
        o.insert("housing_add".into(), json!(m.housing_add));
    }
    if let Some(j) = m.job {
        o.insert("job".into(), json!(j.0));
    }
    Value::Object(o)
}

fn targeted(defs: &Defs, t: &TargetedModifier) -> Value {
    json!({
        "targets": targets(defs, &t.targets),
        "modifier": modifier(&t.modifier),
    })
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
            "transient": r.transient,
            "tradeable": r.tradeable,
            "storage_cap": rules.storage_cap(spacebox_core::ResourceId::from_idx(i)),
        })).collect::<Vec<_>>(),
        "jobs": d.jobs.iter().enumerate().map(|(i, j)| json!({
            "id": i,
            "name": j.key.name,
            "display_name": j.display_name,
            "appeal": j.appeal,
            "tags": j.tags.iter().map(|t| tag(d, *t)).collect::<Vec<_>>(),
            "associated_resources": j.associated_resources.iter().map(|r| r.0).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "species": d.species.iter().enumerate().map(|(i, s)| json!({
            "id": i,
            "name": qualified(s.key.file, &s.key.name),
            "display_name": s.display_name,
            "description": s.description,
            "upkeep": amounts(&s.upkeep),
            "starvation_resource": s.starvation_resource.map(|r| r.0),
            "comfort_resource": s.comfort_resource.map(|r| r.0),
            "growth_rate": s.growth_rate,
            "min_growth": s.min_growth,
            "starvation_rate": s.starvation_rate,
            "migration_rate": s.migration_rate,
        })).collect::<Vec<_>>(),
        "buildings": d.buildings.iter().enumerate().map(|(i, b)| json!({
            "id": i,
            "name": qualified(b.key.file, &b.key.name),
            "display_name": b.display_name,
            "description": b.description,
            "max_workers": b.max_workers,
            "job": b.job.map(|j| j.0),
            "tags": b.tags.iter().map(|c| d.tag_name(*c)).collect::<Vec<_>>(),
            "qualified_tags": b.tags.iter().map(|c| tag(d, *c)).collect::<Vec<_>>(),
            "barren": b.barren,
            "blocker": b.blocker,
            "land": b.land,
            "water": b.water,
            "space": b.space,
            "housing": b.housing,
            "build_cost": cost(&b.build_cost),
            "remove_cost": b.remove_cost.as_ref().map(cost),
            "upkeep": amounts(&b.upkeep),
            "inputs": amounts(&b.inputs),
            "outputs": yields(&b.outputs),
            "passive_outputs": yields(&b.passive_outputs),
            "requires_tech": b.requires_tech.iter().map(|t| t.0).collect::<Vec<_>>(),
            "max_per_planet": b.max_per_planet,
            "hp": b.hp.unwrap_or(rules.building_hp),
            "upgrades": b.available_upgrades.iter().map(|u| u.0).collect::<Vec<_>>(),
            "adjacency": b.adjacency.iter().map(|a| targeted(d, a)).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "upgrades": d.upgrades.iter().enumerate().map(|(i, u)| json!({
            "id": i,
            "name": qualified(u.key.file, &u.key.name),
            "display_name": u.display_name,
            "description": u.description,
            "applies_to": targets(d, &u.applies_to),
            "after_upgrades": u.after_upgrades.iter().map(|n| n.0).collect::<Vec<_>>(),
            "requires_tech": u.requires_tech.iter().map(|t| t.0).collect::<Vec<_>>(),
            "build_cost": cost(&u.build_cost),
            "modifier": modifier(&u.modifier),
        })).collect::<Vec<_>>(),
        "technologies": d.technologies.iter().enumerate().map(|(i, t)| json!({
            "id": i,
            "name": t.key.name,
            "display_name": t.display_name,
            "description": t.description,
            "tags": t.tags.iter().map(|x| tag(d, *x)).collect::<Vec<_>>(),
            "cost": amounts(&t.cost),
            "prerequisites": t.prerequisites.iter().map(|p| p.0).collect::<Vec<_>>(),
            "pop_growth_mult": t.pop_growth_mult,
            "fleet_size_add": t.fleet_size_add,
            "buildings": t.buildings.iter().map(|m| targeted(d, m)).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "terraforming": {
            "large": rules.terraform_large.as_ref().map(|t| json!({ "tech": t.requires_tech.map(|x| x.0), "cost": cost(&t.cost) })),
            "small": rules.terraform_small.as_ref().map(|t| json!({ "tech": t.requires_tech.map(|x| x.0), "cost": cost(&t.cost) })),
        },
        "ships": d.ships.iter().enumerate().map(|(i, s)| {
            let st = &s.base_stats;
            json!({
                "id": i,
                "name": qualified(s.key.file, &s.key.name),
                "display_name": s.display_name,
                "description": s.description,
                "requires_tech": s.requires_tech.iter().map(|t| t.0).collect::<Vec<_>>(),
                "build_cost": cost(&s.build_cost),
                "upkeep": amounts(&s.upkeep),
                "hull": st.max_hull,
                "shield": st.max_shield,
                "shield_regen": st.shield_regen,
                "speed": st.speed,
                "hyperjump_days": rules.hyperjump_days * st.hyperjump_mult,
                "range": st.max_range,
                "fleet_size": s.fleet_size,
                "weapons": s.weapons.iter().map(|w| w.0).collect::<Vec<_>>(),
                "outfits": s.outfits.iter().map(|o| o.0).collect::<Vec<_>>(),
                "dps": st.armament.iter().map(|a| (a.hull_damage + a.shield_damage) / a.cooldown).sum::<f64>(),
            })
        }).collect::<Vec<_>>(),
        "weapons": d.weapons.iter().enumerate().map(|(i, x)| json!({
            "id": i, "display_name": x.display_name, "shield_damage": x.shield_damage,
            "hull_damage": x.hull_damage, "fire_rate": x.fire_rate, "range": x.range,
        })).collect::<Vec<_>>(),
        "outfits": d.outfits.iter().enumerate().map(|(i, x)| json!({
            "id": i, "display_name": x.display_name, "description": x.description,
        })).collect::<Vec<_>>(),
        "rules": {
            "display_name": rules.display_name,
            "expansion_resource": rules.expansion_resource.map(|r| r.0),
            "expansion_base_cost": rules.expansion_base_cost,
            "expansion_base_days": rules.expansion_base_days,
            "hyperjump_days": rules.hyperjump_days,
            "arrival_radius": rules.arrival_radius,
            "building_disable_damage": rules.building_disable_damage,
            "base_fleet_size": rules.base_fleet_size,
            "research_speed_mult": rules.research_speed_mult,
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
