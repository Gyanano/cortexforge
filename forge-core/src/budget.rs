//! Budget tracking and enforcement.
//!
//! Implements budget checks from §5.1 (spawn pre-checks) and §6.5 (suicide gate).
//! BudgetTracker lives in types.rs; this module provides global/per-layer checking.

use crate::config::ForgeConfig;
use crate::types::NodeDepth;

/// Check whether there is enough remaining budget for a child node.
///
/// Checks both global max_tokens_total and the per-layer token budget,
/// returning the tighter constraint. Used by spawn pre-checks (§5.1).
pub fn remaining_budget(
    config: &ForgeConfig,
    child_depth: NodeDepth,
    request_tokens: u64,
    current_total_tokens: u64,
    current_layer_tokens: u64,
) -> Option<u64> {
    let layer = child_depth.as_u32();

    // Check global budget
    let global_remaining = config
        .budget
        .global
        .max_tokens_total
        .map(|max| max.saturating_sub(current_total_tokens));

    // Check per-layer budget
    let layer_remaining = config
        .budget
        .per_layer
        .iter()
        .find(|lb| lb.layer == layer)
        .and_then(|lb| lb.tokens)
        .map(|max| max.saturating_sub(current_layer_tokens));

    // Take the more restrictive
    let min_remaining = match (global_remaining, layer_remaining) {
        (Some(g), Some(l)) => Some(g.min(l)),
        (Some(g), None) => Some(g),
        (None, Some(l)) => Some(l),
        (None, None) => return None, // unlimited
    };

    if let Some(rem) = min_remaining {
        if rem < request_tokens {
            return Some(rem); // insufficient
        }
    }

    min_remaining
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BudgetSection, ForgeSection, GlobalBudget, LayerBudgetEntry, PathsSection};

    fn test_config() -> ForgeConfig {
        ForgeConfig {
            forge: ForgeSection {
                schema_version: 1,
                max_depth: 4,
                max_total_nodes: 64,
                heartbeat_interval_sec: 15,
                heartbeat_timeout_sec: 60,
                default_max_retries: 3,
                stuck_threshold_heartbeats: 4,
                scan_interval_sec: 5,
                spawn_timeout_sec: 30,
            },
            budget: BudgetSection {
                global: GlobalBudget {
                    max_tokens_total: Some(1_000_000),
                    max_wallclock_total_sec: Some(3600),
                },
                per_layer: vec![LayerBudgetEntry {
                    layer: 2,
                    tokens: Some(200_000),
                    wallclock_sec: Some(1800),
                    model: Some("claude-sonnet-4-6".into()),
                }],
            },
            paths: PathsSection::default(),
        }
    }

    #[test]
    fn test_budget_exceeds_global() {
        let cfg = test_config();
        let rem = remaining_budget(&cfg, NodeDepth(2), 100_000, 950_000, 50_000);
        assert!(rem.is_some());
    }

    #[test]
    fn test_budget_sufficient() {
        let cfg = test_config();
        let rem = remaining_budget(&cfg, NodeDepth(2), 50_000, 100_000, 50_000);
        assert!(rem.is_some());
    }

    #[test]
    fn test_budget_no_limits() {
        let mut cfg = test_config();
        cfg.budget.global.max_tokens_total = None;
        cfg.budget.per_layer.clear();
        let rem = remaining_budget(&cfg, NodeDepth(2), 999_999, 0, 0);
        assert!(rem.is_none()); // unlimited
    }
}
