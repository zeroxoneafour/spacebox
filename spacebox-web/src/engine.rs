//! The server-side engine: the live [`World`] plus the bookkeeping needed for diffing.
//!
//! Every time a client asks for state, a snapshot is taken (see [`Snapshot`]). If it differs
//! from the latest one it becomes a new *version*. The last few versions are kept (sharing the
//! text of unchanged entities) so that a client that reports the version it holds gets only a
//! diff. A new *epoch* starts whenever the world is replaced (definitions may differ), which
//! forces clients to refetch everything.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use spacebox_core::World;
use spacebox_core::rng::Rng;

use crate::diff;
use crate::snapshot::{self, Snapshot};

const SNAPSHOTS_KEPT: usize = 8;
const EVENTS_KEPT: usize = 2000;

pub struct Engine {
    pub world: World,
    pub data_dir: PathBuf,
    pub rng: Rng,
    pub epoch: u64,
    version: u64,
    snapshots: VecDeque<(u64, Arc<Snapshot>)>,
    defs: Arc<Value>,
    events: VecDeque<Value>,
    next_event: u64,
}

/// What the client says it already has.
#[derive(Debug, Default, serde::Deserialize)]
pub struct Known {
    pub since: Option<u64>,
    pub epoch: Option<u64>,
    pub since_event: Option<u64>,
}

impl Engine {
    pub fn new(world: World, data_dir: PathBuf) -> Self {
        let mut e = Engine {
            rng: Rng::new(world.seed ^ 0xC0FFEE),
            defs: Arc::new(snapshot::defs(&world.defs, &world)),
            world,
            data_dir,
            epoch: 1,
            version: 0,
            snapshots: VecDeque::new(),
            events: VecDeque::new(),
            next_event: 1,
        };
        e.world.refresh_topology();
        e
    }

    /// Replaces the world (load/regenerate) and starts a new epoch.
    pub fn replace(&mut self, world: World) {
        self.defs = Arc::new(snapshot::defs(&world.defs, &world));
        self.rng = Rng::new(world.seed ^ 0xC0FFEE);
        self.world = world;
        self.world.refresh_topology();
        self.epoch += 1;
        self.snapshots.clear();
        // Old events refer to the old world's ids.
        self.world.drain_events();
        self.events.clear();
    }

    pub fn defs_json(&self) -> Arc<Value> {
        self.defs.clone()
    }

    pub fn collect_events(&mut self) {
        for e in self.world.drain_events() {
            self.events.push_back(snapshot::event(self.next_event, &e));
            self.next_event += 1;
            if self.events.len() > EVENTS_KEPT {
                self.events.pop_front();
            }
        }
    }

    pub fn tick(&mut self, days: u64) {
        for _ in 0..days {
            self.world.tick();
        }
        self.collect_events();
    }

    /// Takes a snapshot, registering a new version if anything changed.
    fn snapshot(&mut self) -> (u64, Arc<Snapshot>) {
        let prev = self.snapshots.back().map(|(_, s)| &**s);
        let state = snapshot::state(&self.world, prev);
        if let Some((v, last)) = self.snapshots.back()
            && last.same(&state)
        {
            return (*v, last.clone());
        }
        self.version += 1;
        let state = Arc::new(state);
        self.snapshots.push_back((self.version, state.clone()));
        if self.snapshots.len() > SNAPSHOTS_KEPT {
            self.snapshots.pop_front();
        }
        (self.version, state)
    }

    fn events_since(&self, since: Option<u64>) -> Vec<Value> {
        let from = since.unwrap_or(0);
        let skip = self.events.len().saturating_sub(200);
        self.events
            .iter()
            .skip(if since.is_none() { skip } else { 0 })
            .filter(|e| e["seq"].as_u64().unwrap_or(0) > from)
            .cloned()
            .collect()
    }

    /// Full state for a fresh client, as JSON text.
    pub fn full(&mut self) -> String {
        self.collect_events();
        let (version, state) = self.snapshot();
        response(&[
            ("epoch", json!(self.epoch).to_string()),
            ("version", version.to_string()),
            ("event_seq", (self.next_event - 1).to_string()),
            ("state", state.to_json()),
            ("events", Value::from(self.events_since(None)).to_string()),
        ])
    }

    /// Diff from the client's version to now, or the full state if that version is gone, as
    /// JSON text.
    pub fn sync(&mut self, known: &Known) -> String {
        self.collect_events();
        let (version, state) = self.snapshot();
        let base = known
            .since
            .filter(|_| known.epoch == Some(self.epoch))
            .and_then(|v| {
                self.snapshots
                    .iter()
                    .find(|(sv, _)| *sv == v)
                    .map(|(_, s)| s.clone())
            });
        let events = Value::from(self.events_since(known.since_event)).to_string();
        let event_seq = (self.next_event - 1).to_string();
        let epoch = self.epoch.to_string();
        match base {
            Some(base) => response(&[
                ("epoch", epoch),
                ("version", version.to_string()),
                ("base", json!(known.since).to_string()),
                (
                    "ops",
                    Value::from(diff::diff_snapshots(&base, &state)).to_string(),
                ),
                ("events", events),
                ("event_seq", event_seq),
            ]),
            None => response(&[
                ("epoch", epoch),
                ("version", version.to_string()),
                ("state", state.to_json()),
                ("events", events),
                ("event_seq", event_seq),
            ]),
        }
    }

    /// Resolves a path relative to the data directory, refusing to escape it.
    pub fn data_path(&self, rel: &str) -> Result<PathBuf, String> {
        let p = Path::new(rel);
        if p.is_absolute()
            || p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("paths must be relative to the data directory".into());
        }
        Ok(self.data_dir.join(p))
    }

    pub fn used_system_names(&self) -> HashSet<String> {
        self.world
            .systems
            .iter()
            .map(|s| s.display_name.clone())
            .collect()
    }
}

/// A JSON object from already-serialized values.
fn response(fields: &[(&str, String)]) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("\"{k}\":{v}"));
    }
    out.push('}');
    out
}
