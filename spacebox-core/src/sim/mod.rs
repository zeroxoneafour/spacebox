//! The tick loop.
//!
//! One tick is one day:
//!
//! 0. Queued commands (player orders, last tick's governor decisions) are applied serially.
//! 1. **Space** (parallel per system): fleets move, charge hyperdrives, fight hostile fleets
//!    in sub-ticks, and bombard planets. Bombardment results (building damage, surrenders) are
//!    applied serially afterwards. This runs before any economy, as the design requires.
//! 2. **Planets** (parallel): construction, job assignment, production/consumption, stability,
//!    growth, and migration offers.
//! 3. **Systems** (parallel): sum planet flows per owner, match migrants within the system.
//! 4. **Empires** (parallel): combine systems into economic pools (with logistics losses,
//!    fleet upkeep and empire income), match migrants within the empire.
//! 5. **Galaxy** (serial, cheap): galactic market and inter-empire migration.
//! 6. **Settlement** (parallel): surpluses go to storage, deficits drain it, empty stores start
//!    crises; shortage ratios are published for the next tick. Then (serially) projects draw
//!    from storage and transient production, research completes, migrations are committed,
//!    finished ships launched and completed expansion sites claim their planets and systems.
//!    Wars then update (exhaustion, humiliation, wars left without one side end).
//! 7. **Governors** (parallel per AI empire) look at the fresh state and queue commands.

pub mod ai;
pub mod commands;
pub mod economy;
pub mod events;
pub mod migration;
pub mod planet;
pub mod space;
pub mod war;

use rayon::prelude::*;

use crate::ids::*;
use crate::sim::events::{Event, EventKind};
use crate::world::{Fleet, Pool, Ship, SystemGroup, World};

impl World {
    /// Advances the simulation by one day.
    pub fn tick(&mut self) {
        let day = self.day;
        if self.topology_dirty {
            self.refresh_topology();
        }

        // 0. Commands queued since the last tick (including last tick's governor decisions).
        for cmd in std::mem::take(&mut self.queue) {
            match commands::apply(self, cmd.clone()) {
                Ok(ev) => self.events.extend(ev),
                Err(msg) => self.events.push(Event::new(
                    day,
                    EventKind::CommandRejected,
                    format!("command rejected: {msg} ({cmd:?})"),
                )),
            }
        }
        if self.topology_dirty {
            self.refresh_topology();
        }

        // 1. Space.
        self.compute_fleet_upkeep();
        space::tick(self);
        if self.topology_dirty {
            self.refresh_topology();
        }
        let seed = self.seed;

        let World {
            defs,
            ruleset,
            empires,
            systems,
            planets,
            pools,
            ..
        } = self;
        let rules = defs.ruleset(*ruleset);

        // 2. Planets.
        planets.par_iter_mut().enumerate().for_each(|(i, p)| {
            let pool = &pools[p.pool.idx()];
            let owner = p.controller().map(|o| (o, &empires[o.idx()]));
            let ctx = planet::PlanetCtx {
                defs,
                rules,
                sat: &pool.sat,
                scarcity: &pool.scarcity,
                owner,
                day,
                seed,
            };
            planet::tick(p, PlanetId::from_idx(i), &ctx);
        });

        let busy: Vec<u32> = planets
            .par_iter()
            .enumerate()
            .filter(|(_, p)| {
                p.constructing
                    || p.terraform.is_some()
                    || !p.events.is_empty()
                    || !p.claims.is_empty()
            })
            .map(|(i, _)| i as u32)
            .collect();

        // 3. Systems.
        systems
            .par_iter_mut()
            .for_each(|s| economy::tick_system(s, &planets[s.planet_range()]));

        // 4. Empires.
        let World {
            defs,
            ruleset,
            empires,
            systems,
            pools,
            empire_presence,
            ..
        } = self;
        let rules = defs.ruleset(*ruleset);
        pools
            .par_iter_mut()
            .for_each(|p| economy::aggregate_pool(p, systems, empires, rules));
        empires
            .par_iter_mut()
            .zip(empire_presence.par_iter())
            .enumerate()
            .for_each(|(i, (e, present))| {
                migration::empire_level(e, EmpireId::from_idx(i), present, systems)
            });

        // 5. Galaxy.
        migration::galaxy_level(self);
        economy::market(self);

        // 6. Settlement.
        let World {
            defs,
            ruleset,
            empires,
            systems,
            planets,
            pools,
            empire_planets,
            ..
        } = self;
        let rules = defs.ruleset(*ruleset);
        empires
            .par_iter_mut()
            .zip(empire_planets.par_iter())
            .enumerate()
            .for_each(|(i, (e, owned))| {
                economy::finalize_empire(
                    e,
                    EmpireId::from_idx(i),
                    pools,
                    systems,
                    planets,
                    owned,
                    defs,
                    rules,
                    day,
                )
            });
        pools
            .par_iter_mut()
            .for_each(|p| economy::finalize_pool(p, empires));
        self.busy = busy;
        economy::fund_projects(self);
        migration::commit(self);
        self.launch_ships();
        self.apply_claims();
        war::tick(self);

        for &i in &self.busy {
            self.events.append(&mut self.planets[i as usize].events);
        }
        for e in &mut self.empires {
            self.events.append(&mut e.events);
        }

        // 7. Governor decisions, based on this tick's fresh stats; applied next tick.
        if self.topology_dirty {
            self.refresh_topology();
        }
        let interval = self.rules().ai_interval_days.max(1) as u64;
        let this = &*self;
        let decided: Vec<_> = (0..self.empires.len())
            .into_par_iter()
            .filter(|&i| this.empires[i].ai.is_some() && (day + i as u64).is_multiple_of(interval))
            .flat_map_iter(|i| ai::decide(this, EmpireId::from_idx(i)))
            .collect();
        self.queue.extend(decided);
        self.day += 1;
    }

