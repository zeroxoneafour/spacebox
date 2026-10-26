//! Wars, alliances, truces and claims (see `design/data/savegame.toml`).
//!
//! An empire declares a war with a goal, paying its influence cost; its allies and the target's
//! join the two sides, except allies of both, who stay out. The defense sets its own goal for
//! free. While the war goes on, captured planets are occupied (they produce for the occupier),
//! or in a total war taken for good.
//!
//! Each side's exhaustion (0..1) rises every day of the war, with ships and planets lost and
//! with resource crises (rates in `[ruleset.x.war]`). A war ends when the two sides agree on an
//! outcome (one proposes, the other accepts), when one side forces the outcome of its choice on
//! a side whose exhaustion has reached 1, or on its own once a side has no planets left.
//!
//! When it ends:
//! * the losers of a humiliation war get no influence income for `humiliation-days`;
//! * in a claim war, systems the winners claim and occupy become theirs, and the losers drop
//!   their claims on the winners;
//! * every other occupation is lifted (in a status quo, all of them); what changed hands in a
//!   total war stays changed;
//! * all participants enter a truce for `truce-days`.

use crate::ids::*;
use crate::sim::events::{Event, EventKind};
use crate::world::{Alliance, Side, Truce, War, WarGoal, WarOutcome, World};

/// Daily bookkeeping: humiliation runs out, exhaustion rises and wars without one side end.
pub(crate) fn tick(world: &mut World) {
    for e in &mut world.empires {
        e.humiliated = e.humiliated.saturating_sub(1);
    }
    let n = world.empires.len();
    world.ship_losses.resize(n, 0.0);
    let ship_losses = std::mem::take(&mut world.ship_losses);
    let planet_losses = std::mem::take(&mut world.planet_losses);
    world.ship_losses = vec![0.0; n];
    if !world.wars.iter().any(War::is_active) {
        return;
    }

    let defs = world.defs.clone();
    let rules = defs.ruleset(world.ruleset);
    // Fleet slots each empire still has.
    let mut slots = vec![0.0; n];
    for f in &world.fleets {
        slots[f.owner.idx()] += f.size(&defs) as f64;
    }
    let mut lost_planets = vec![0.0; n];
    for (e, populated) in planet_losses {
        lost_planets[e.idx()] += if populated {
            rules.populated_planet_loss_exhaustion
        } else {
            rules.planet_loss_exhaustion
        };
    }
    // Empires that still own a planet: a system stays its owner's while it holds any planet
    // there, so these are the empires with systems.
    let alive: Vec<bool> = (0..n)
        .map(|e| !world.empire_systems(EmpireId::from_idx(e)).is_empty())
        .collect();

    for w in 0..world.wars.len() {
        if !world.wars[w].is_active() {
            continue;
        }
        let id = WarId::from_idx(w);
        let sides = [
            world.members(id, Side::Offense),
            world.members(id, Side::Defense),
        ];
        let standing = sides.clone().map(|m| m.iter().any(|e| alive[e.idx()]));
        match standing {
            [true, true] => {}
            [true, false] => {
                end_war(world, id, WarOutcome::Offensive);
                continue;
            }
            [false, true] => {
                end_war(world, id, WarOutcome::Defensive);
                continue;
            }
            [false, false] => {
                end_war(world, id, WarOutcome::StatusQuo);
                continue;
            }
        }
        // A claim goal whose claims are gone (say, taken by an ally) becomes humiliation.
        for side in [Side::Offense, Side::Defense] {
            let (mine, theirs) = (&sides[side.idx()], &sides[side.other().idx()]);
            if world.wars[w].goals[side.idx()] == Some(WarGoal::Claim)
                && !mine.iter().any(|m| world.claims_on(*m, theirs))
            {
                world.wars[w].goals[side.idx()] = Some(WarGoal::Humiliate);
                world.events.push(
                    Event::new(
                        world.day,
                        EventKind::Command,
                        format!(
                            "{}: with no claims left to fight for, the {} now seeks humiliation",
                            world.wars[w].display_name,
                            side.as_str()
                        ),
                    )
                    .empire(mine.first().copied()),
                );
            }
        }
        for side in [Side::Offense, Side::Defense] {
            let members = &sides[side.idx()];
            let lost: f64 = members.iter().map(|e| ship_losses[e.idx()]).sum();
            let have: f64 = members.iter().map(|e| slots[e.idx()]).sum();
            let ships = if lost > 0.0 {
                rules.ship_loss_exhaustion * lost / (lost + have)
            } else {
                0.0
            };
            let planets: f64 = members.iter().map(|e| lost_planets[e.idx()]).sum();
            let crises = members
                .iter()
                .map(|e| {
                    world.empires[e.idx()]
                        .crises
                        .iter()
                        .map(|c| c.severity)
                        .sum::<f64>()
                })
                .sum::<f64>()
                / members.len().max(1) as f64;
            let x = &mut world.wars[w].exhaustion[side.idx()];
            *x = (*x + rules.daily_exhaustion + ships + planets + rules.crisis_exhaustion * crises)
                .clamp(0.0, 1.0);
        }
    }
}

