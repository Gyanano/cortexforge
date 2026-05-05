//! `CortexForge` CLI entry point.
#![allow(clippy::ptr_arg)]

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use forge_core::config::ForgeConfig;
use forge_core::protocol::NodeDefinition;

#[derive(Parser)]
#[command(name = "forge", about = "CortexForge — MCU embedded agent orchestration", version)]
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
    // ── Step 1: Project name ──
    let project_name = name.unwrap_or_else(|| {
        root.file_name()
            .map_or_else(|| "cortexforge-project".into(), |n| n.to_string_lossy().to_string())
    });
    println!("CortexForge — project initialization wizard\n");
    println!("Project: {project_name}");
    println!("Root:    {}", root.display());
    println!();

    // ── Step 2: API key ──
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .or_else(|_| std::env::var("CLAUDE_API_KEY"))
        .unwrap_or_default();
    let api_ready = if api_key.is_empty() {
        println!("⚠  No ANTHROPIC_API_KEY or CLAUDE_API_KEY found in environment.");
        println!("  Set it before running 'forge run', or enter it now (leave blank to skip):");
        print!("  API Key: ");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).ok();
        let key = input.trim().to_string();
        if key.is_empty() {
            println!("  → Skipped. Set ANTHROPIC_API_KEY before forge run.\n");
            false
        } else {
            // Store key in a static for later use (set_var is unsafe in 2024 edition)
            // The key is stored in the forge.toml path, or passed via environment
            // For now, just warn: the user should export ANTHROPIC_API_KEY
            println!(
                "  → Note: please export ANTHROPIC_API_KEY={} before forge run.\n",
                if key.len() > 4 {
                    format!("{}...{}", &key[..2], &key[key.len() - 2..])
                } else {
                    "****".into()
                }
            );
            true
        }
    } else {
        println!("✓ API key found in environment.\n");
        true
    };

    // ── Step 3: MCU selection ──
    let mcu = select_option("Select target MCU family:", MCU_OPTIONS);
    println!();

    // ── Step 4: Toolchain ──
    let toolchain = select_option("Select build system / toolchain:", TOOLCHAIN_OPTIONS);
    println!();

    // ── Step 5: Standard layers ──
    let layers = select_layers(EMBEDDED_LAYERS);
    println!();

    // ── Step 6: Module descriptions per layer ──
    let mut layer_descriptions: Vec<(String, String)> = Vec::new();
    for layer in &layers {
        println!("Describe modules for layer '{layer}'.");
        println!("  Examples: 'hal-clock needs to provide APB1_CLK, APB2_CLK, HCLK'");
        println!("            'driver-ws2812 needs TIM2_CH1 PWM output on PA0'");
        print!("  > ");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let mut desc = String::new();
        std::io::stdin().read_line(&mut desc).ok();
        let desc = desc.trim().to_string();
        if !desc.is_empty() {
            layer_descriptions.push((layer.clone(), desc));
        }
    }
    println!();

    // ── Step 7: Create project skeleton ──
    std::fs::create_dir_all(root)?;
    std::fs::create_dir_all(root.join("modules"))?;
    std::fs::create_dir_all(root.join(".forge"))?;

    // Write forge.toml
    let forge_toml = build_forge_toml(&mcu, &toolchain);
    let forge_toml_path = root.join("forge.toml");
    forge_core::atomic_write(&forge_toml_path, &forge_toml)?;
    println!("Created {}", forge_toml_path.display());

    // Create skeleton node.toml files for each layer with description
    for (layer, desc) in &layer_descriptions {
        let parts: Vec<&str> = desc.split_whitespace().collect();
        let module_name =
            parts.first().map_or(layer.as_str(), |s| s.strip_suffix(':').unwrap_or(s));
        let sanitized = module_name.replace(['/', '\\', ' '], "-");
        let dir = root.join("modules").join(&sanitized);
        std::fs::create_dir_all(dir.join(".forge"))?;
        std::fs::create_dir_all(dir.join("shared"))?;

        let node_toml = build_skeleton_node_toml(&sanitized, layer);
        forge_core::atomic_write(&dir.join("node.toml"), &node_toml)?;
        if sanitized != module_name {
            // Write description comment to a separate file for LLM to read
            std::fs::write(dir.join("DESCRIPTION.txt"), desc)?;
        }
        println!(
            "Created {}/node.toml ({})",
            dir.strip_prefix(root).unwrap_or(&dir).display(),
            layer
        );
    }

    // Write .gitignore
    let gitignore = root.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
    let additions = "\n# CortexForge runtime artifacts\n.forge/eventbus.log\n**/.forge/stdout.log\n**/.forge/stderr.log\n**/.forge/inbox/processed/\n";
    if !existing.contains(".forge/eventbus.log") {
        std::fs::write(&gitignore, format!("{existing}{additions}"))?;
    }

    // ── Step 8: Generate flesh-out prompt ──
    let prompt_path = root.join(".forge/FLESH_OUT_PROMPT.md");
    let prompt = build_flesh_out_prompt(
        &project_name,
        &mcu,
        &toolchain,
        api_ready,
        &layers,
        &layer_descriptions,
    );
    std::fs::write(&prompt_path, &prompt)?;

    println!();
    println!("Project '{project_name}' initialized successfully.");
    println!();
    println!("Next steps:");
    println!("  1. Review forge.toml and skeleton node.toml files");
    println!("  2. Set ANTHROPIC_API_KEY (if not done)");
    println!(
        "  3. Run: claude -p \"$(cat .forge/FLESH_OUT_PROMPT.md)\" --dangerously-skip-permissions"
    );
    println!(
        "     This will use Claude to flesh out verify.sh, CLAUDE.md, and complete node.toml files."
    );
    println!("  4. forge validate  — check everything is correct");
    println!("  5. forge run       — start the orchestrator");
    Ok(())
}

