//! Core types for the CortexForge orchestration tree.

use serde::{Deserialize, Serialize};

// ── Node identity ──

/// Role of a node in the orchestration tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    /// L0 — pure-code daemon, no LLM
    Orchestrator,
    /// L1 — manages a domain and its sub-modules
    Domain,
    /// L2+ — implements a specific module
    Module,
    /// L3+ — sub-module, same mechanism as Module
    Submodule,
}

impl std::fmt::Display for NodeRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Orchestrator => write!(f, "orchestrator"),
            Self::Domain => write!(f, "domain"),
            Self::Module => write!(f, "module"),
            Self::Submodule => write!(f, "submodule"),
        }
    }
}

impl std::str::FromStr for NodeRole {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "orchestrator" => Ok(Self::Orchestrator),
            "domain" => Ok(Self::Domain),
            "module" => Ok(Self::Module),
            "submodule" => Ok(Self::Submodule),
            _ => Err(format!("unknown node role: {s}")),
        }
    }
}

/// A unique name for a node (e.g. "module-bsp-uart").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeName(String);

impl NodeName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Depth of a node in the tree (0 = root Orchestrator).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeDepth(pub u32);

impl NodeDepth {
    pub const ROOT: Self = Self(0);

    pub fn as_u32(&self) -> u32 {
        self.0
    }

    pub fn child_depth(&self) -> Self {
        Self(self.0 + 1)
    }
}

impl std::fmt::Display for NodeDepth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Relative path from forge root to a node's working directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodePath(String);

impl NodePath {
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// ── Dependency types (§4.4, §15) ──

/// A monotonically increasing version number for provided values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Seq(pub u64);

impl Seq {
    pub const ZERO: Self = Self(0);

    pub fn next(&self) -> Self {
        Self(self.0 + 1)
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for Seq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A dependency key (e.g. "APB1_CLK", "UART_TX_PIN").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DependencyKey(String);

impl DependencyKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DependencyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A dependency value provided by a module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyValue {
    pub value: String,
    #[serde(default)]
    pub desc: String,
    pub seq: Seq,
}

// ── Budget / time types (§4.6) ──

use chrono::{DateTime, FixedOffset};

/// Wall-clock timestamp (RFC 3339 with timezone).
pub type Timestamp = DateTime<FixedOffset>;

/// Tracks token and wall-clock consumption against limits.
#[derive(Debug, Clone, Default)]
pub struct BudgetTracker {
    pub tokens_used: u64,
    pub max_tokens: Option<u64>,
    pub wallclock_sec_used: u64,
    pub max_wallclock_sec: Option<u64>,
    pub started_at: Option<Timestamp>,
}

impl BudgetTracker {
    pub fn new(max_tokens: Option<u64>, max_wallclock_sec: Option<u64>) -> Self {
        Self {
            max_tokens,
            max_wallclock_sec,
            ..Default::default()
        }
    }

    pub fn tokens_exhausted(&self) -> bool {
        self.max_tokens.map_or(false, |max| self.tokens_used >= max)
    }

    pub fn wallclock_exhausted(&self) -> bool {
        self.max_wallclock_sec
            .map_or(false, |max| self.wallclock_sec_used >= max)
    }

    pub fn is_exhausted(&self) -> bool {
        self.tokens_exhausted() || self.wallclock_exhausted()
    }
}
