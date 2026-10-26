//! Queued interventions: player orders, governor (AI) decisions and game-master edits.
//!
//! Commands are queued with [`World::queue`] and applied serially at the start of the next tick,
//! before the parallel phases run. Invalid commands are dropped with a `CommandRejected` event.
//!
//! Projects (constructions, removals, upgrades, terraforming, research, ships) do not need to
//! be affordable up front: they draw what they need from the stockpile as it becomes
//! available. Cancelling one refunds the normal resources already spent on it.

use crate::defs::{Cost, Defs, ResourceAmount, Ruleset};
use crate::ids::*;
use crate::sim::events::{Event, EventKind};
use crate::sim::war;
use crate::world::{
    BuildingInstance, Construction, Fleet, FleetOrder, ProjectKind, ResearchProject, ShipProject,
    TerraformProject, WarGoal, WarOutcome, World,
};

#[derive(Clone, Debug)]
pub enum Command {
    /// Relocate pops between any two planets (spread over species by their numbers), as many
    /// as the destination has free housing for.
    MovePops {
        from: PlanetId,
        to: PlanetId,
        count: u64,
    },
    /// Set the number of pops of one species on a planet.
    SetPopulation {
        planet: PlanetId,
        species: SpeciesId,
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
    /// Cancel a tile's construction project (refunding what was spent), or else remove its
    /// building: instantly if the building has no `remove-cost`, otherwise as a removal
    /// project (which is how natural blockers are cleared). The planet's owner pays, or the
    /// system's owner for an unowned planet.
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
    CancelTerraform {
        planet: PlanetId,
    },
    /// Add a technology to the empire's current research.
    Research {
        empire: EmpireId,
        tech: TechId,
    },
    CancelResearch {
        empire: EmpireId,
        tech: TechId,
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
    /// Start building a ship at a planet of the owner's. It is funded like any project:
    /// normal resources from the stockpile, shipbuilding from the empire's shipyards.
    BuildShip {
        planet: PlanetId,
        ship: ShipId,
    },
    /// Cancel a ship under construction (by its index in `shipbuilding`), refunding what was
    /// spent on it.
    CancelShip {
        empire: EmpireId,
        index: usize,
    },
    /// Replace a fleet's order queue (or with `append`, add to it). Jumps must follow
    /// hyperlanes, and bombardment targets must be in the system the fleet will be in.
    SetOrders {
        fleet: FleetId,
        orders: Vec<FleetOrder>,
        append: bool,
    },
    /// Convenience: replace a fleet's orders with jumps along the shortest route to `system`,
    /// then optionally a move to a point within it.
    MoveFleet {
        fleet: FleetId,
        system: SystemId,
        coordinates: Option<[f64; 2]>,
    },
    /// Convenience: replace a fleet's orders with jumps along the shortest route to the
    /// planet's system, then bombardment of the planet (a tile, or any building).
    Bombard {
        fleet: FleetId,
        planet: PlanetId,
        tile: Option<usize>,
    },
    /// Clear a fleet's orders.
    StopFleet {
        fleet: FleetId,
    },
    /// Move all ships of `from` into `into` (same owner, same system, within the fleet size
    /// limit).
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
    /// Declare a war with `goal` on `target` (costs influence; see [`World::can_declare_war`]).
    DeclareWar {
        empire: EmpireId,
        target: EmpireId,
        goal: WarGoal,
    },
    /// Set the defense's goal in a war declared on `empire` (free).
    SetWarGoal {
        empire: EmpireId,
        war: WarId,
        goal: WarGoal,
    },
    /// Propose ending a war with `outcome` (the war ends if the other side proposed the same).
    ProposePeace {
        empire: EmpireId,
        war: WarId,
        outcome: WarOutcome,
    },
    /// Accept the other side's proposal.
    AcceptPeace {
        empire: EmpireId,
        war: WarId,
    },
    /// End a war with `outcome`, once the other side's exhaustion is 1.
    EnforcePeace {
        empire: EmpireId,
        war: WarId,
        outcome: WarOutcome,
    },
    /// Claim another empire's system (costs influence like expanding there).
    Claim {
        empire: EmpireId,
        system: SystemId,
    },
    DropClaim {
        empire: EmpireId,
        system: SystemId,
    },
    CreateAlliance {
        empire: EmpireId,
        display_name: String,
    },
    JoinAlliance {
        empire: EmpireId,
        alliance: AllianceId,
    },
    LeaveAlliance {
        empire: EmpireId,
        alliance: AllianceId,
    },
    /// Change an empire's policy (fields left `None` stay as they are).
    SetPolicy {
        empire: EmpireId,
        accepts_migrants: Option<bool>,
        market_access: Option<bool>,
        total_war: Option<bool>,
    },
}

/// What starting a construction will cost.
#[derive(Clone, Debug)]
pub struct BuildPlan {
    /// The building's cost plus any expansion surcharge (influence and days).
    pub cost: Cost,
    /// Hyperlane jumps beyond the builder's borders, when building in an unowned system.
    pub expansion_jumps: Option<u32>,
}

type CmdResult = Result<Option<Event>, String>;

/// Whether `storage` covers every normal resource in `cost` (transient ones are ignored).
pub fn can_afford(defs: &Defs, storage: &[f64], cost: &[ResourceAmount]) -> bool {
    cost.iter()
        .filter(|a| !defs.is_transient(a.resource))
        .all(|a| storage[a.resource.idx()] + 1e-9 >= a.count)
}

/// Gives back what a cancelled project paid, except the expansion resource (influence),
/// which is never refunded.
pub(crate) fn refund(rules: &Ruleset, paid: &[ResourceAmount], storage: &mut [f64]) {
    for a in paid {
        if Some(a.resource) != rules.expansion_resource {
            storage[a.resource.idx()] += a.count;
        }
    }
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
    /// terrain, technology, limits) and returns the full cost.
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
        if def.space && !def.land && !def.water {
            return Err(format!(
                "{} must be built on an orbital megastructure (not available yet)",
                def.display_name
            ));
        }
        if !def.fits_terrain(t.water) {
            return Err(if t.water {
                format!("{} cannot be built on water", def.display_name)
            } else {
                format!("{} must be built on a water tile", def.display_name)
            });
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
            expansion_jumps: None,
        };
        if sys.owner.is_none() {
            // An empire without territory may found a colony anywhere at the base cost.
            let jumps = match borders {
                Some(d) if d.contains(&0) => d[p.system.idx()],
                Some(_) => 1,
                None if self.owned_systems(empire).is_empty() => 1,
                None => self.jumps_to(empire, p.system).unwrap_or(u32::MAX),
            };
            if jumps == u32::MAX {
                return Err(format!("no hyperlane route to {}", sys.display_name));
            }
            let rules = self.rules();
            let (influence, days) = rules.expansion(jumps);
            if let Some(r) = rules.expansion_resource {
                crate::defs::add_amounts(
                    &mut plan.cost.resources,
                    &[ResourceAmount {
                        resource: r,
                        count: influence,
                    }],
                );
            }
            plan.cost.days += days;
            plan.expansion_jumps = Some(jumps);
        }
        Ok(plan)
    }

    /// [`World::build_plan`] plus a check that `storage` already covers the cost (used by
    /// governors to stay within budget).
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
        if !can_afford(&self.defs, storage, &plan.cost.resources) {
            return Err(format!(
                "cannot afford {} ({})",
                self.defs.building(building).display_name,
                describe_cost(self, &plan.cost)
            ));
        }
        Ok(plan)
    }