// ── Interactive helpers ──────────────────────────────────────────────────

fn select_option(prompt: &str, options: &[(char, &str, &str)]) -> String {
    println!("{prompt}");
    for (key, name, desc) in options {
        println!("  [{key}] {name} — {desc}");
    }
    print!("  choice (default: {}): ", options[0].0);
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).ok();
    let choice = input.trim().to_lowercase();
    for (key, name, _) in options {
        if choice == key.to_string() || choice == name.to_lowercase() {
            return name.to_string();
        }
    }
    options[0].1.to_string()
}

fn select_layers(options: &[(char, &str, &str)]) -> Vec<String> {
    println!("Select standard embedded layers to include (comma-separated, e.g. 'hal,bsp,mw'):");
    for (key, name, desc) in options {
        println!("  [{key}] {name} — {desc}");
    }
    print!("  layers (default: hal,bsp,mw): ");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).ok();
    let choices: Vec<String> = input
        .trim()
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if choices.is_empty() {
        return vec!["hal".into(), "bsp".into(), "mw".into()];
    }
    let mut selected = Vec::new();
    for choice in choices {
        for (key, name, _) in options {
            if choice == key.to_string() || choice == name.to_lowercase() {
                selected.push(name.to_string());
            }
        }
    }
    selected
}

// ── Hardcoded option tables ─────────────────────────────────────────────

/// MCU family options: (key, display_name, description)
const MCU_OPTIONS: &[(char, &str, &str)] = &[
    ('1', "STM32F1", "Cortex-M3, 64-128KB flash, common in Blue Pill"),
    ('2', "STM32F4", "Cortex-M4, 512KB-1MB flash, DSP + FPU"),
    ('3', "STM32G0", "Cortex-M0+, 32-128KB flash, low cost"),
    ('4', "STM32H7", "Cortex-M7, 1-2MB flash, high performance"),
    ('5', "ESP32", "Xtensa LX6, WiFi/BLE, 4-16MB flash"),
    ('6', "ESP32-S3", "Xtensa LX7, WiFi/BLE, USB-OTG, AI acceleration"),
    ('7', "RP2040", "Dual Cortex-M0+, 264KB SRAM, Raspberry Pi Pico"),
    ('8', "GD32F103", "Cortex-M3, STM32F103-compatible, lower cost"),
    ('9', "NXP-Kinetis", "Cortex-M4, various flash sizes"),
    ('0', "Other", "Generic ARM Cortex-M or other MCU"),
];