    /// Runs `days` ticks.
    pub fn run(&mut self, days: u64) {
        for _ in 0..days {
            self.tick();
        }
    }

    fn compute_fleet_upkeep(&mut self) {
        for e in &mut self.empires {
            e.fleet_upkeep.fill(0.0);
        }
        for f in &self.fleets {
            let up = &mut self.empires[f.owner.idx()].fleet_upkeep;
            for s in &f.ships {
                for a in &self.defs.ship(s.def).upkeep {
                    up[a.resource.idx()] += a.count;
                }
            }
        }
    }

    /// Turns finished ships into fleets: every new ship starts a fleet of its own in orbit of
    /// the planet that built it.
    fn launch_ships(&mut self) {
        let day = self.day;
        for ei in 0..self.empires.len() {
            let eid = EmpireId::from_idx(ei);
            for project in std::mem::take(&mut self.empires[ei].completed_ships) {
                if project.planet.idx() >= self.planets.len() {
                    continue;
                }
                let p = &self.planets[project.planet.idx()];
                let (sys, coords, pname) = (p.system, p.coordinates, p.display_name.clone());
                let ship = Ship::new(project.ship, self.ship_stats(eid, project.ship));
                // The next fleet id keeps names unique even after merges and losses.
                let n = self.next_fleet_id;
                let mut fl = Fleet::new(
                    String::new(),
                    format!("{} Fleet {n}", self.systems[sys.idx()].display_name),
                    eid,
                    sys,
                );
                fl.coordinates = coords;
                fl.ships.push(ship);
                let fname = fl.display_name.clone();
                self.add_fleet(fl);
                self.events.push(
                    Event::new(
                        day,
                        EventKind::ShipLaunched,
                        format!(
                            "{} launches a {} at {pname} ({fname})",
                            self.empires[ei].display_name,
                            self.defs.ship(project.ship).display_name
                        ),
                    )
                    .empire(Some(eid))
                    .planet(project.planet),
                );
            }
        }
    }

