//! spacebox-core: the simulation engine behind Spacebox, a galactic-scale economy sandbox.
//!
//! The crate is organised around the data flow of the game:
//!
//! * [`loader`] reads TOML asset/save files (with `[import]` aliasing) into strongly-typed
//!   definitions ([`defs`]) and live game state ([`world`]).
//! * [`geometry`] maps planet tile indices onto faces of a dodecahedron (small planets) or a
//!   truncated icosahedron (large planets) and provides the adjacency graph.
//! * [`sim`] runs the parallel tick loop: planets → systems → empires → galaxy.
//! * [`save`] writes the world back out as a TOML save file.
//! * [`generate`] builds random galaxies from a set of asset files.

pub mod defs;
pub mod error;
pub mod generate;
pub mod geometry;
pub mod ids;
pub mod loader;
mod raw;
pub mod rng;
pub mod save;
pub mod sim;
mod split;
pub mod world;

pub use defs::Defs;
pub use error::{Error, Result};
pub use ids::*;
pub use loader::{load_assets, load_world};
pub use sim::commands::Command;
pub use sim::events::{Event, EventKind};
pub use world::World;
