//! `CortexForge` core library.
//!
//! Provides the foundational types, file protocol implementations,
//! state machine engine, and dependency resolution logic for the
//! N-level recursive agent orchestration tree.

pub mod budget;
pub mod config;
pub mod deliverables;
pub mod deps;
pub mod error;
pub mod event;
pub mod eventbus;
pub mod heartbeat;
pub mod logging;
pub mod permission;
pub mod protocol;
pub mod spawn;
pub mod state;
pub mod types;

mod atomic;
pub use atomic::{atomic_write, safe_read_toml};