    /// Applies construction sites that finished on planets their builder did not own. The
    /// builder claims the planet; if the system was unowned it claims the system too (its
    /// other planets stay unowned, but no other empire can claim them), and every rival site
    /// in the system is cancelled and refunded.
    fn apply_claims(&mut self) {
        let day = self.day;
        let mut claims = Vec::new();
        for &i in &self.busy {
            for e in self.planets[i as usize].claims.drain(..) {
                claims.push((PlanetId(i), e));
            }
        }
        for (pid, e) in claims {
            let sys = self.planets[pid.idx()].system;
            let owner = self.systems[sys.idx()].owner;
            if owner.is_some() && owner != Some(e) {
                continue; // a rival finished first; this site's building stays unclaimed
            }
            if self.planets[pid.idx()].owner.is_some_and(|o| o != e) {
                continue;
            }
            let claiming_system = owner.is_none();
            self.planets[pid.idx()].owner = Some(e);
            self.planets[pid.idx()].dirty = true;
            if claiming_system {
                self.systems[sys.idx()].owner = Some(e);
                let ids: Vec<PlanetId> = self.systems[sys.idx()].planet_ids().collect();
                for p in ids {
                    let planet = &mut self.planets[p.idx()];
                    for t in &mut planet.tiles {
                        let rival = match &t.construction {
                            Some(c) if c.empire.is_some_and(|r| r != e) => c.empire.unwrap(),
                            _ => continue,
                        };
                        let c = t.construction.take().unwrap();
                        commands::refund(
                            self.defs.ruleset(self.ruleset),
                            &c.paid,
                            &mut self.empires[rival.idx()].storage,
                        );
                        planet.dirty = true;
                        self.events.push(
                            Event::new(
                                day,
                                EventKind::ClaimLost,
                                format!(
                                    "{}'s construction on {} is cancelled and refunded: {} has claimed {}",
                                    self.empires[rival.idx()].display_name,
                                    planet.display_name,
                                    self.empires[e.idx()].display_name,
                                    self.systems[sys.idx()].display_name
                                ),
                            )
                            .empire(Some(rival))
                            .planet(p),
                        );
                    }
                }
                self.events.push(
                    Event::new(
                        day,
                        EventKind::SystemClaimed,
                        format!(
                            "{} claims the {} system",
                            self.empires[e.idx()].display_name,
                            self.systems[sys.idx()].display_name
                        ),
                    )
                    .empire(Some(e))
                    .planet(pid),
                );
            }
            self.topology_dirty = true;
        }
    }

    /// Recomputes ownership lists, capitals, ownership groups and economic pools. Called
    /// automatically when ownership or hyperlanes change.
    pub fn refresh_topology(&mut self) {
        let n_res = self.defs.resource_count();
        let n_sys = self.systems.len();
        let n_emp = self.empires.len();
        // Pools that survive (same owner and hub) are carried over with their buffers and
        // shortage ratios, so a refresh in a busy galaxy doesn't reallocate everything.
        let mut old_pools: Vec<Option<Pool>> = std::mem::take(&mut self.pools)
            .into_iter()
            .map(Some)
            .collect();
        let old_index: std::collections::HashMap<(Option<EmpireId>, SystemId), usize> = old_pools
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.as_ref().and_then(|p| p.hub.map(|h| ((p.empire, h), i))))
            .collect();
        let mut reuse = |empire: Option<EmpireId>, hub: SystemId| -> (Pool, bool) {
            match old_index
                .get(&(empire, hub))
                .and_then(|&i| old_pools[i].take())
            {
                Some(mut p) => {
                    p.systems.clear();
                    p.members.clear();
                    p.planets.clear();
                    p.core = false;
                    (p, true)
                }
                None => (Pool::new(empire, n_res), false),
            }
        };
        let mut carried: Vec<bool> = Vec::new();

        let valid = |o: Option<EmpireId>| o.filter(|o| o.idx() < n_emp);
        let mut owned = vec![Vec::new(); n_emp];
        let mut owned_planets = vec![Vec::new(); n_emp];
        let mut presence = vec![Vec::new(); n_emp];
        // Borders follow ownership; economies follow control (occupiers run what they hold).
        for (i, s) in self.systems.iter().enumerate() {
            if let Some(o) = valid(s.owner) {
                owned[o.idx()].push(SystemId::from_idx(i));
            }
            if let Some(o) = valid(s.occupier.or(s.owner)) {
                presence[o.idx()].push(SystemId::from_idx(i));
            }
        }
        for (i, p) in self.planets.iter().enumerate() {
            if let Some(o) = valid(p.controller()) {
                owned_planets[o.idx()].push(PlanetId::from_idx(i));
                presence[o.idx()].push(p.system);
            }
        }
        for v in &mut presence {
            v.sort_unstable();
            v.dedup();
        }

        // Ownership groups within each system, in order of first appearance.
        let mut owners: Vec<Option<EmpireId>> = Vec::new();
        for s in &mut self.systems {
            owners.clear();
            for p in &mut self.planets[s.planet_start as usize..s.planet_end as usize] {
                p.owner = valid(p.owner);
                p.occupier = valid(p.occupier);
                let owner = p.controller();
                p.group = match owners.iter().position(|o| *o == owner) {
                    Some(g) => g as u8,
                    None => {
                        owners.push(owner);
                        (owners.len() - 1) as u8
                    }
                };
            }
            // Keep the existing groups (and their buffers) when the owners are unchanged.
            if !s.groups.iter().map(|g| g.owner).eq(owners.iter().copied()) {
                s.quiet = false;
                s.groups = owners
                    .iter()
                    .map(|o| SystemGroup::new(*o, PoolId(0), n_res))
                    .collect();
            }
        }

