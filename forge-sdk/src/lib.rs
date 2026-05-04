//! `CortexForge` per-node SDK.
//!
//! Provides the runtime environment for individual nodes in the
//! orchestration tree: lifecycle management, heartbeat watchdog,
//! budget tracking, verify-gate execution, and prompt construction.

pub mod runtime;
pub mod prompt;

pub use runtime::NodeRuntime;