    /// Who would pay to remove the building on `planet`/`tile`, and what it costs (`None`:
    /// free and instant).
    pub fn demolish_plan(
        &self,
        planet: PlanetId,
        tile: usize,
    ) -> Result<(EmpireId, Option<Cost>), String> {
        self.check_tile(planet, tile)?;
        let p = self.planet(planet);
        let t = &p.tiles[tile];
        let Some(b) = &t.building else {
            return Err(format!("tile {tile} of {} has no building", p.display_name));
        };
        if t.construction.is_some() {
            return Err(format!(
                "tile {tile} of {} is under construction",
                p.display_name
            ));
        }
        let payer = p
            .owner
            .or(self.system(p.system).owner)
            .ok_or("nobody owns this system: claim it before clearing anything")?;
        Ok((payer, self.defs.building(b.def).remove_cost.clone()))
    }

    /// Checks whether the owner of `planet` may start building `ship` there.
    pub fn can_build_ship(&self, planet: PlanetId, ship: ShipId) -> Result<EmpireId, String> {
        self.check_planet(planet)?;
        let p = self.planet(planet);
        let Some(owner) = p.owner else {
            return Err(format!("{} is not owned by any empire", p.display_name));
        };
        if ship.idx() >= self.defs.ships.len() {
            return Err(format!("no ship design with id {}", ship.0));
        }
        let d = self.defs.ship(ship);
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
        Ok(owner)
    }

