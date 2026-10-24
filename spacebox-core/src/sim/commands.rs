//! Queued interventions: player orders, governor (AI) decisions and game-master edits.
//!
//! Commands are queued with [`World::queue`] and applied serially at the start of the next tick,
//! before the parallel phases run. Invalid commands are dropped with a `CommandRejected` event.

use crate::defs::ResourceAmount;
use crate::ids::*;
use crate::sim::events::{Event, EventKind};
use crate::world::{BuildingInstance, Construction, Fleet, FleetOrder, ShipOrder, World};

#[derive(Clone, Debug)]
pub enum Command {
    /// Relocate pops between any two planets.
    MovePops {
        from: PlanetId,
        to: PlanetId,
        count: u64,
    },
    /// Pin exactly `workers` pops to a building (filled before appeal-based assignment), or
    /// unpin it with `None`.
    AssignWorkers {
        planet: PlanetId,
        tile: usize,
        workers: Option<u32>,
    },
    /// Restrict how many jobs a building offers (`None` = all).
    SetJobCap {
        planet: PlanetId,
        tile: usize,
        cap: Option<u32>,
    },
    SetEnabled {
        planet: PlanetId,
        tile: usize,
        enabled: bool,
    },
    /// Start construction. `empire` defaults to the planet's (or system's) owner and must be
    /// given when building in an unowned system, which costs influence and extra time and
    /// claims the system when the building is finished.
    Build {
        planet: PlanetId,
        tile: usize,
        building: BuildingId,
        empire: Option<EmpireId>,
    },
    /// Remove a building (paying its `remove_cost`, which is how natural blockers are
    /// cleared) or cancel a construction site. The planet's owner pays, or the system's owner
    /// for an unowned planet.
    Demolish {
        planet: PlanetId,
        tile: usize,
    },
    Upgrade {
        planet: PlanetId,
        tile: usize,
        upgrade: UpgradeId,
    },
    Terraform {
        planet: PlanetId,
    },
    SetResearch {
        empire: EmpireId,
        tech: Option<TechId>,
    },
    SetOwner {
        system: SystemId,
        owner: Option<EmpireId>,
    },
    SetCapital {
        empire: EmpireId,
        system: SystemId,
    },
    Link {
        a: SystemId,
        b: SystemId,
    },
    Unlink {
        a: SystemId,
        b: SystemId,
    },
    /// Add (or with a negative count, remove) resources from an empire's storage.
    GrantResources {
        empire: EmpireId,
        resource: ResourceId,
        count: f64,
    },
    /// Hand a planet to an empire (or none) directly.
    SetPlanetOwner {
        planet: PlanetId,
        owner: Option<EmpireId>,
    },
    /// Queue a ship at a shipyard planet. Non-on-demand costs are paid now; on-demand costs
    /// (shipbuilding) are supplied by the owner's shipyards over time.
    BuildShip {
        planet: PlanetId,
        ship: ShipId,
        fleet: Option<FleetId>,
    },
    /// Remove a queued ship order, refunding what was paid up front.
    CancelShipOrder {
        empire: EmpireId,
        index: usize,
    },
    /// Send a fleet to a system (hyperjumping along the shortest route), optionally to a point
    /// within it.
    MoveFleet {
        fleet: FleetId,
        system: SystemId,
        coordinates: Option<[f64; 2]>,
    },
    /// Send a fleet to bombard a planet of an empire it is at war with (or an unowned one).
    Bombard {
        fleet: FleetId,
        planet: PlanetId,
        tile: Option<usize>,
    },
    StopFleet {
        fleet: FleetId,
    },
    /// Move all ships of `from` into `into` (same owner, same system).
    MergeFleets {
        into: FleetId,
        from: FleetId,
    },
    /// Split the ships at `ships` (indices) off into a new fleet.
    SplitFleet {
        fleet: FleetId,
        ships: Vec<usize>,
    },
    RenameFleet {
        fleet: FleetId,
        display_name: String,
    },
    DisbandFleet {
        fleet: FleetId,
    },
    DeclareWar {
        empire: EmpireId,
        target: EmpireId,
    },
    MakePeace {
        empire: EmpireId,
        target: EmpireId,
    },
}