/// Toolchain options
const TOOLCHAIN_OPTIONS: &[(char, &str, &str)] = &[
    ('1', "arm-gcc-cmake", "arm-none-eabi-gcc + CMake, most common"),
    ('2', "arm-gcc-make", "arm-none-eabi-gcc + Makefile"),
    ('3', "stm32cube", "STM32CubeIDE / HAL library"),
    ('4', "esp-idf", "ESP-IDF framework (ESP32)"),
    ('5', "platformio", "PlatformIO (multi-platform)"),
    ('6', "pico-sdk", "Raspberry Pi Pico SDK (RP2040)"),
    ('7', "zephyr", "Zephyr RTOS"),
    ('8', "other", "Custom / other toolchain"),
];

/// Standard embedded layers
const EMBEDDED_LAYERS: &[(char, &str, &str)] = &[
    ('h', "hal", "Hardware Abstraction Layer — clock, gpio, uart, spi, i2c, tim, adc"),
    ('b', "bsp", "Board Support Package — pin mappings, board init, peripheral config"),
    ('m', "mw", "Middleware — RTOS, USB stack, filesystem, network, display driver"),
    ('a', "app", "Application — main logic, state machines, user-facing features"),
    ('d', "drv", "Drivers — external chips: sensors, LED drivers, motor controllers"),
    ('t', "test", "Tests — unit tests, HIL tests, integration tests"),
];

// ─── forge.toml builder ─────────────────────────────────────────────────

fn build_forge_toml(mcu: &str, toolchain: &str) -> String {
    format!(
        r#"# CortexForge project configuration
# Target: {mcu} / {toolchain}

[forge]
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
"#
    )
}

fn build_skeleton_node_toml(name: &str, layer: &str) -> String {
    let _role = layer; // all skeleton nodes default to module role
    format!(
        r#"# Skeleton node.toml — complete this file using the flesh-out prompt.
# See docs/01-architecture.md §4.1 for full syntax.

[node]
name = "{name}"
role = "module"
cwd = "modules/{name}"
parent = ""
depth = 1

[children]
declared = []
spawn_strategy = "lazy"

[provides]
declared = []

[budget]
max_tokens = 200_000
max_wallclock_sec = 1800
max_retries = 3
max_subprocess = 4

[runtime]
model = "claude-sonnet-4-6"
"#
    )
}

// ─── Flesh-out prompt builder ───────────────────────────────────────────