    /// Current housing on a planet, from its enabled buildings (no housing exists without
    /// buildings).
    pub fn housing(&self, planet: PlanetId) -> f64 {
        let p = self.planet(planet);
        let owner = p.owner.map(|o| self.empire(o));
        (0..p.tiles.len())
            .filter(|t| p.tiles[*t].building.as_ref().is_some_and(|b| b.enabled))
            .filter_map(|t| crate::sim::planet::building_profile(&self.defs, p, t, owner))
            .map(|prof| prof.housing)
            .sum()
    }

    /// Checks one order for fleet `f`, carried out while the fleet is in system `at`.
    pub fn check_order(&self, f: &Fleet, at: SystemId, o: &FleetOrder) -> Result<(), String> {
        match o {
            FleetOrder::Move { coordinates: c } => {
                if !(c[0].is_finite() && c[1].is_finite()) {
                    return Err("coordinates must be finite".into());
                }
            }
            FleetOrder::Jump { system } => {
                if system.idx() >= self.systems.len() {
                    return Err("no such system".into());
                }
                if !self.system(at).links.contains(system) {
                    return Err(format!(
                        "no hyperlane from {} to {}",
                        self.system(at).display_name,
                        self.system(*system).display_name
                    ));
                }
            }
            FleetOrder::Bombard { planet, .. } => {
                self.check_planet(*planet)?;
                let p = self.planet(*planet);
                if p.system != at {
                    return Err(format!(
                        "{} is not in {}",
                        p.display_name,
                        self.system(at).display_name
                    ));
                }
                match p.owner {
                    Some(o) if o == f.owner => {
                        return Err("a fleet cannot bombard its own planet".into());
                    }
                    Some(o) if !self.at_war(f.owner, o) => {
                        return Err(format!(
                            "{} is not at war with {}",
                            self.empire(f.owner).display_name,
                            self.empire(o).display_name
                        ));
                    }
                    None if self.system(at).owner.is_some_and(|o| o != f.owner) => {
                        return Err(format!(
                            "{} is an unclaimed planet in another empire's system",
                            p.display_name
                        ));
                    }
                    _ => {}
                }
                if f.max_range(&self.empire(f.owner).ship_stats) <= 0.0 {
                    return Err(format!("{} has no weapons", f.display_name));
                }
            }
        }
        Ok(())
    }

    fn check_fleet(&self, fleet: FleetId) -> Result<&Fleet, String> {
        self.fleet(fleet)
            .ok_or_else(|| format!("no fleet with id {}", fleet.0))
    }

    /// Checks whether the owner of `planet` may install `upgrade` on `tile`.
    pub fn can_upgrade(
        &self,
        planet: PlanetId,
        tile: usize,
        upgrade: UpgradeId,
    ) -> Result<(), String> {
        self.check_tile(planet, tile)?;
        let p = self.planet(planet);
        if upgrade.idx() >= self.defs.upgrades.len() {
            return Err(format!("no upgrade with id {}", upgrade.0));
        }
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
            .after_upgrades
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
        Ok(())
    }