        let mut pools: Vec<Pool> = Vec::new();
        let mut mark = vec![u32::MAX; n_sys];
        let mut seen = vec![u32::MAX; n_sys];
        let day = self.day;
        for (ei, present) in presence.iter().enumerate() {
            let eid = EmpireId::from_idx(ei);
            for s in present {
                mark[s.idx()] = ei as u32;
            }
            let e = &mut self.empires[ei];
            e.core_pool = None;
            if e.capital
                .is_some_and(|c| c.idx() >= n_sys || mark[c.idx()] != ei as u32)
            {
                e.capital = None;
            }
            if e.capital.is_none() && !present.is_empty() {
                let pop = |s: &SystemId| -> u64 {
                    self.planets[self.systems[s.idx()].planet_range()]
                        .iter()
                        .filter(|p| p.owner == Some(eid))
                        .map(|p| p.pop())
                        .sum()
                };
                let best = *present
                    .iter()
                    .max_by(|a, b| pop(a).cmp(&pop(b)).then(b.cmp(a)))
                    .unwrap();
                e.capital = Some(best);
                self.events.push(
                    Event::new(
                        day,
                        EventKind::CapitalChanged,
                        format!(
                            "{} establishes its capital at {}",
                            e.display_name,
                            self.systems[best.idx()].display_name
                        ),
                    )
                    .empire(Some(eid)),
                );
            }
            let starts: Vec<SystemId> = e
                .capital
                .into_iter()
                .chain(present.iter().copied())
                .collect();
            for start in starts {
                if seen[start.idx()] == ei as u32 {
                    continue;
                }
                let id = PoolId::from_idx(pools.len());
                let (mut pool, kept) = reuse(Some(eid), start);
                carried.push(kept);
                pool.core = Some(start) == e.capital;
                pool.hub = Some(start);
                if pool.core {
                    e.core_pool = Some(id);
                }
                let mut stack = vec![start];
                seen[start.idx()] = ei as u32;
                while let Some(s) = stack.pop() {
                    pool.systems.push(s);
                    for &n in &self.systems[s.idx()].links {
                        if seen[n.idx()] != ei as u32 && mark[n.idx()] == ei as u32 {
                            seen[n.idx()] = ei as u32;
                            stack.push(n);
                        }
                    }
                }
                pool.systems.sort_unstable();
                for &s in &pool.systems {
                    let sys = &mut self.systems[s.idx()];
                    if let Some(g) = sys.groups.iter().position(|g| g.owner == Some(eid)) {
                        sys.groups[g].pool = id;
                        pool.members.push((s, g as u8));
                    }
                }
                pools.push(pool);
            }
        }
        for (si, sys) in self.systems.iter_mut().enumerate() {
            if let Some(g) = sys.groups.iter().position(|g| g.owner.is_none()) {
                let id = PoolId::from_idx(pools.len());
                let (mut pool, kept) = reuse(None, SystemId::from_idx(si));
                carried.push(kept);
                pool.hub = Some(SystemId::from_idx(si));
                pool.systems.push(SystemId::from_idx(si));
                pool.members.push((SystemId::from_idx(si), g as u8));
                sys.groups[g].pool = id;
                pools.push(pool);
            }
        }
        // Planets join their group's pool. New pools inherit shortage ratios from the pool
        // their first planet was in before.
        let mut inherited = carried;
        for (i, p) in self.planets.iter_mut().enumerate() {
            let old = p.pool;
            let sys = &self.systems[p.system.idx()];
            p.pool = sys.groups[p.group as usize].pool;
            let pool = &mut pools[p.pool.idx()];
            pool.planets.push(PlanetId::from_idx(i));
            if !inherited[p.pool.idx()] {
                inherited[p.pool.idx()] = true;
                if let Some(Some(o)) = old_pools.get(old.idx()) {
                    pool.sat.copy_from_slice(&o.sat);
                    pool.scarcity.copy_from_slice(&o.scarcity);
                }
            }
        }
        self.pools = pools;
        self.empire_systems = owned;
        self.empire_planets = owned_planets;
        self.empire_presence = presence;
        self.topology_dirty = false;
    }
}