/// Ends a war with `outcome` and applies its terms.
pub(crate) fn end_war(world: &mut World, war: WarId, outcome: WarOutcome) {
    let day = world.day;
    let rules = world.rules().clone();
    let sides = [
        world.members(war, Side::Offense),
        world.members(war, Side::Defense),
    ];
    let side_of = |e: EmpireId| {
        [Side::Offense, Side::Defense]
            .into_iter()
            .find(|s| sides[s.idx()].contains(&e))
    };
    let goals = world.wars[war.idx()].goals;
    let winner = outcome.winner();

    // Occupations: claimed systems the winners of a claim war occupy change hands; everything
    // else (all of it in a status quo) goes back to its owner.
    let keeps = |s: Side| winner == Some(s) && goals[s.idx()] == Some(WarGoal::Claim);
    for sys in 0..world.systems.len() {
        let sid = SystemId::from_idx(sys);
        let s = &world.systems[sys];
        let (Some(owner), Some(occ)) = (s.owner, s.occupier) else {
            continue;
        };
        let (Some(so), Some(sc)) = (side_of(owner), side_of(occ)) else {
            continue;
        };
        if so == sc || !keeps(sc) || !world.empires[occ.idx()].claims.contains(&sid) {
            continue;
        }
        let planets: Vec<PlanetId> = s
            .planet_ids()
            .filter(|p| {
                let p = &world.planets[p.idx()];
                p.owner == Some(owner) && p.occupier == Some(occ)
            })
            .collect();
        for p in planets {
            world.set_planet_owner(p, Some(occ));
        }
        world.events.push(
            Event::new(
                day,
                EventKind::SystemClaimed,
                format!(
                    "{} cedes {} to {}",
                    world.empires[owner.idx()].display_name,
                    world.systems[sys].display_name,
                    world.empires[occ.idx()].display_name
                ),
            )
            .empire(Some(occ)),
        );
    }
    for p in 0..world.planets.len() {
        let pl = &world.planets[p];
        if let (Some(o), Some(occ)) = (pl.owner, pl.occupier)
            && side_of(o).is_some()
            && side_of(occ).is_some()
        {
            world.set_planet_occupier(PlanetId::from_idx(p), None);
        }
    }

    // Humiliation, and losers giving up their claims on the winners.
    if let Some(w) = winner {
        let losers = &sides[w.other().idx()];
        if goals[w.idx()] == Some(WarGoal::Humiliate) {
            for e in losers {
                let e = &mut world.empires[e.idx()];
                e.humiliated = e.humiliated.max(rules.humiliation_days);
            }
        }
        for l in losers {
            let systems = &world.systems;
            let winners = &sides[w.idx()];
            world.empires[l.idx()]
                .claims
                .retain(|c| systems[c.idx()].owner.is_none_or(|o| !winners.contains(&o)));
        }
    }
    // Claims on systems that are now one's own are fulfilled.
    for e in sides.iter().flatten() {
        let systems = &world.systems;
        world.empires[e.idx()]
            .claims
            .retain(|c| systems[c.idx()].owner != Some(*e));
    }

    // The truce.
    let w = &mut world.wars[war.idx()];
    w.victory = Some(outcome);
    w.proposals = [None, None];
    let war_name = w.name.clone();
    let war_display = w.display_name.clone();
    let truce = TruceId::from_idx(world.truces.len());
    world.truces.push(Truce {
        name: unique_name(
            world.truces.iter().map(|t| t.name.as_str()),
            &format!("truce-{war_name}"),
        ),
        display_name: format!("Truce of {war_display}"),
        valid_until: day + rules.truce_days,
    });
    for e in sides.iter().flatten() {
        world.empires[e.idx()].truces.push(truce);
    }
    world.topology_dirty = true;

    // Peace stands down queued bombardment of former enemies.
    let targetable: Vec<Vec<bool>> = world
        .fleets
        .iter()
        .map(|f| {
            f.orders
                .iter()
                .map(|o| match o {
                    crate::world::FleetOrder::Bombard { planet, .. } => world
                        .planets
                        .get(planet.idx())
                        .and_then(|p| p.controller())
                        .is_none_or(|c| c != f.owner && world.at_war(f.owner, c)),
                    _ => true,
                })
                .collect()
        })
        .collect();
    for (f, keep) in world.fleets.iter_mut().zip(targetable) {
        let mut keep = keep.into_iter();
        f.orders.retain(|_| keep.next().unwrap());
    }

    let how = match outcome {
        WarOutcome::Offensive => "in victory for the offense",
        WarOutcome::Defensive => "in victory for the defense",
        WarOutcome::StatusQuo => "in a status quo",
    };
    world.events.push(
        Event::new(
            day,
            EventKind::PeaceMade,
            format!("{war_display} ends {how}"),
        )
        .empire(sides[0].first().copied()),
    );
}