fn build_flesh_out_prompt(
    project_name: &str,
    mcu: &str,
    toolchain: &str,
    api_ready: bool,
    layers: &[String],
    descriptions: &[(String, String)],
) -> String {
    let desc_summary: String = descriptions
        .iter()
        .map(|(layer, desc)| format!("  [{layer}] {desc}"))
        .collect::<Vec<_>>()
        .join("\n");

    let layers_str = layers.join(", ");
    let api_note = if api_ready {
        "API key is configured and ready for use."
    } else {
        "⚠ API key is NOT configured. The user must set ANTHROPIC_API_KEY before running forge run."
    };

    format!(
        r#"You are assisting with a CortexForge MCU embedded project. Your job is to flesh out the
skeleton node.toml files, create verify.sh scripts, and write per-module CLAUDE.md files.

## Project Context
- Project name: {project_name}
- Target MCU: {mcu}
- Toolchain: {toolchain}
- Active layers: {layers_str}
- {api_note}

## User Descriptions (per layer)
{desc_summary}

## CortexForge Architecture Reference

### Directory Layout
Each module lives in its own directory under `modules/<name>/`:
```
modules/<name>/
  node.toml         — static node definition (§4.1)
  verify.sh         — verification script (§8)
  CLAUDE.md         — module-level methodology & toolchain notes
  .forge/           — runtime state (created by SDK at startup)
    state.toml
    inbox/
  shared/           — dependency protocol files
    needs.toml      — declares what this module NEEDS
    provides.toml   — declares what this module PROVIDES
    resolved.toml   — written by Orchestrator, values from other modules
    tasks.toml      — written by Orchestrator, tasks to fulfill
  deliverables/     — build outputs after verify passes
```

### node.toml Format (§4.1)
```toml
[node]
name = "<unique-name>"           # e.g. "hal-clock", "bsp-uart", "driver-ws2812"
role = "<role>"                  # domain | module | submodule
cwd = "modules/<name>"           # relative path from project root
parent = "<parent-node-name>"    # empty for root domain agent
depth = <n>                      # 1 = domain, 2 = module, 3+ = submodule

[children]
declared = ["child-a", "child-b"] # sub-modules this domain manages
spawn_strategy = "lazy"           # lazy (on-demand) | eager (at start)

[provides]
declared = ["INTERFACE_KEY_1", "INTERFACE_KEY_2"]
# Interface keys are UPPER_SNAKE_CASE identifiers that other modules can depend on.
# Examples: APB1_CLK, UART1_TX_PIN, TIM2_CH1_PWM, SPI1_SCK_PIN

[budget]
max_tokens = 200_000
max_wallclock_sec = 1800
max_retries = 3
max_subprocess = 4

[runtime]
model = "claude-sonnet-4-6"
```

### verify.sh Requirements (§8)
- Must be executable (`chmod +x verify.sh`)
- Exit code 0 = pass, non-zero = fail
- Should compile the module, run unit tests, check static analysis, verify
  memory/flash constraints, and validate public interface compatibility
- Output failure details to stdout/stderr

Example for {toolchain}:
```sh
#!/bin/sh
set -e
echo "[<module>] building..."
# Add compile command here, e.g.:
# arm-none-eabi-gcc -mcpu=cortex-m3 -mthumb -std=c11 -Wall -Werror -c src/*.c
echo "[<module>] running tests..."
# Add test commands here
echo "[<module>] verify PASS"
```

### CLAUDE.md Content
Each module's CLAUDE.md should document:
1. Toolchain assumptions (compiler, flags, linker script)
2. MCU-specific details (register maps, clock tree, peripheral config)
3. Coding conventions used in this module
4. Dependencies on other modules (but do NOT duplicate needs.toml)
5. Test methodology (how to run verify.sh locally)

### Dependency Protocol (§15)
- Modules declare needs in shared/needs.toml (interface keys like APB1_CLK)
- Modules declare provides in shared/provides.toml
- Orchestrator matches needs → provides and writes resolved values
- Key naming convention: UPPER_SNAKE_CASE, include peripheral instance number
  (e.g. UART1_TX_PIN vs UART2_TX_PIN)

### Role Definitions (§2.2)
- **domain**: Manages a group of sub-modules, requests child spawn,
  aggregates child states. Only domain agents can request spawn.
- **module**: Implements a specific hardware/software module. No spawn authority.
- **submodule**: Same as module, for deeper nesting.

## Your Task

For each module described in the user descriptions above:

1. **Complete node.toml**: Fill in `provides.declared` with concrete interface keys
   based on what this module offers. Use the MCU datasheet knowledge for {mcu}
   (e.g., STM32F1 clock tree: APB1 max 36MHz, APB2 max 72MHz).
   Fill in `children.declared` if the module needs sub-modules.

2. **Create verify.sh**: Write a verification script appropriate for {toolchain}
   and {mcu}. Include compile commands, basic test structure, and an exit 0 at the end.

3. **Create CLAUDE.md**: Document toolchain assumptions, register maps,
   peripheral configuration, and coding conventions specific to this module.

4. **Define dependencies**: Based on the module descriptions, determine what
   interface keys each module needs (write to shared/needs.toml) and what it
   provides (write to shared/provides.toml with seq=1).

## Output Format

For each module, output the complete file contents in this format:

```
=== modules/<name>/node.toml ===
<complete node.toml>

=== modules/<name>/verify.sh ===
<complete verify.sh>

=== modules/<name>/CLAUDE.md ===
<complete CLAUDE.md>

=== modules/<name>/shared/needs.toml ===
<complete needs.toml, or "NONE" if no dependencies>

=== modules/<name>/shared/provides.toml ===
<complete provides.toml with seq=1, or "NONE" if nothing to provide>
```

Write real files to disk under the project root. Do NOT output placeholder
text — write production-quality, MCU-accurate configurations.
"#
    )
}

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

fn cmd_run(root: &PathBuf, daemon: bool) -> forge_core::error::ForgeResult<()> {
    use forge_core::orchestrator::Orchestrator;

    if daemon {
        tracing::info!("daemon mode requested but not yet implemented, running foreground");
        println!("Daemon mode not yet implemented. Running in foreground...");
    }

    let mut orch = Orchestrator::new(root)?;
    println!("Orchestrator running. Press Ctrl+C to stop.");
    orch.run()?;
    Ok(())
}