    pub fn can_terraform(&self, planet: PlanetId) -> Result<(), String> {
        self.check_planet(planet)?;
        let p = self.planet(planet);
        let Some(owner) = self.planet_owner(planet) else {
            return Err(format!("{} is not owned by any empire", p.display_name));
        };
        if p.habitable {
            return Err(format!("{} is already habitable", p.display_name));
        }
        if p.terraform.is_some() {
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
        Ok(())
    }

    /// Checks whether `empire` may start researching `tech`.
    pub fn can_research(&self, empire: EmpireId, tech: TechId) -> Result<(), String> {
        let e = self.empires.get(empire.idx()).ok_or("no such empire")?;
        if tech.idx() >= self.defs.technologies.len() {
            return Err(format!("no technology with id {}", tech.0));
        }
        let t = self.defs.tech(tech);
        if e.knows(tech) {
            return Err(format!("{} is already known", t.display_name));
        }
        if e.is_researching(tech) {
            return Err(format!("{} is already being researched", t.display_name));
        }
        // Prerequisites may still be in research: the project waits until they are known.
        if let Some(p) = t
            .prerequisites
            .iter()
            .find(|p| !e.knows(**p) && !e.is_researching(**p))
        {
            return Err(format!(
                "{} first needs {}",
                t.display_name,
                self.defs.tech(*p).display_name
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

pub fn describe_cost(world: &World, cost: &Cost) -> String {
    let mut parts: Vec<String> = cost
        .resources
        .iter()
        .map(|a| {
            format!(
                "{:.0} {}",
                a.count,
                world.defs.resource(a.resource).display_name
            )
        })
        .collect();
    if cost.days > 0 {
        parts.push(format!("{} days", cost.days));
    }
    parts.join(", ")
}

/// Starts a tile project, or applies it at once if it costs nothing.
fn start_project(world: &mut World, planet: PlanetId, tile: usize, c: Construction) {
    let p = world.planet_mut(planet);
    if c.remaining.is_done() {
        let t = &mut p.tiles[tile];
        match c.kind {
            ProjectKind::Building(b) => {
                t.building = Some(BuildingInstance::new(b));
                if let Some(e) = c.empire.filter(|e| p.owner != Some(*e)) {
                    p.claims.push(e);
                }
            }
            ProjectKind::Upgrade(u) => {
                if let Some(b) = &mut t.building {
                    b.add_upgrade(u);
                }
            }
            ProjectKind::Remove => t.building = None,
        }
    } else {
        p.tiles[tile].construction = Some(Box::new(c));
    }
    p.invalidate();
}

/// Replaces (or extends) a fleet's orders. A new queue starts the hyperdrives from scratch.
fn set_orders(world: &mut World, fleet: FleetId, orders: Vec<FleetOrder>, append: bool) {
    let f = world.fleet_mut(fleet).unwrap();
    if append {
        f.orders.extend(orders);
    } else {
        f.orders = orders;
        for s in &mut f.ships {
            s.jump_charge = 0.0;
        }
    }
}

pub(crate) fn apply(world: &mut World, cmd: Command) -> CmdResult {
    let day = world.day;
    match cmd {
        Command::MovePops { from, to, count } => {
            world.check_planet(from)?;
            world.check_planet(to)?;
            // Pops can only go where there is housing for them.
            let dest = world.planet(to);
            let free = (world.housing(to) - dest.pop() as f64).floor().max(0.0) as u64;
            if free == 0 {
                return Err(format!(
                    "{} has no free housing: build housing there first",
                    dest.display_name
                ));
            }
            let moved = world.planet_mut(from).remove_pops(count.min(free));
            let n: u64 = moved.iter().map(|(_, c)| c).sum();
            for (s, c) in moved {
                world.planet_mut(to).add_pops(s, c);
            }
            let msg = format!(
                "{n} pops relocated from {} to {}",
                world.planet(from).display_name,
                world.planet(to).display_name
            );
            Ok(Some(Event::new(day, EventKind::Command, msg).planet(to)))
        }
        Command::SetPopulation {
            planet,
            species,
            count,
        } => {
            world.check_planet(planet)?;
            if species.idx() >= world.defs.species.len() {
                return Err(format!("no species with id {}", species.0));
            }
            let p = world.planet_mut(planet);
            let now = p.species_count(species);
            if count > now {
                p.add_pops(species, count - now);
            } else {
                p.remove_species(species, now - count);
            }
            Ok(None)
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
            if building.idx() >= world.defs.buildings.len() {
                return Err(format!("no building with id {}", building.0));
            }
            let p = world.planet(planet);
            let owner = empire
                .or(p.owner)
                .or(world.system(p.system).owner)
                .ok_or("nobody owns this system: specify which empire builds")?;
            let plan = world.build_plan(owner, planet, tile, building)?;
            let name = world.defs.building(building).display_name.clone();
            let days = plan.cost.days;
            let empire = (world.planet(planet).owner != Some(owner)).then_some(owner);
            start_project(
                world,
                planet,
                tile,
                Construction {
                    paid: Vec::new(),
                    kind: ProjectKind::Building(building),
                    remaining: plan.cost,
                    empire,
                },
            );
            let msg = match plan.expansion_jumps {
                Some(_) => format!(
                    "{} stakes a claim on {}, building {name} ({days} days)",
                    world.empire(owner).display_name,
                    world.planet(planet).display_name,
                ),
                None => format!(
                    "{} begins construction of {name} on {}",
                    world.empire(owner).display_name,
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
            if let Some(c) = &t.construction {
                // Cancel the project and refund what it has paid so far.
                let payer = c.empire.or(world.planet(planet).owner);
                let paid = c.paid.clone();
                if let Some(payer) = payer {
                    let defs = world.defs.clone();
                    refund(
                        defs.ruleset(world.ruleset),
                        &paid,
                        &mut world.empires[payer.idx()].storage,
                    );
                }
                let p = world.planet_mut(planet);
                p.tiles[tile].construction = None;
                p.dirty = true;
                return Ok(None);
            }
            if t.building.is_none() {
                return Err("nothing to demolish".into());
            }
            let (payer, cost) = world.demolish_plan(planet, tile)?;
            let def = world.planet(planet).tiles[tile]
                .building
                .as_ref()
                .unwrap()
                .def;
            let d = world.defs.building(def);
            let (verb, name) = (
                if d.blocker { "clear" } else { "demolish" },
                d.display_name.clone(),
            );
            let empire = (world.planet(planet).owner != Some(payer)).then_some(payer);
            let instant = cost.is_none();
            start_project(
                world,
                planet,
                tile,
                Construction {
                    paid: Vec::new(),
                    kind: ProjectKind::Remove,
                    remaining: cost.unwrap_or_default(),
                    empire,
                },
            );
            let msg = if instant {
                format!(
                    "{} {verb}s the {name} on {}",
                    world.empire(payer).display_name,
                    world.planet(planet).display_name
                )
            } else {
                format!(
                    "{} begins to {verb} the {name} on {}",
                    world.empire(payer).display_name,
                    world.planet(planet).display_name
                )
            };
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
            world.can_upgrade(planet, tile, upgrade)?;
            let owner = world.planet_owner(planet).unwrap();
            let up = world.defs.upgrade(upgrade).clone();
            start_project(
                world,
                planet,
                tile,
                Construction {
                    paid: Vec::new(),
                    kind: ProjectKind::Upgrade(upgrade),
                    remaining: up.build_cost.clone(),
                    empire: None,
                },
            );
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
            world.can_terraform(planet)?;
            let owner = world.planet_owner(planet).unwrap();
            let mut cost = world
                .rules()
                .terraform(world.planet(planet).size)
                .unwrap()
                .cost
                .clone();
            cost.days = cost.days.max(1);
            world.planet_mut(planet).terraform = Some(Box::new(TerraformProject {
                paid: Vec::new(),
                remaining: cost,
            }));
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
        Command::CancelTerraform { planet } => {
            world.check_planet(planet)?;
            let p = world.planet(planet);
            let (Some(project), Some(owner)) = (p.terraform.clone(), p.owner) else {
                return Err(format!("{} is not being terraformed", p.display_name));
            };
            let defs = world.defs.clone();
            refund(
                defs.ruleset(world.ruleset),
                &project.paid,
                &mut world.empires[owner.idx()].storage,
            );
            world.planet_mut(planet).terraform = None;
            Ok(None)
        }
        Command::Research { empire, tech } => {
            world.can_research(empire, tech)?;
            let remaining = world.defs.research_cost(tech);
            world.empires[empire.idx()].research.push(ResearchProject {
                tech,
                paid: Vec::new(),
                remaining,
            });
            Ok(None)
        }
        Command::CancelResearch { empire, tech } => {
            let e = world.empires.get(empire.idx()).ok_or("no such empire")?;
            let i = e
                .research
                .iter()
                .position(|p| p.tech == tech)
                .ok_or("that technology is not being researched")?;
            // Cancelling a technology also cancels anything queued behind it.
            let defs = world.defs.clone();
            let e = &mut world.empires[empire.idx()];
            let mut cancelled = vec![e.research.remove(i)];
            loop {
                let blocked = e.research.iter().position(|p| {
                    defs.tech(p.tech)
                        .prerequisites
                        .iter()
                        .any(|q| !e.techs[q.idx()] && !e.is_researching(*q))
                });
                match blocked {
                    Some(j) => cancelled.push(e.research.remove(j)),
                    None => break,
                }
            }
            let rules = defs.ruleset(world.ruleset);
            for project in cancelled {
                refund(rules, &project.paid, &mut e.storage);
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
            if resource.idx() >= world.defs.resource_count() {
                return Err("no such resource".into());
            }
            if world.defs.is_transient(resource) {
                return Err(format!(
                    "{} is transient and cannot be stored",
                    world.defs.resource(resource).display_name
                ));
            }
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
        Command::BuildShip { planet, ship } => {
            let owner = world.can_build_ship(planet, ship)?;
            let remaining = world.defs.ship(ship).build_cost.clone();
            world.empires[owner.idx()].shipbuilding.push(ShipProject {
                ship,
                planet,
                paid: Vec::new(),
                remaining,
            });
            Ok(None)
        }
        Command::CancelShip { empire, index } => {
            let e = world.empires.get(empire.idx()).ok_or("no such empire")?;
            if index >= e.shipbuilding.len() {
                return Err("no such ship under construction".into());
            }
            let project = world.empires[empire.idx()].shipbuilding.remove(index);
            let defs = world.defs.clone();
            refund(
                defs.ruleset(world.ruleset),
                &project.paid,
                &mut world.empires[empire.idx()].storage,
            );
            Ok(None)
        }
        Command::SetOrders {
            fleet,
            orders,
            append,
        } => {
            let f = world.check_fleet(fleet)?;
            let mut at = if append { f.destination() } else { f.system };
            for o in &orders {
                world.check_order(f, at, o)?;
                if let FleetOrder::Jump { system } = o {
                    at = *system;
                }
            }
            set_orders(world, fleet, orders, append);
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
            let mut orders = world.route_orders(f.system, system).ok_or_else(|| {
                format!(
                    "no hyperlane route to {}",
                    world.system(system).display_name
                )
            })?;
            if let Some(c) = coordinates {
                if !(c[0].is_finite() && c[1].is_finite()) {
                    return Err("coordinates must be finite".into());
                }
                orders.push(FleetOrder::Move { coordinates: c });
            }
            set_orders(world, fleet, orders, false);
            Ok(None)
        }
        Command::Bombard {
            fleet,
            planet,
            tile,
        } => {
            world.check_planet(planet)?;
            let f = world.check_fleet(fleet)?;
            let target = world.planet(planet).system;
            let mut orders = world.route_orders(f.system, target).ok_or_else(|| {
                format!(
                    "no hyperlane route to {}",
                    world.planet(planet).display_name
                )
            })?;
            let order = FleetOrder::Bombard { planet, tile };
            world.check_order(f, target, &order)?;
            orders.push(order);
            set_orders(world, fleet, orders, false);
            Ok(None)
        }
        Command::StopFleet { fleet } => {
            world.check_fleet(fleet)?;
            set_orders(world, fleet, Vec::new(), false);
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
            let limit = world.empire(a.owner).max_fleet_size(world.rules());
            let (sa, sb) = (a.size(&world.defs), b.size(&world.defs));
            if sa + sb > limit {
                return Err(format!(
                    "a fleet has at most {limit} slots ({sa} + {sb} would be needed)"
                ));
            }
            let ships = std::mem::take(&mut world.fleet_mut(from).unwrap().ships);
            let f = world.fleet_mut(into).unwrap();
            f.ships.extend(ships);
            f.reform();
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
            src.reform();
            new.ships.reverse();
            new.reform();
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
        Command::DeclareWar {
            empire,
            target,
            goal,
        } => war::declare_war(world, empire, target, goal),
        Command::SetWarGoal { empire, war, goal } => war::set_war_goal(world, empire, war, goal),
        Command::ProposePeace {
            empire,
            war,
            outcome,
        } => war::propose_peace(world, empire, war, outcome),
        Command::AcceptPeace { empire, war } => war::accept_peace(world, empire, war),
        Command::EnforcePeace {
            empire,
            war,
            outcome,
        } => war::enforce_peace(world, empire, war, outcome),
        Command::Claim { empire, system } => war::claim(world, empire, system),
        Command::DropClaim { empire, system } => war::drop_claim(world, empire, system),
        Command::CreateAlliance {
            empire,
            display_name,
        } => war::create_alliance(world, empire, display_name),
        Command::JoinAlliance { empire, alliance } => war::join_alliance(world, empire, alliance),
        Command::LeaveAlliance { empire, alliance } => war::leave_alliance(world, empire, alliance),
        Command::SetPolicy {
            empire,
            accepts_migrants,
            market_access,
            total_war,
        } => {
            let e = world
                .empires
                .get_mut(empire.idx())
                .ok_or("no such empire")?;
            if let Some(v) = accepts_migrants {
                e.accepts_migrants = v;
            }
            if let Some(v) = market_access {
                e.market_access = v;
            }
            if let Some(v) = total_war {
                e.total_war = v;
            }
            Ok(None)
        }
    }
}
