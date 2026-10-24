//! Narrative events emitted by the simulation, for logs and storytelling.

use crate::ids::{EmpireId, PlanetId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    ConstructionStarted,
    ConstructionFinished,
    Terraformed,
    ResearchFinished,
    CrisisStarted,
    CrisisResolved,
    Starvation,
    Unrest,
    Settled,
    Abandoned,
    CapitalChanged,
    Command,
    CommandRejected,
    SystemClaimed,
    ClaimLost,
    ShipLaunched,
    FleetArrived,
    Battle,
    ShipDestroyed,
    FleetDestroyed,
    Bombardment,
    BuildingDestroyed,
    Surrendered,
    WarDeclared,
    PeaceMade,
    EmpireFallen,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub day: u64,
    pub kind: EventKind,
    pub empire: Option<EmpireId>,
    pub planet: Option<PlanetId>,
    pub message: String,
}

impl Event {
    pub fn new(day: u64, kind: EventKind, message: impl Into<String>) -> Self {
        Event {
            day,
            kind,
            empire: None,
            planet: None,
            message: message.into(),
        }
    }

    pub fn empire(mut self, e: Option<EmpireId>) -> Self {
        self.empire = e;
        self
    }

    pub fn planet(mut self, p: PlanetId) -> Self {
        self.planet = Some(p);
        self
    }
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[day {:>5}] {}", self.day, self.message)
    }
}