/// What starting a construction will cost and how long it will take.
#[derive(Clone, Debug)]
pub struct BuildPlan {
    pub cost: Vec<ResourceAmount>,
    pub days: u32,
    /// Hyperlane jumps beyond the builder's borders, when building in an unowned system.
    pub expansion_jumps: Option<u32>,
}

type CmdResult = Result<Option<Event>, String>;

pub fn can_afford(storage: &[f64], cost: &[ResourceAmount]) -> bool {
    cost.iter()
        .all(|a| storage[a.resource.idx()] + 1e-9 >= a.count)
}

fn pay(storage: &mut [f64], cost: &[ResourceAmount]) {
    for a in cost {
        storage[a.resource.idx()] -= a.count;
    }
}

fn describe_cost(world: &World, cost: &[ResourceAmount]) -> String {
    cost.iter()
        .map(|a| {
            format!(
                "{:.0} {}",
                a.count,
                world.defs.resource(a.resource).display_name
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl World {
    fn check_planet(&self, planet: PlanetId) -> Result<(), String> {
        if planet.idx() >= self.planets.len() {
            return Err(format!("no planet with id {}", planet.0));
        }
        Ok(())
    }

    fn check_tile(&self, planet: PlanetId, tile: usize) -> Result<(), String> {
        self.check_planet(planet)?;
        let p = self.planet(planet);
        if tile >= p.tiles.len() {
            return Err(format!("{} has no tile {tile}", p.display_name));
        }
        Ok(())
    }

    /// Checks whether `empire` may build `building` on `planet`/`tile` (ownership, expansion,
    /// barren rules, technology, limits) and returns the full cost and duration.
    pub fn build_plan(
        &self,
        empire: EmpireId,
        planet: PlanetId,
        tile: usize,
        building: BuildingId,
    ) -> Result<BuildPlan, String> {
        self.build_plan_with(empire, planet, tile, building, None)
    }

    /// [`World::build_plan`] with the empire's border distances
    /// ([`World::jumps_from_borders`]) precomputed, for callers checking many sites.
    pub fn build_plan_with(
        &self,
        empire: EmpireId,
        planet: PlanetId,
        tile: usize,
        building: BuildingId,
        borders: Option<&[u32]>,
    ) -> Result<BuildPlan, String> {
        self.check_tile(planet, tile)?;
        if empire.idx() >= self.empires.len() {
            return Err(format!("no empire with id {}", empire.0));
        }
        let p = self.planet(planet);
        let sys = self.system(p.system);
        let def = self.defs.building(building);
        let e = self.empire(empire);
        if let Some(o) = sys.owner.filter(|o| *o != empire) {
            return Err(format!(
                "{} is owned by {}: you cannot build in a system you do not own",
                sys.display_name,
                self.empire(o).display_name
            ));
        }
        if let Some(o) = p.owner.filter(|o| *o != empire) {
            return Err(format!(
                "{} belongs to {}",
                p.display_name,
                self.empire(o).display_name
            ));
        }
        let t = &p.tiles[tile];
        if let Some(b) = t
            .building
            .as_ref()
            .filter(|b| self.defs.building(b.def).blocker)
        {
            return Err(format!(
                "tile {tile} of {} is blocked by {}; clear it first",
                p.display_name,
                self.defs.building(b.def).display_name
            ));
        }
        if !t.is_free() {
            return Err(format!("tile {tile} of {} is occupied", p.display_name));
        }
        if def.blocker {
            return Err(format!(
                "{} occurs naturally and cannot be constructed",
                def.display_name
            ));
        }
        if def.space {
            return Err(format!(
                "{} must be built on an orbital megastructure (not available yet)",
                def.display_name
            ));
        }
        if !def.buildable {
            return Err(format!("{} cannot be constructed", def.display_name));
        }
        if def.land && t.water {
            return Err(format!(
                "{} must be built on land, not water",
                def.display_name
            ));
        }
        if def.water && !t.water {
            return Err(format!(
                "{} must be built on a water tile",
                def.display_name
            ));
        }
        if !p.habitable && !def.barren {
            return Err(format!(
                "{} cannot be built on barren {}",
                def.display_name, p.display_name
            ));
        }
        if let Some(t) = def.requires_tech.iter().find(|t| !e.knows(**t)) {
            return Err(format!(
                "{} requires {}",
                def.display_name,
                self.defs.tech(*t).display_name
            ));
        }
        if let Some(max) = def.max_per_planet
            && p.building_count(building) >= max as usize
        {
            return Err(format!(
                "{} already has the maximum number of {}",
                p.display_name, def.display_name
            ));
        }
        let mut plan = BuildPlan {
            cost: def.build_cost.clone(),
            days: def.build_days,
            expansion_jumps: None,
        };
        if sys.owner.is_none() {
            let computed;
            let dist = match borders {
                Some(d) => d,
                None => {
                    computed = self.jumps_from_borders(empire);
                    &computed
                }
            };
            // Every owned system is at distance 0, so the minimum tells whether there are any.
            let owns_any = dist.contains(&0);
            let jumps = if owns_any {
                dist[p.system.idx()]
            } else {
                1 // an empire without territory may found a colony anywhere at the base cost
            };
            if jumps == u32::MAX {
                return Err(format!("no hyperlane route to {}", sys.display_name));
            }
            let rules = self.rules();
            let (influence, days) = rules.expansion(jumps);
            if let Some(r) = rules.expansion_resource {
                match plan.cost.iter_mut().find(|a| a.resource == r) {
                    Some(a) => a.count += influence,
                    None => plan.cost.push(ResourceAmount {
                        resource: r,
                        count: influence,
                    }),
                }
            }
            plan.days += days;
            plan.expansion_jumps = Some(jumps);
        }
        Ok(plan)
    }

    /// [`World::build_plan`] plus a check that `storage` covers the cost.
    pub fn can_build(
        &self,
        empire: EmpireId,
        planet: PlanetId,
        tile: usize,
        building: BuildingId,
        storage: &[f64],
    ) -> Result<BuildPlan, String> {
        self.can_build_with(empire, planet, tile, building, storage, None)
    }

    pub fn can_build_with(
        &self,
        empire: EmpireId,
        planet: PlanetId,
        tile: usize,
        building: BuildingId,
        storage: &[f64],
        borders: Option<&[u32]>,
    ) -> Result<BuildPlan, String> {
        let plan = self.build_plan_with(empire, planet, tile, building, borders)?;
        if !can_afford(storage, &plan.cost) {
            return Err(format!(
                "cannot afford {} ({})",
                self.defs.building(building).display_name,
                describe_cost(self, &plan.cost)
            ));
        }
        Ok(plan)
    }

    /// Who would pay to demolish the building on `planet`/`tile`, and how much.
    pub fn demolish_plan(
        &self,
        planet: PlanetId,
        tile: usize,
    ) -> Result<(EmpireId, Vec<ResourceAmount>), String> {
        self.check_tile(planet, tile)?;
        let p = self.planet(planet);
        let Some(b) = &p.tiles[tile].building else {
            return Err(format!("tile {tile} of {} has no building", p.display_name));
        };
        let payer = p
            .owner
            .or(self.system(p.system).owner)
            .ok_or("nobody owns this system: claim it before clearing anything")?;
        Ok((payer, self.defs.building(b.def).remove_cost.clone()))
    }

    /// Whether `planet` has a working building that produces an on-demand resource.
    pub fn has_shipyard(&self, planet: PlanetId) -> bool {
        let rules = self.rules();
        self.planet(planet).tiles.iter().any(|t| {
            t.building.as_ref().is_some_and(|b| {
                b.enabled
                    && b.damage < rules.building_disable_damage
                    && self.defs.produces_on_demand(b.def)
            })
        })
    }

    pub fn can_build_ship(
        &self,
        planet: PlanetId,
        ship: ShipId,
        storage: &[f64],
    ) -> Result<Vec<ResourceAmount>, String> {
        self.check_planet(planet)?;
        let p = self.planet(planet);
        let Some(owner) = p.owner else {
            return Err(format!("{} is not owned by any empire", p.display_name));
        };
        if ship.idx() >= self.defs.ships.len() {
            return Err(format!("no ship design with id {}", ship.0));
        }
        let d = self.defs.ship(ship);
        if !self.has_shipyard(planet) {
            return Err(format!("{} has no working shipyard", p.display_name));
        }
        if let Some(t) = d
            .requires_tech
            .iter()
            .find(|t| !self.empire(owner).knows(**t))
        {
            return Err(format!(
                "{} requires {}",
                d.display_name,
                self.defs.tech(*t).display_name
            ));
        }
        let upfront: Vec<ResourceAmount> = d
            .build_cost
            .iter()
            .filter(|a| !self.defs.resource(a.resource).on_demand)
            .copied()
            .collect();
        if !can_afford(storage, &upfront) {
            return Err(format!(
                "cannot afford a {} ({})",
                d.display_name,
                describe_cost(self, &upfront)
            ));
        }
        Ok(upfront)
    }

    fn check_fleet(&self, fleet: FleetId) -> Result<&Fleet, String> {
        self.fleet(fleet)
            .ok_or_else(|| format!("no fleet with id {}", fleet.0))
    }

    pub fn can_upgrade(
        &self,
        planet: PlanetId,
        tile: usize,
        upgrade: UpgradeId,
        storage: &[f64],
    ) -> Result<(), String> {
        self.check_tile(planet, tile)?;
        let p = self.planet(planet);
        let up = self.defs.upgrade(upgrade);
        let Some(owner) = self.planet_owner(planet) else {
            return Err(format!("{} is not owned by any empire", p.display_name));
        };
        let t = &p.tiles[tile];
        let Some(inst) = &t.building else {
            return Err(format!("tile {tile} of {} has no building", p.display_name));
        };
        if t.construction.is_some() {
            return Err(format!(
                "tile {tile} of {} is already under construction",
                p.display_name
            ));
        }
        let def = self.defs.building(inst.def);
        if !def.available_upgrades.contains(&upgrade) {
            return Err(format!(
                "{} cannot be applied to {}",
                up.display_name, def.display_name
            ));
        }
        if inst.upgrades.contains(&upgrade) {
            return Err(format!(
                "{} already has {}",
                def.display_name, up.display_name
            ));
        }
        if let Some(n) = up
            .needs_upgrades
            .iter()
            .find(|n| !inst.upgrades.contains(n))
        {
            return Err(format!(
                "{} first needs {}",
                up.display_name,
                self.defs.upgrade(*n).display_name
            ));
        }
        if let Some(t) = up
            .requires_tech
            .iter()
            .find(|t| !self.empire(owner).knows(**t))
        {
            return Err(format!(
                "{} requires {}",
                up.display_name,
                self.defs.tech(*t).display_name
            ));
        }
        if !can_afford(storage, &up.cost) {
            return Err(format!(
                "cannot afford {} ({})",
                up.display_name,
                describe_cost(self, &up.cost)
            ));
        }
        Ok(())
    }

    pub fn can_terraform(&self, planet: PlanetId, storage: &[f64]) -> Result<(), String> {
        self.check_planet(planet)?;
        let p = self.planet(planet);
        let Some(owner) = self.planet_owner(planet) else {
            return Err(format!("{} is not owned by any empire", p.display_name));
        };
        if p.habitable {
            return Err(format!("{} is already habitable", p.display_name));
        }
        if p.terraform_days_left.is_some() {
            return Err(format!("{} is already being terraformed", p.display_name));
        }
        let Some(rule) = self.rules().terraform(p.size) else {
            return Err(format!("{} planets cannot be terraformed", p.size.as_str()));
        };
        if let Some(t) = rule.requires_tech.filter(|t| !self.empire(owner).knows(*t)) {
            return Err(format!(
                "terraforming {} planets requires {}",
                p.size.as_str(),
                self.defs.tech(t).display_name
            ));
        }
        if !can_afford(storage, &rule.cost) {
            return Err(format!(
                "cannot afford terraforming ({})",
                describe_cost(self, &rule.cost)
            ));
        }
        Ok(())
    }

    /// Applies a command immediately instead of queueing it.
    pub fn apply_now(&mut self, cmd: Command) -> Result<(), String> {
        let ev = apply(self, cmd)?;
        self.events.extend(ev);
        Ok(())
    }
}

pub(crate) fn apply(world: &mut World, cmd: Command) -> CmdResult {
    let day = world.day;
    match cmd {
        Command::MovePops { from, to, count } => {
            world.check_planet(from)?;
            world.check_planet(to)?;
            let n = count.min(world.planet(from).pop);
            world.planet_mut(from).pop -= n;
            world.planet_mut(to).pop += n;
            let msg = format!(
                "{n} pops relocated from {} to {}",
                world.planet(from).display_name,
                world.planet(to).display_name
            );
            Ok(Some(Event::new(day, EventKind::Command, msg).planet(to)))
        }
        Command::AssignWorkers {
            planet,
            tile,
            workers,
        } => {
            world.check_tile(planet, tile)?;
            let p = world.planet_mut(planet);
            let b = p.tiles[tile]
                .building
                .as_mut()
                .ok_or("no building on that tile")?;
            b.job_cap = workers;
            b.pinned = workers.is_some();
            p.dirty = true;
            Ok(None)
        }
        Command::SetJobCap { planet, tile, cap } => {
            world.check_tile(planet, tile)?;
            let p = world.planet_mut(planet);
            let b = p.tiles[tile]
                .building
                .as_mut()
                .ok_or("no building on that tile")?;
            b.job_cap = cap;
            b.pinned = false;
            p.dirty = true;
            Ok(None)
        }
        Command::SetEnabled {
            planet,
            tile,
            enabled,
        } => {
            world.check_tile(planet, tile)?;
            let p = world.planet_mut(planet);
            let b = p.tiles[tile]
                .building
                .as_mut()
                .ok_or("no building on that tile")?;
            b.enabled = enabled;
            p.dirty = true;
            Ok(None)
        }
        Command::Build {
            planet,
            tile,
            building,
            empire,
        } => {
            world.check_planet(planet)?;
            let p = world.planet(planet);
            let owner = empire
                .or(p.owner)
                .or(world.system(p.system).owner)
                .ok_or("nobody owns this system: specify which empire builds")?;
            let plan =
                world.can_build(owner, planet, tile, building, &world.empire(owner).storage)?;
            let def = world.defs.building(building).clone();
            pay(&mut world.empires[owner.idx()].storage, &plan.cost);
            let p = world.planet_mut(planet);
            if plan.days == 0 {
                p.tiles[tile].building = Some(BuildingInstance::new(building));
                if p.owner.is_none() {
                    p.claims.push(owner);
                }
            } else {
                p.tiles[tile].construction = Some(Construction::Building {
                    def: building,
                    days_left: plan.days,
                    empire: Some(owner),
                    paid: plan.cost.clone(),
                });
            }
            p.dirty = true;
            let msg = match plan.expansion_jumps {
                Some(_) => format!(
                    "{} stakes a claim on {}, building {} ({} days)",
                    world.empire(owner).display_name,
                    world.planet(planet).display_name,
                    def.display_name,
                    plan.days
                ),
                None => format!(
                    "{} begins construction of {} on {}",
                    world.empire(owner).display_name,
                    def.display_name,
                    world.planet(planet).display_name
                ),
            };
            Ok(Some(
                Event::new(day, EventKind::ConstructionStarted, msg)
                    .empire(Some(owner))
                    .planet(planet),
            ))
        }
        Command::Demolish { planet, tile } => {
            world.check_tile(planet, tile)?;
            let t = &world.planet(planet).tiles[tile];
            if t.building.is_none() {
                if t.construction.is_none() {
                    return Err("nothing to demolish".into());
                }
                let p = world.planet_mut(planet);
                p.tiles[tile].construction = None;
                p.dirty = true;
                return Ok(None);
            }
            let (payer, cost) = world.demolish_plan(planet, tile)?;
            if !can_afford(&world.empire(payer).storage, &cost) {
                return Err(format!(
                    "cannot afford to clear it ({})",
                    describe_cost(world, &cost)
                ));
            }
            pay(&mut world.empires[payer.idx()].storage, &cost);
            let p = world.planet_mut(planet);
            let def = p.tiles[tile].building.take().unwrap().def;
            p.tiles[tile].construction = None;
            p.dirty = true;
            let d = world.defs.building(def);
            let verb = if d.blocker { "clears" } else { "demolishes" };
            let msg = format!(
                "{} {verb} the {} on {}",
                world.empire(payer).display_name,
                d.display_name,
                world.planet(planet).display_name
            );
            Ok(Some(
                Event::new(day, EventKind::Command, msg)
                    .empire(Some(payer))
                    .planet(planet),
            ))
        }
        Command::Upgrade {
            planet,
            tile,
            upgrade,
        } => {
            let owner = world.planet_owner(planet).ok_or("planet is unowned")?;
            world.can_upgrade(planet, tile, upgrade, &world.empire(owner).storage)?;
            let up = world.defs.upgrade(upgrade).clone();
            pay(&mut world.empires[owner.idx()].storage, &up.cost);
            let p = world.planet_mut(planet);
            if up.build_days == 0 {
                p.tiles[tile]
                    .building
                    .as_mut()
                    .unwrap()
                    .upgrades
                    .push(upgrade);
            } else {
                p.tiles[tile].construction = Some(Construction::Upgrade {
                    upgrade,
                    days_left: up.build_days,
                });
            }
            p.dirty = true;
            let msg = format!(
                "{} begins installing {} on {}",
                world.empire(owner).display_name,
                up.display_name,
                world.planet(planet).display_name
            );
            Ok(Some(
                Event::new(day, EventKind::ConstructionStarted, msg)
                    .empire(Some(owner))
                    .planet(planet),
            ))
        }
        Command::Terraform { planet } => {
            let owner = world.planet_owner(planet).ok_or("planet is unowned")?;
            world.can_terraform(planet, &world.empire(owner).storage)?;
            let rule = world
                .rules()
                .terraform(world.planet(planet).size)
                .unwrap()
                .clone();
            pay(&mut world.empires[owner.idx()].storage, &rule.cost);
            world.planet_mut(planet).terraform_days_left = Some(rule.days.max(1));
            let msg = format!(
                "{} begins terraforming {}",
                world.empire(owner).display_name,
                world.planet(planet).display_name
            );
            Ok(Some(
                Event::new(day, EventKind::ConstructionStarted, msg)
                    .empire(Some(owner))
                    .planet(planet),
            ))
        }
        Command::SetResearch { empire, tech } => {
            let e = world.empires.get(empire.idx()).ok_or("no such empire")?;
            if let Some(t) = tech
                && !world.defs.tech_available(t, &e.techs)
            {
                return Err(format!(
                    "{} is not available to research",
                    world.defs.tech(t).display_name
                ));
            }
            let e = &mut world.empires[empire.idx()];
            if e.researching != tech {
                e.researching = tech;
                e.research_progress = 0.0;
            }
            Ok(None)
        }
        Command::SetOwner { system, owner } => {
            let s = world
                .systems
                .get_mut(system.idx())
                .ok_or("no such system")?;
            if owner.is_some_and(|o| o.idx() >= world.empires.len()) {
                return Err("no such empire".into());
            }
            // Transferring a system transfers all of its planets.
            s.owner = owner;
            for p in s.planet_range() {
                world.planets[p].owner = owner;
                world.planets[p].siege = 0.0;
                world.planets[p].dirty = true;
            }
            world.topology_dirty = true;
            let name = &world.systems[system.idx()].display_name;
            let msg = match owner {
                Some(o) => format!(
                    "{name} is now controlled by {}",
                    world.empire(o).display_name
                ),
                None => format!("{name} is no longer controlled by any empire"),
            };
            Ok(Some(Event::new(day, EventKind::Command, msg).empire(owner)))
        }
        Command::SetCapital { empire, system } => {
            let s = world.systems.get(system.idx()).ok_or("no such system")?;
            let present = s.owner == Some(empire)
                || s.planet_ids()
                    .any(|p| world.planet(p).owner == Some(empire));
            if !present {
                return Err(format!(
                    "{} has nothing owned by that empire",
                    s.display_name
                ));
            }
            world.empires[empire.idx()].capital = Some(system);
            world.topology_dirty = true;
            Ok(None)
        }
        Command::Link { a, b } => {
            if a.idx() >= world.systems.len() || b.idx() >= world.systems.len() {
                return Err("no such system".into());
            }
            world.link(a, b);
            Ok(None)
        }
        Command::Unlink { a, b } => {
            if a.idx() >= world.systems.len() || b.idx() >= world.systems.len() {
                return Err("no such system".into());
            }
            world.systems[a.idx()].links.retain(|s| *s != b);
            world.systems[b.idx()].links.retain(|s| *s != a);
            world.topology_dirty = true;
            Ok(None)
        }
        Command::GrantResources {
            empire,
            resource,
            count,
        } => {
            let e = world
                .empires
                .get_mut(empire.idx())
                .ok_or("no such empire")?;
            let s = &mut e.storage[resource.idx()];
            *s = (*s + count).max(0.0);
            Ok(None)
        }
        Command::SetPlanetOwner { planet, owner } => {
            world.check_planet(planet)?;
            if owner.is_some_and(|o| o.idx() >= world.empires.len()) {
                return Err("no such empire".into());
            }
            world.set_planet_owner(planet, owner);
            let p = world.planet(planet);
            let msg = match owner {
                Some(o) => format!(
                    "{} now belongs to {}",
                    p.display_name,
                    world.empire(o).display_name
                ),
                None => format!("{} is no longer ruled by any empire", p.display_name),
            };
            Ok(Some(
                Event::new(day, EventKind::Command, msg)
                    .empire(owner)
                    .planet(planet),
            ))
        }
        Command::BuildShip {
            planet,
            ship,
            fleet,
        } => {
            world.check_planet(planet)?;
            let owner = world.planet(planet).owner.ok_or("planet is unowned")?;
            let upfront = world.can_build_ship(planet, ship, &world.empire(owner).storage)?;
            pay(&mut world.empires[owner.idx()].storage, &upfront);
            let remaining = crate::loader::on_demand_cost(&world.defs, ship);
            world.empires[owner.idx()].ship_queue.push(ShipOrder {
                ship,
                planet,
                remaining,
                fleet,
            });
            Ok(None)
        }
        Command::CancelShipOrder { empire, index } => {
            let e = world.empires.get(empire.idx()).ok_or("no such empire")?;
            if index >= e.ship_queue.len() {
                return Err("no such ship order".into());
            }
            let order = world.empires[empire.idx()].ship_queue.remove(index);
            let defs = world.defs.clone();
            for a in defs
                .ship(order.ship)
                .build_cost
                .iter()
                .filter(|a| !defs.resource(a.resource).on_demand)
            {
                world.empires[empire.idx()].storage[a.resource.idx()] += a.count;
            }
            Ok(None)
        }
        Command::MoveFleet {
            fleet,
            system,
            coordinates,
        } => {
            let f = world.check_fleet(fleet)?;
            if system.idx() >= world.systems.len() {
                return Err("no such system".into());
            }
            if f.system != system && world.next_hop(f.system, system).is_none() {
                return Err(format!(
                    "no hyperlane route to {}",
                    world.system(system).display_name
                ));
            }
            if let Some(c) = coordinates
                && !(c[0].is_finite() && c[1].is_finite())
            {
                return Err("coordinates must be finite".into());
            }
            world.fleet_mut(fleet).unwrap().order = FleetOrder::Move {
                system,
                coordinates,
            };
            Ok(None)
        }
        Command::Bombard {
            fleet,
            planet,
            tile,
        } => {
            world.check_planet(planet)?;
            let f = world.check_fleet(fleet)?;
            let p = world.planet(planet);
            match p.owner {
                Some(o) if o == f.owner => {
                    return Err("a fleet cannot bombard its own planet".into());
                }
                Some(o) if !world.at_war(f.owner, o) => {
                    return Err(format!(
                        "{} is not at war with {}",
                        world.empire(f.owner).display_name,
                        world.empire(o).display_name
                    ));
                }
                _ => {}
            }
            if f.system != p.system && world.next_hop(f.system, p.system).is_none() {
                return Err(format!("no hyperlane route to {}", p.display_name));
            }
            if f.max_range(&world.defs) <= 0.0 {
                return Err(format!("{} has no weapons", f.display_name));
            }
            world.fleet_mut(fleet).unwrap().order = FleetOrder::Bombard { planet, tile };
            Ok(None)
        }
        Command::StopFleet { fleet } => {
            world.check_fleet(fleet)?;
            world.fleet_mut(fleet).unwrap().order = FleetOrder::Idle;
            Ok(None)
        }
        Command::MergeFleets { into, from } => {
            let (a, b) = (world.check_fleet(into)?, world.check_fleet(from)?);
            if into == from {
                return Err("cannot merge a fleet with itself".into());
            }
            if a.owner != b.owner || a.system != b.system {
                return Err("fleets must share an owner and a system to merge".into());
            }
            let ships = std::mem::take(&mut world.fleet_mut(from).unwrap().ships);
            world.fleet_mut(into).unwrap().ships.extend(ships);
            world.remove_fleet(from);
            Ok(None)
        }
        Command::SplitFleet { fleet, mut ships } => {
            let f = world.check_fleet(fleet)?;
            ships.sort_unstable();
            ships.dedup();
            if ships.is_empty() || ships.iter().any(|i| *i >= f.ships.len()) {
                return Err("invalid ship selection".into());
            }
            if ships.len() == f.ships.len() {
                return Err("cannot split off every ship".into());
            }
            let mut new = Fleet::new(
                String::new(),
                format!("{} (detachment)", f.display_name),
                f.owner,
                f.system,
            );
            new.coordinates = f.coordinates;
            let src = world.fleet_mut(fleet).unwrap();
            for i in ships.into_iter().rev() {
                new.ships.push(src.ships.remove(i));
            }
            new.ships.reverse();
            world.add_fleet(new);
            Ok(None)
        }
        Command::RenameFleet {
            fleet,
            display_name,
        } => {
            world.check_fleet(fleet)?;
            world.fleet_mut(fleet).unwrap().display_name = display_name;
            Ok(None)
        }
        Command::DisbandFleet { fleet } => {
            world.check_fleet(fleet)?;
            world.remove_fleet(fleet);
            Ok(None)
        }
        Command::DeclareWar { empire, target } => set_war(world, empire, target, true),
        Command::MakePeace { empire, target } => set_war(world, empire, target, false),
    }
}

fn set_war(world: &mut World, empire: EmpireId, target: EmpireId, war: bool) -> CmdResult {
    let n = world.empires.len();
    if empire.idx() >= n || target.idx() >= n {
        return Err("no such empire".into());
    }
    if empire == target {
        return Err("an empire cannot be at war with itself".into());
    }
    if world.at_war(empire, target) == war {
        return Err(if war {
            "already at war"
        } else {
            "already at peace"
        }
        .into());
    }
    world.set_war(empire, target, war);
    let (a, b) = (
        &world.empire(empire).display_name,
        &world.empire(target).display_name,
    );
    let (kind, msg) = if war {
        (EventKind::WarDeclared, format!("{a} declares war on {b}"))
    } else {
        // Peace stands down any bombardment of the former enemy.
        (EventKind::PeaceMade, format!("{a} makes peace with {b}"))
    };
    if !war {
        for f in &mut world.fleets {
            if let FleetOrder::Bombard { planet, .. } = f.order {
                let owner = world.planets.get(planet.idx()).and_then(|p| p.owner);
                if (f.owner == empire && owner == Some(target))
                    || (f.owner == target && owner == Some(empire))
                {
                    f.order = FleetOrder::Idle;
                }
            }
        }
    }
    Ok(Some(Event::new(world.day, kind, msg).empire(Some(empire))))
}
