//! `CortexForge` CLI entry point.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use forge_core::config::ForgeConfig;
use forge_core::protocol::NodeDefinition;

#[derive(Parser)]
#[command(
    name = "forge",
    about = "CortexForge — MCU embedded agent orchestration",
    version
)]
struct Cli {
    /// Project root directory
    #[arg(short, long, default_value = ".", env = "FORGE_ROOT")]
    root: PathBuf,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new `CortexForge` project
    Init {
        /// Project name (defaults to directory name)
        #[arg(short, long)]
        name: Option<String>,
    },

    /// Validate project configuration
    Validate,

    /// Start the orchestrator daemon
    Run {
        /// Run in background (daemon mode)
        #[arg(short, long)]
        daemon: bool,
    },

    /// Show node status tree
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Kill a node or subtree
    Kill {
        /// Node name to kill
        node: String,

        /// Skip grace period, send SIGKILL immediately
        #[arg(short, long)]
        force: bool,

        /// Recursively kill children
        #[arg(short, long)]
        cascade: bool,
    },

    /// Read the event bus log
    Log {
        /// Filter by node name
        #[arg(short, long)]
        node: Option<String>,

        /// Filter by event type
        #[arg(short, long)]
        event: Option<String>,

        /// Filter events since timestamp (RFC 3339)
        #[arg(long)]
        since: Option<String>,

        /// Follow mode (tail -f)
        #[arg(short, long)]
        follow: bool,
    },

    /// Node management subcommands
    #[command(subcommand)]
    Node(NodeCommand),
}

#[derive(Subcommand)]
enum NodeCommand {
    /// List all declared nodes
    List,
    /// Show details for a specific node
    Show { name: String },
    /// Manually spawn a node (debugging)
    Spawn { name: String },
    /// Send a message to a node's inbox (debugging)
    Message { to: String },
}