/// `base`, or `base-2`, `base-3`... whichever is not taken.
fn unique_name<'a>(taken: impl Iterator<Item = &'a str> + Clone, base: &str) -> String {
    let free = |n: &str| !taken.clone().any(|t| t == n);
    if free(base) {
        return base.to_string();
    }
    (2..)
        .map(|k| format!("{base}-{k}"))
        .find(|n| free(n))
        .unwrap()
}

impl World {
    fn check_empire(&self, e: EmpireId) -> Result<(), String> {
        if e.idx() < self.empires.len() {
            Ok(())
        } else {
            Err("no such empire".into())
        }
    }

    fn check_war(&self, war: WarId) -> Result<&War, String> {
        let w = self.wars.get(war.idx()).ok_or("no such war")?;
        if !w.is_active() {
            return Err(format!("{} is over", w.display_name));
        }
        Ok(w)
    }

    fn influence(&self, empire: EmpireId) -> f64 {
        self.rules().expansion_resource.map_or(f64::INFINITY, |r| {
            self.empires[empire.idx()].storage[r.idx()]
        })
    }

    fn pay_influence(&mut self, empire: EmpireId, amount: f64) {
        if let Some(r) = self.rules().expansion_resource {
            self.empires[empire.idx()].storage[r.idx()] -= amount;
        }
    }

