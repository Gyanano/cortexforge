//! Logging infrastructure — tracing setup for Orchestrator and nodes.

use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

/// Initialize tracing for the Orchestrator process.
///
/// Output format: human-readable to stderr by default; JSON when `FORGE_LOG_JSON=1`.
pub fn init_orchestrator(verbose: bool) {
    let env_filter = if verbose {
        EnvFilter::new("forge=debug,forge_core=debug")
    } else {
        EnvFilter::new("forge=info,forge_core=info")
    };

    let use_json = std::env::var("FORGE_LOG_JSON").is_ok();

    if use_json {
        let layer = fmt::layer()
            .json()
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_filter(env_filter);
        tracing_subscriber::registry().with(layer).init();
    } else {
        let layer = fmt::layer().with_target(false).with_thread_ids(false).with_filter(env_filter);
        tracing_subscriber::registry().with(layer).init();
    }
}

/// Initialize tracing for a node (claude subprocess).
///
/// Nodes always log to stderr (which is redirected to `.forge/stderr.log`).
pub fn init_node(node_name: &str, verbose: bool) {
    let level = if verbose { "debug" } else { "info" };
    let env_filter = EnvFilter::new(format!("forge_sdk={level}"));
    let layer = fmt::layer().with_target(false).with_filter(env_filter);
    tracing_subscriber::registry().with(layer).init();
    tracing::info!(node = %node_name, "node logging initialized");
}