fn main() {
    let cli = Cli::parse();

    forge_core::logging::init_orchestrator(cli.verbose);

    let result = match cli.command {
        Commands::Init { name } => cmd_init(&cli.root, name),
        Commands::Validate => cmd_validate(&cli.root),
        Commands::Run { daemon } => cmd_run(&cli.root, daemon),
        Commands::Status { json } => cmd_status(&cli.root, json),
        Commands::Kill { node, force, cascade } => cmd_kill(&cli.root, &node, force, cascade),
        Commands::Log { node, event, since, follow } => {
            cmd_log(&cli.root, node.as_deref(), event.as_deref(), since.as_deref(), follow)
        }
        Commands::Node(cmd) => match cmd {
            NodeCommand::List => cmd_node_list(&cli.root),
            NodeCommand::Show { name } => cmd_node_show(&cli.root, &name),
            NodeCommand::Spawn { name } => cmd_node_spawn(&cli.root, &name),
            NodeCommand::Message { to } => cmd_node_message(&cli.root, &to),
        },
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

// ── Command implementations ────────────────────────────────────────────────

fn cmd_init(root: &PathBuf, name: Option<String>) -> forge_core::error::ForgeResult<()> {
    let project_name = name.unwrap_or_else(|| {
        root.file_name().map_or_else(|| "cortexforge-project".into(), |n| n.to_string_lossy().to_string())
    });

    tracing::info!(name = %project_name, root = %root.display(), "initializing project");

    // Create directory structure
    std::fs::create_dir_all(root)?;
    std::fs::create_dir_all(root.join("modules"))?;
    std::fs::create_dir_all(root.join(".forge"))?;

    // Write forge.toml from template
    let forge_toml = FORGE_TOML_TEMPLATE.trim_start();
    let forge_toml_path = root.join("forge.toml");
    forge_core::atomic_write(&forge_toml_path, forge_toml)?;
    println!("Created {}", forge_toml_path.display());

    // Write .gitignore additions
    let gitignore = root.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
    let additions = "\n# CortexForge runtime artifacts\n.forge/eventbus.log\n**/.forge/stdout.log\n**/.forge/stderr.log\n**/.forge/inbox/processed/\n";
    if !existing.contains(".forge/eventbus.log") {
        std::fs::write(&gitignore, format!("{existing}{additions}"))?;
        println!("Updated {}", gitignore.display());
    }

    println!("Project '{project_name}' initialized successfully.");
    println!("Next: edit forge.toml, then run 'forge validate'");
    Ok(())
}

const FORGE_TOML_TEMPLATE: &str = r#"[forge]
schema_version = 1
max_depth = 4
max_total_nodes = 64
heartbeat_interval_sec = 15
heartbeat_timeout_sec = 60
default_max_retries = 3
stuck_threshold_heartbeats = 4
scan_interval_sec = 5
spawn_timeout_sec = 30

[budget.global]
max_tokens_total = 5_000_000
max_wallclock_total_sec = 14400

[[budget.per_layer]]
layer = 1
tokens = 300_000
wallclock_sec = 3600
model = "claude-sonnet-4-6"

[[budget.per_layer]]
layer = 2
tokens = 200_000
wallclock_sec = 1800
model = "claude-sonnet-4-6"

[[budget.per_layer]]
layer = 3
tokens = 100_000
wallclock_sec = 900
model = "claude-haiku-4-5"

[paths]
event_bus = ".forge/eventbus.log"
escalated = ".forge/escalated.toml"
"#;

fn cmd_validate(root: &PathBuf) -> forge_core::error::ForgeResult<()> {
    let forge_toml = root.join("forge.toml");
    if !forge_toml.exists() {
        return Err(forge_core::error::ForgeError::Config(format!(
            "forge.toml not found at {}. Run 'forge init' first.",
            forge_toml.display()
        )));
    }

    // Validate forge.toml
    let content = std::fs::read_to_string(&forge_toml)?;
    let config: ForgeConfig = toml::from_str(&content)
        .map_err(|e| forge_core::error::ForgeError::Config(format!("invalid forge.toml: {e}")))?;

    if config.forge.schema_version != 1 {
        return Err(forge_core::error::ForgeError::Config(format!(
            "unsupported forge.toml schema_version: {}",
            config.forge.schema_version
        )));
    }

    if config.forge.heartbeat_interval_sec >= config.forge.heartbeat_timeout_sec {
        return Err(forge_core::error::ForgeError::Config(
            "heartbeat_interval_sec must be < heartbeat_timeout_sec".into(),
        ));
    }

    println!("forge.toml: OK (schema_version={})", config.forge.schema_version);

    // Validate node.toml files
    let nodes = collect_node_defs(root)?;
    println!("node.toml files: OK ({} found)", nodes.len());
    println!("\nValidation passed.");
    Ok(())
}

fn cmd_run(_root: &PathBuf, daemon: bool) -> forge_core::error::ForgeResult<()> {
    if daemon {
        tracing::info!("daemon mode requested but not yet implemented");
        println!("Daemon mode not yet implemented. Running in foreground...");
    }
    println!("Orchestrator starting...");
    // TODO: Full orchestrator implementation in P7
    println!("Orchestrator running. Press Ctrl+C to stop.");
    println!("(Orchestrator main loop not yet implemented)");
    Ok(())
}

fn cmd_status(_root: &PathBuf, json: bool) -> forge_core::error::ForgeResult<()> {
    if json {
        println!("{{\"status\": \"not yet implemented\"}}");
    } else {
        println!("Node status tree not yet implemented.");
        println!("Run 'forge node list' to see declared nodes.");
    }
    Ok(())
}

fn cmd_kill(_root: &PathBuf, node: &str, force: bool, cascade: bool) -> forge_core::error::ForgeResult<()> {
    println!("Killing node '{node}' (force={force}, cascade={cascade})...");
    println!("(Kill mechanism not yet implemented)");
    Ok(())
}

fn cmd_log(
    root: &PathBuf,
    node: Option<&str>,
    event: Option<&str>,
    since: Option<&str>,
    follow: bool,
) -> forge_core::error::ForgeResult<()> {
    let eventbus_path = root.join(".forge/eventbus.log");
    if !eventbus_path.exists() {
        println!("No eventbus.log found. The orchestrator hasn't run yet.");
        return Ok(());
    }

    let bus = forge_core::eventbus::EventBus::open(&eventbus_path);
    let entries = if let Some(n) = node {
        bus.read_by_node(n)?
    } else if let Some(ev) = event {
        bus.read_by_event(ev)?
    } else if let Some(s) = since {
        bus.read_since(s)?
    } else {
        bus.read_all()?
    };

    for entry in &entries {
        println!(
            "{} [{}] {}",
            entry.ts.format("%Y-%m-%dT%H:%M:%S%:z"),
            entry.node,
            entry.event.name()
        );
    }

    if entries.is_empty() {
        println!("(no matching events)");
    } else {
        println!("--- {} events ---", entries.len());
    }

    if follow {
        println!("Follow mode not yet implemented.");
    }
    Ok(())
}

// ── Node subcommands ──────────────────────────────────────────────────────

fn cmd_node_list(root: &PathBuf) -> forge_core::error::ForgeResult<()> {
    let nodes = collect_node_defs(root)?;
    if nodes.is_empty() {
        println!("No nodes found. Create node.toml files in your project.");
        return Ok(());
    }
    for (name, cwd) in &nodes {
        println!("  {name} → {cwd}");
    }
    println!("{} node(s)", nodes.len());
    Ok(())
}

fn cmd_node_show(root: &PathBuf, name: &str) -> forge_core::error::ForgeResult<()> {
    use forge_core::protocol::{NodeDefinition, NodeState};
    let nodes = collect_node_defs(root)?;
    let cwd = nodes.iter().find(|(n, _)| n == name).map(|(_, c)| c);
    let Some(cwd) = cwd else {
        return Err(forge_core::error::ForgeError::Config(format!(
            "node '{name}' not found"
        )));
    };

    let node_path = root.join(cwd).join("node.toml");
    let state_path = root.join(cwd).join(".forge/state.toml");

    let def = NodeDefinition::load(&node_path)?;
    println!("Node: {}", def.node.name);
    println!("  Role: {:?}", def.node.role);
    println!("  Depth: {}", def.node.depth);
    println!("  Parent: {}", def.node.parent);
    println!("  Children: {:?}", def.children.declared);

    if state_path.exists() {
        let state = NodeState::load(&state_path)?;
        println!("  State: {}", state.state.current);
        println!("  Sequence: {}", state.state.sequence);
        println!("  Progress: {}% — {}", state.progress.percent_self_estimate, state.progress.summary);
    } else {
        println!("  State: (not yet spawned)");
    }

    Ok(())
}

fn cmd_node_spawn(_root: &PathBuf, name: &str) -> forge_core::error::ForgeResult<()> {
    println!("Spawning node '{name}'... (not yet implemented)");
    Ok(())
}

fn cmd_node_message(_root: &PathBuf, to: &str) -> forge_core::error::ForgeResult<()> {
    println!("Sending message to '{to}'... (not yet implemented)");
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Recursively find all node.toml files under the project root.
fn collect_node_defs(root: &PathBuf) -> forge_core::error::ForgeResult<Vec<(String, String)>> {
    
    let mut nodes = Vec::new();
    collect_node_defs_recursive(root, root, &mut nodes)?;
    Ok(nodes)
}

fn collect_node_defs_recursive(
    root: &PathBuf,
    dir: &PathBuf,
    nodes: &mut Vec<(String, String)>,
) -> forge_core::error::ForgeResult<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            // Skip .forge and .git dirs
            let name = path.file_name().map(|n| n.to_string_lossy());
            if let Some(ref n) = name {
                if n.starts_with('.') && n != "." {
                    continue;
                }
            }
            // Check for node.toml in this directory
            let node_toml = path.join("node.toml");
            if node_toml.exists() {
                if let Ok(def) = NodeDefinition::load(&node_toml) {
                    let rel = path.strip_prefix(root).unwrap_or(&path);
                    nodes.push((def.node.name.clone(), rel.to_string_lossy().to_string()));
                }
            }
            collect_node_defs_recursive(root, &path, nodes)?;
        }
    }
    Ok(())
}