    /// Checks that each side's goal in an ongoing war meets its prerequisites: a claim goal
    /// needs a member with claims on the other side, a total goal a member with the
    /// `total-war` policy (or, for the defense, a total war declared on it).
    pub fn check_war_goals(&self, war: WarId) -> Result<(), String> {
        let w = &self.wars[war.idx()];
        if !w.is_active() {
            return Ok(());
        }
        let sides = [
            self.members(war, Side::Offense),
            self.members(war, Side::Defense),
        ];
        for side in [Side::Offense, Side::Defense] {
            let (mine, theirs) = (&sides[side.idx()], &sides[side.other().idx()]);
            if mine.is_empty() || theirs.is_empty() {
                continue;
            }
            match w.goals[side.idx()] {
                Some(WarGoal::Claim) if !mine.iter().any(|m| self.claims_on(*m, theirs)) => {
                    return Err(format!(
                        "{}: the {} fights for claims without holding any on the {}",
                        w.display_name,
                        side.as_str(),
                        side.other().as_str()
                    ));
                }
                Some(WarGoal::Total)
                    if !(side == Side::Defense
                        && w.goals[Side::Offense.idx()] == Some(WarGoal::Total))
                        && !mine.iter().any(|m| self.empires[m.idx()].total_war) =>
                {
                    return Err(format!(
                        "{}: the {} fights a total war without the total-war policy",
                        w.display_name,
                        side.as_str()
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Checks `empire`'s ongoing wars after a change of its policy or claims.
    pub(crate) fn check_wars_of(&self, empire: EmpireId) -> Result<(), String> {
        self.active_wars(empire)
            .try_for_each(|(w, _)| self.check_war_goals(w))
    }

    /// Whether `empire` claims a system owned by one of `enemies`.
    pub(crate) fn claims_on(&self, empire: EmpireId, enemies: &[EmpireId]) -> bool {
        self.empires[empire.idx()].claims.iter().any(|c| {
            self.systems[c.idx()]
                .owner
                .is_some_and(|o| enemies.contains(&o))
        })
    }

    /// Who would fight on each side if `empire` attacked `target`: each of them with their
    /// allies, except allies of someone on the other side or with a truce with someone there.
    pub fn war_sides(&self, empire: EmpireId, target: EmpireId) -> [Vec<EmpireId>; 2] {
        let n = self.empires.len();
        let allies = |e: EmpireId| {
            (0..n)
                .map(EmpireId::from_idx)
                .filter(move |o| self.allied(e, *o))
        };
        let mut off: Vec<EmpireId> = std::iter::once(empire).chain(allies(empire)).collect();
        let mut def: Vec<EmpireId> = std::iter::once(target).chain(allies(target)).collect();
        // Allies of both sides stay out; then so does anyone allied to, or in a truce with,
        // someone on the other side.
        let both: Vec<EmpireId> = off.iter().copied().filter(|e| def.contains(e)).collect();
        off.retain(|e| !both.contains(e));
        def.retain(|e| !both.contains(e));
        loop {
            let before = off.len() + def.len();
            let (o2, d2) = (off.clone(), def.clone());
            let bound =
                |e: EmpireId, o: EmpireId| e == o || self.allied(e, o) || self.in_truce(e, o);
            off.retain(|e| *e == empire || !d2.iter().any(|d| bound(*e, *d)));
            def.retain(|e| *e == target || !o2.iter().any(|o| bound(*e, *o)));
            if off.len() + def.len() == before {
                break;
            }
        }
        [off, def]
    }

    /// Checks whether `empire` may declare war on `target` with `goal`, and what it costs.
    pub fn can_declare_war(
        &self,
        empire: EmpireId,
        target: EmpireId,
        goal: WarGoal,
    ) -> Result<f64, String> {
        self.check_empire(empire)?;
        self.check_empire(target)?;
        if empire == target {
            return Err("an empire cannot declare war on itself".into());
        }
        let (a, b) = (
            &self.empires[empire.idx()],
            &self.empires[target.idx()].display_name,
        );
        if self.at_war(empire, target) {
            return Err(format!("{} is already at war with {b}", a.display_name));
        }
        if self.allied(empire, target) {
            return Err(format!("{} is allied with {b}", a.display_name));
        }
        if self.in_truce(empire, target) {
            return Err(format!("{} has a truce with {b}", a.display_name));
        }
        let [_, def] = self.war_sides(empire, target);
        match goal {
            WarGoal::Humiliate => {}
            WarGoal::Claim if !self.claims_on(empire, &def) => {
                return Err(format!("{} has no claims on {b}'s side", a.display_name));
            }
            WarGoal::Claim => {}
            WarGoal::Total if !a.total_war => {
                return Err(format!(
                    "{}'s policy does not allow total war",
                    a.display_name
                ));
            }
            WarGoal::Total => {}
        }
        let cost = self.rules().war_cost(goal);
        if self.influence(empire) + 1e-9 < cost {
            return Err(format!(
                "declaring a {} war costs {cost:.0} influence",
                goal.as_str()
            ));
        }
        Ok(cost)
    }

    /// Checks whether `empire`, on the defense, may set `goal` in `war`.
    pub fn can_set_war_goal(
        &self,
        empire: EmpireId,
        war: WarId,
        goal: WarGoal,
    ) -> Result<(), String> {
        self.check_empire(empire)?;
        let w = self.check_war(war)?;
        if self.side_of(war, empire) != Some(Side::Defense) {
            return Err("only the defense sets its war goal".into());
        }
        match goal {
            WarGoal::Humiliate => Ok(()),
            WarGoal::Claim if self.claims_on(empire, &self.members(war, Side::Offense)) => Ok(()),
            WarGoal::Claim => Err("no claims on the offense".into()),
            WarGoal::Total
                if w.goals[Side::Offense.idx()] == Some(WarGoal::Total)
                    || self.empires[empire.idx()].total_war =>
            {
                Ok(())
            }
            WarGoal::Total => {
                Err("total war needs the total-war policy or a total war declared on you".into())
            }
        }
    }

    /// Influence to claim `system`, if `empire` may: the cost of expanding there.
    pub fn claim_cost(&self, empire: EmpireId, system: SystemId) -> Result<f64, String> {
        self.check_empire(empire)?;
        let s = self.systems.get(system.idx()).ok_or("no such system")?;
        match s.owner {
            None => return Err(format!("{} is not owned by anyone", s.display_name)),
            Some(o) if o == empire => return Err(format!("{} is already yours", s.display_name)),
            Some(_) => {}
        }
        if self.empires[empire.idx()].claims.contains(&system) {
            return Err(format!("{} is already claimed", s.display_name));
        }
        let jumps = self
            .jumps_to(empire, system)
            .ok_or_else(|| format!("no hyperlane route to {}", s.display_name))?;
        let (cost, _) = self.rules().expansion(jumps.max(1));
        if self.influence(empire) + 1e-9 < cost {
            return Err(format!(
                "claiming {} costs {cost:.0} influence",
                s.display_name
            ));
        }
        Ok(cost)
    }
}

pub(crate) type CmdResult = Result<Option<Event>, String>;

pub(crate) fn declare_war(
    world: &mut World,
    empire: EmpireId,
    target: EmpireId,
    goal: WarGoal,
) -> CmdResult {
    let cost = world.can_declare_war(empire, target, goal)?;
    world.pay_influence(empire, cost);
    let [off, def] = world.war_sides(empire, target);
    let (a, b) = (
        world.empires[empire.idx()].display_name.clone(),
        world.empires[target.idx()].display_name.clone(),
    );
    let base = format!(
        "war-{}-{}",
        world.empires[empire.idx()].name,
        world.empires[target.idx()].name
    );
    let id = WarId::from_idx(world.wars.len());
    world.wars.push(War {
        name: unique_name(world.wars.iter().map(|w| w.name.as_str()), &base),
        display_name: format!("{a}–{b} War"),
        goals: [Some(goal), None],
        starting_day: world.day,
        exhaustion: [0.0; 2],
        victory: None,
        proposals: [None, None],
    });
    for (side, members) in [(Side::Offense, off), (Side::Defense, def)] {
        for e in members {
            world.empires[e.idx()].wars.push((id, side));
        }
    }
    world.topology_dirty = true;
    Ok(Some(
        Event::new(
            world.day,
            EventKind::WarDeclared,
            format!("{a} declares a {} war on {b}", goal.as_str()),
        )
        .empire(Some(empire)),
    ))
}

pub(crate) fn set_war_goal(
    world: &mut World,
    empire: EmpireId,
    war: WarId,
    goal: WarGoal,
) -> CmdResult {
    world.can_set_war_goal(empire, war, goal)?;
    world.wars[war.idx()].goals[Side::Defense.idx()] = Some(goal);
    let msg = format!(
        "{} fights {} for a {} goal",
        world.empires[empire.idx()].display_name,
        world.wars[war.idx()].display_name,
        goal.as_str()
    );
    Ok(Some(
        Event::new(world.day, EventKind::Command, msg).empire(Some(empire)),
    ))
}

/// `empire` proposes ending `war` with `outcome`; if the other side already proposed the same,
/// the war ends.
pub(crate) fn propose_peace(
    world: &mut World,
    empire: EmpireId,
    war: WarId,
    outcome: WarOutcome,
) -> CmdResult {
    world.check_empire(empire)?;
    let w = world.check_war(war)?;
    let side = world
        .side_of(war, empire)
        .ok_or("not taking part in this war")?;
    if w.proposals[side.other().idx()] == Some(outcome) {
        end_war(world, war, outcome);
        return Ok(None);
    }
    world.wars[war.idx()].proposals[side.idx()] = Some(outcome);
    let msg = format!(
        "{} proposes to end {} in {}",
        world.empires[empire.idx()].display_name,
        world.wars[war.idx()].display_name,
        match outcome {
            WarOutcome::StatusQuo => "a status quo".to_string(),
            o => format!("{} victory", o.as_str()),
        }
    );
    Ok(Some(
        Event::new(world.day, EventKind::Command, msg).empire(Some(empire)),
    ))
}

/// `empire` accepts the other side's proposal.
pub(crate) fn accept_peace(world: &mut World, empire: EmpireId, war: WarId) -> CmdResult {
    world.check_empire(empire)?;
    let w = world.check_war(war)?;
    let side = world
        .side_of(war, empire)
        .ok_or("not taking part in this war")?;
    let outcome = w.proposals[side.other().idx()].ok_or("the other side has proposed nothing")?;
    end_war(world, war, outcome);
    Ok(None)
}

/// `empire` imposes `outcome` on a fully exhausted enemy.
pub(crate) fn enforce_peace(
    world: &mut World,
    empire: EmpireId,
    war: WarId,
    outcome: WarOutcome,
) -> CmdResult {
    world.check_empire(empire)?;
    let w = world.check_war(war)?;
    let side = world
        .side_of(war, empire)
        .ok_or("not taking part in this war")?;
    if w.exhaustion[side.other().idx()] < 1.0 {
        return Err("the enemy is not exhausted yet".into());
    }
    end_war(world, war, outcome);
    Ok(None)
}

pub(crate) fn claim(world: &mut World, empire: EmpireId, system: SystemId) -> CmdResult {
    let cost = world.claim_cost(empire, system)?;
    world.pay_influence(empire, cost);
    world.empires[empire.idx()].claims.push(system);
    let msg = format!(
        "{} claims {}",
        world.empires[empire.idx()].display_name,
        world.systems[system.idx()].display_name
    );
    Ok(Some(
        Event::new(world.day, EventKind::Command, msg).empire(Some(empire)),
    ))
}

pub(crate) fn drop_claim(world: &mut World, empire: EmpireId, system: SystemId) -> CmdResult {
    world.check_empire(empire)?;
    let old = world.empires[empire.idx()].claims.clone();
    world.empires[empire.idx()].claims.retain(|c| *c != system);
    if world.empires[empire.idx()].claims.len() == old.len() {
        return Err("no such claim".into());
    }
    if let Err(e) = world.check_wars_of(empire) {
        world.empires[empire.idx()].claims = old;
        return Err(format!("this claim is needed: {e}"));
    }
    Ok(None)
}

pub(crate) fn create_alliance(
    world: &mut World,
    empire: EmpireId,
    display_name: String,
    invite: Vec<EmpireId>,
) -> CmdResult {
    world.check_empire(empire)?;
    for i in &invite {
        world.check_empire(*i)?;
    }
    let id = AllianceId::from_idx(world.alliances.len());
    let base = format!("alliance-{}", world.empires[empire.idx()].name);
    world.alliances.push(Alliance {
        name: unique_name(world.alliances.iter().map(|a| a.name.as_str()), &base),
        display_name: display_name.clone(),
        invites: Vec::new(),
    });
    world.empires[empire.idx()].alliances.push(id);
    // Invitations that can't stand (e.g. to an enemy) are dropped.
    for i in invite {
        let _ = invite_to_alliance(world, empire, id, i);
    }
    let msg = format!(
        "{} founds the {display_name}",
        world.empires[empire.idx()].display_name
    );
    Ok(Some(
        Event::new(world.day, EventKind::Command, msg).empire(Some(empire)),
    ))
}

impl World {
    /// The members of an alliance.
    pub fn alliance_members(&self, alliance: AllianceId) -> Vec<EmpireId> {
        (0..self.empires.len())
            .map(EmpireId::from_idx)
            .filter(|e| self.empires[e.idx()].alliances.contains(&alliance))
            .collect()
    }

    /// Whether `empire` could join `alliance` (allies cannot fight each other).
    pub fn can_join_alliance(&self, empire: EmpireId, alliance: AllianceId) -> Result<(), String> {
        self.check_empire(empire)?;
        let a = self
            .alliances
            .get(alliance.idx())
            .ok_or("no such alliance")?;
        if self.empires[empire.idx()].alliances.contains(&alliance) {
            return Err(format!("already in the {}", a.display_name));
        }
        if let Some(m) = self
            .alliance_members(alliance)
            .into_iter()
            .find(|m| self.at_war(empire, *m))
        {
            return Err(format!(
                "at war with {}, a member of the {}",
                self.empires[m.idx()].display_name,
                a.display_name
            ));
        }
        Ok(())
    }
}

/// A member invites `invitee`, who may then join.
pub(crate) fn invite_to_alliance(
    world: &mut World,
    empire: EmpireId,
    alliance: AllianceId,
    invitee: EmpireId,
) -> CmdResult {
    world.check_empire(empire)?;
    world.can_join_alliance(invitee, alliance)?;
    if !world.empires[empire.idx()].alliances.contains(&alliance) {
        return Err("only members can invite".into());
    }
    let a = &mut world.alliances[alliance.idx()];
    if !a.invites.contains(&invitee) {
        a.invites.push(invitee);
    }
    let msg = format!(
        "{} invites {} into the {}",
        world.empires[empire.idx()].display_name,
        world.empires[invitee.idx()].display_name,
        world.alliances[alliance.idx()].display_name
    );
    Ok(Some(
        Event::new(world.day, EventKind::Command, msg).empire(Some(invitee)),
    ))
}

/// Joining takes an invitation from a member (or an alliance with no members left).
pub(crate) fn join_alliance(
    world: &mut World,
    empire: EmpireId,
    alliance: AllianceId,
) -> CmdResult {
    world.can_join_alliance(empire, alliance)?;
    let a = &world.alliances[alliance.idx()];
    if !a.invites.contains(&empire) && !world.alliance_members(alliance).is_empty() {
        return Err(format!("not invited into the {}", a.display_name));
    }
    let msg = format!(
        "{} joins the {}",
        world.empires[empire.idx()].display_name,
        a.display_name
    );
    world.alliances[alliance.idx()]
        .invites
        .retain(|i| *i != empire);
    world.empires[empire.idx()].alliances.push(alliance);
    Ok(Some(
        Event::new(world.day, EventKind::Command, msg).empire(Some(empire)),
    ))
}

pub(crate) fn leave_alliance(
    world: &mut World,
    empire: EmpireId,
    alliance: AllianceId,
) -> CmdResult {
    world.check_empire(empire)?;
    let list = &mut world.empires[empire.idx()].alliances;
    let before = list.len();
    list.retain(|a| *a != alliance);
    if list.len() == before {
        return Err("not a member of that alliance".into());
    }
    // An invitation stands only while the inviting side is still there.
    if world.alliance_members(alliance).is_empty() {
        world.alliances[alliance.idx()].invites.clear();
    }
    Ok(None)
}