fn cmd_status(root: &PathBuf, json: bool) -> forge_core::error::ForgeResult<()> {
    let nodes = collect_node_defs(root)?;
    if nodes.is_empty() {
        println!("No nodes found. Run 'forge init' to create a project.");
        return Ok(());
    }

    // Build tree: map parent → children, find roots
    let mut children_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut node_info: std::collections::HashMap<String, (String, String, String, u32)> =
        std::collections::HashMap::new(); // name → (cwd, status, summary, percent)

    for (name, cwd) in &nodes {
        let state_path = root.join(cwd).join(".forge/state.toml");
        let (status, summary, percent) = if let Some(state) =
            forge_core::safe_read_toml::<forge_core::protocol::NodeState>(&state_path)
        {
            (
                state.state.current.clone(),
                state.progress.summary.clone(),
                state.progress.percent_self_estimate,
            )
        } else {
            ("(not spawned)".into(), String::new(), 0u32)
        };

        // Read parent from node.toml
        let def_path = root.join(cwd).join("node.toml");
        let parent = if let Ok(def) = NodeDefinition::load(&def_path) {
            def.node.parent.clone()
        } else {
            String::new()
        };

        node_info.insert(name.clone(), (cwd.clone(), status, summary, percent));
        children_map.entry(parent).or_default().push(name.clone());
    }

    // Find roots (nodes whose parent is empty or not in our node list)
    let roots: Vec<&String> = children_map
        .keys()
        .filter(|p| p.is_empty() || !node_info.contains_key(*p))
        .flat_map(|p| children_map.get(p.as_str()).map(Vec::as_slice).unwrap_or(&[]))
        .collect();

    if json {
        let mut out = String::from("[");
        for (i, (name, cwd)) in nodes.iter().enumerate() {
            let (_, status, summary, percent) = node_info
                .get(name.as_str())
                .cloned()
                .unwrap_or_else(|| (cwd.clone(), "unknown".into(), String::new(), 0));
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"name\":\"{name}\",\"cwd\":\"{cwd}\",\"status\":\"{status}\",\"progress\":{percent},\"summary\":\"{summary}\"}}"
            ));
        }
        out.push(']');
        println!("{out}");
    } else {
        // Tree rendering
        for root in &roots {
            print_tree(root, "", true, &children_map, &node_info);
        }
        println!("--- {} node(s) ---", nodes.len());
    }

    Ok(())
}

/// Pretty-print a node tree.
fn print_tree(
    name: &str,
    prefix: &str,
    is_last: bool,
    children_map: &std::collections::HashMap<String, Vec<String>>,
    node_info: &std::collections::HashMap<String, (String, String, String, u32)>,
) {
    let connector = if is_last { "└── " } else { "├── " };
    let (_, status, summary, percent) = node_info
        .get(name)
        .cloned()
        .unwrap_or_else(|| ("?".into(), "unknown".into(), String::new(), 0));

    let icon = status_icon(&status);
    let progress = if percent > 0 && status != "delivered" && status != "dead" {
        format!(" [{percent}%]")
    } else {
        String::new()
    };
    let summary_str = if summary.is_empty() { String::new() } else { format!(" — {summary}") };

    println!("{prefix}{connector}{icon} {name} {status}{progress}{summary_str}");

    let child_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });
    if let Some(children) = children_map.get(name) {
        for (i, child) in children.iter().enumerate() {
            let child_is_last = i == children.len() - 1;
            print_tree(child, &child_prefix, child_is_last, children_map, node_info);
        }
    }
}

/// Return a Unicode icon for the given status.
fn status_icon(status: &str) -> &'static str {
    match status {
        "idle" => "○",
        "assigned" => "◐",
        "planning" => "◔",
        "implementing" => "◕",
        "blocked" => "⏳",
        "verifying" => "✓?",
        "delivered" => "✅",
        "dead" => "❌",
        _ => "❓",
    }
}

fn cmd_kill(
    _root: &PathBuf,
    node: &str,
    force: bool,
    cascade: bool,
) -> forge_core::error::ForgeResult<()> {
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
        return Err(forge_core::error::ForgeError::Config(format!("node '{name}' not found")));
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
        println!(
            "  Progress: {}% — {}",
            state.progress.percent_self_estimate, state.progress.summary
        );
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
