//! CortexForge core library.
//!
//! Provides the foundational types, file protocol implementations,
//! state machine engine, and dependency resolution logic for the
//! N-level recursive agent orchestration tree.

pub mod types;
pub mod error;
pub mod config;
pub mod protocol;
pub mod state;
pub mod event;
pub mod eventbus;
pub mod logging;
pub mod budget;
pub mod spawn;
pub mod heartbeat;
pub mod permission;
pub mod deps;
pub mod deliverables;

mod atomic;
pub use atomic::{atomic_write, safe_read_toml};
