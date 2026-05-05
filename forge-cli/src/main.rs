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

    // ── Step 2: DeepSeek API key (project-local, stored in forge.toml [llm]) ──
    let deepseek_key: Option<String> = {
        // Try reading from existing forge.toml first (re-init scenario)
        let existing_key = resolve_deepseek_key(root);
        if let Some(ref key) = existing_key {
            println!("✓ DeepSeek API key found in forge.toml [llm] section.\n");
            Some(key.clone())
        } else {
            println!("💡 DeepSeek API key — used to auto-flesh-out skeleton files after init.");
            println!(
                "   The key is stored in forge.toml so different projects can use different keys."
            );
            println!("   Get one at https://platform.deepseek.com (cheapest, ~$0.01/project).");
            println!(
                "   Enter your key (leave blank to skip; you can add it to forge.toml later):"
            );
            print!("   DEEPSEEK_API_KEY: ");
            use std::io::Write;
            let _ = std::io::stdout().flush();
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).ok();
            let key = input.trim().to_string();
            if key.is_empty() {
                println!("  → Skipped. You can add it later in forge.toml → [llm] section.\n");
                None
            } else {
                println!(
                    "  → Will save key {} to forge.toml [llm] section.\n",
                    if key.len() > 6 {
                        format!("{}...{}", &key[..4], &key[key.len() - 4..])
                    } else {
                        "****".into()
                    }
                );
                Some(key)
            }
        }
    };

    // ── Step 3: Product description (§10) ──
    let product_description = collect_product_description();
    println!();

    // ── Step 4: Clarifying Q&A (DeepSeek-driven) ──
    let clarifications = if let Some(ref key) = deepseek_key {
        run_clarifying_questions(key, &product_description)
    } else {
        println!("💡 Skipping clarifying questions (no DeepSeek key configured).");
        println!("   The AI will design the module tree directly from your product description.\n");
        Vec::new()
    };

    // ── Step 5: MCU selection ──
    let mcu = select_option("Select target MCU family:", MCU_OPTIONS);
    println!();

    // ── Step 6: Toolchain ──
    let toolchain = select_option("Select build system / toolchain:", TOOLCHAIN_OPTIONS);
    println!();

    // ── Step 7: Feedback channel analysis (§10) ──
    let feedback_channels = select_feedback_channels();
    println!();

    // ── Step 8: Create project skeleton ──
    std::fs::create_dir_all(root)?;
    std::fs::create_dir_all(root.join("modules"))?;
    std::fs::create_dir_all(root.join(".forge"))?;

    // Build feedback channels list
    let fb_channels: Vec<String> = feedback_channels.iter().map(|(c, _pins)| c.clone()).collect();

    // Write forge.toml with product + feedback + llm sections
    let forge_toml = build_forge_toml(
        &mcu,
        &toolchain,
        deepseek_key.as_deref(),
        &product_description,
        &fb_channels,
    );
    let forge_toml_path = root.join("forge.toml");
    forge_core::atomic_write(&forge_toml_path, &forge_toml)?;
    println!("Created {}", forge_toml_path.display());

    // Create a single root domain agent as the starting point.
    // DeepSeek will design the full module tree during flesh-out.
    let domain_dir = root.join("modules/firmware");
    std::fs::create_dir_all(domain_dir.join(".forge"))?;
    std::fs::create_dir_all(domain_dir.join("shared"))?;
    let domain_toml = build_skeleton_node_toml("firmware");
    forge_core::atomic_write(&domain_dir.join("node.toml"), &domain_toml)?;
    println!(
        "Created {}/node.toml (root domain agent)",
        domain_dir.strip_prefix(root).unwrap_or(&domain_dir).display()
    );

    // Write .gitignore
    let gitignore = root.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
    let additions = "\n# CortexForge runtime artifacts\n.forge/eventbus.log\n**/.forge/stdout.log\n**/.forge/stderr.log\n**/.forge/inbox/processed/\n# IMPORTANT: if you set deepseek_api_key in forge.toml [llm], add forge.toml to .gitignore\n# or use DEEPSEEK_API_KEY env var instead (a warning will be shown).\n";
    if !existing.contains(".forge/eventbus.log") {
        std::fs::write(&gitignore, format!("{existing}{additions}"))?;
    }

    // ── Step 9: Generate flesh-out prompt + auto-flesh-out via DeepSeek ──
    let prompt_path = root.join(".forge/FLESH_OUT_PROMPT.md");
    let prompt = build_flesh_out_prompt(
        &project_name,
        &mcu,
        &toolchain,
        deepseek_key.is_some(),
        &product_description,
        &clarifications,
        &fb_channels,
    );
    std::fs::write(&prompt_path, &prompt)?;

    let mut auto_fleshed = false;
    if let Some(ref key) = deepseek_key {
        println!();
        println!("🤖 DeepSeek is designing your module tree from the product description...");
        println!("  (This takes ~15-30 seconds, cost ~$0.01)");
        match call_deepseek_flesh_out(key, &prompt) {
            Ok(response) => match apply_flesh_out_response(root, &response) {
                Ok(count) => {
                    println!("✓ DeepSeek designed and wrote {count} files successfully.");
                    auto_fleshed = true;
                }
                Err(e) => {
                    println!("⚠ Failed to parse DeepSeek response: {e}");
                    println!(
                        "  The prompt is saved at .forge/FLESH_OUT_PROMPT.md — you can process it manually."
                    );
                }
            },
            Err(e) => {
                println!("⚠ DeepSeek API call failed: {e}");
                println!(
                    "  The prompt is saved at .forge/FLESH_OUT_PROMPT.md — you can process it manually."
                );
            }
        }
    }

    println!();
    println!("Project '{project_name}' initialized successfully.");
    println!();
    if auto_fleshed {
        println!("Next steps:");
        println!("  1. Review the AI-designed module tree (node.toml, verify.sh, CLAUDE.md, etc.)");
        println!("  2. forge validate  — check everything is correct");
        println!("  3. forge run       — start the orchestrator");
    } else {
        println!("Next steps:");
        println!("  1. Review forge.toml and the root domain agent skeleton");
        println!("  2. Add your DeepSeek API key to forge.toml:");
        println!("       [llm]");
        println!("       deepseek_api_key = \"sk-...\"");
        println!("     Then re-run `forge init` to auto-design the module tree.");
        println!("  3. forge validate  — check everything is correct");
        println!("  4. forge run       — start the orchestrator");
    }
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

// ─── forge.toml builder ─────────────────────────────────────────────────

fn build_forge_toml(
    mcu: &str,
    toolchain: &str,
    deepseek_key: Option<&str>,
    product_desc: &str,
    fb_channels: &[String],
) -> String {
    let llm_section = if let Some(key) = deepseek_key {
        format!(
            "[llm]\n\
             deepseek_api_key = \"{key}\"\n"
        )
    } else {
        String::from(
            "# [llm]\n\
             # deepseek_api_key = \"sk-...\"\n",
        )
    };

    let product_section = format!(
        "# Product description — drives AI-based module tree design.\n\
         [product]\n\
         name = \"\"\n\
         description = \"\"\"\n{product_desc}\n\"\"\"\n\
         goal = \"\"\n\
         constraints = []\n"
    );

    let feedback_channel_entries: String = fb_channels
        .iter()
        .map(|c| {
            format!(
                "[[feedback.channels]]\n\
                 name = \"{c}\"\n\
                 type = \"{c}\"\n\
                 pins = []\n\
                 params = {{}}\n"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let feedback_section = format!(
        "# Runtime feedback channels for monitoring MCU execution.\n\
         [feedback]\n\
         telemetry_dir = \".forge/telemetry\"\n\n\
         [feedback.anomaly_detection]\n\
         window_samples = 20\n\
         deviation_threshold = 0.15\n\
         auto_fix_enabled = false\n\n\
         {feedback_channel_entries}"
    );

    format!(
        r#"# CortexForge project configuration
# Target: {mcu} / {toolchain}
# Generated by `forge init` — AI-designed from product description.

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

{product_section}
{feedback_section}
{llm_section}"#
    )
}

fn build_skeleton_node_toml(name: &str) -> String {
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
    _project_name: &str,
    mcu: &str,
    toolchain: &str,
    api_ready: bool,
    product_desc: &str,
    clarifications: &[(String, String)],
    fb_channels: &[String],
) -> String {
    let qa_summary: String = clarifications
        .iter()
        .map(|(q, a)| format!("  Q: {q}\n  A: {a}"))
        .collect::<Vec<_>>()
        .join("\n\n");

    let fb_summary = if fb_channels.is_empty() {
        "  (none configured)".into()
    } else {
        fb_channels.iter().map(|c| format!("  - {c}")).collect::<Vec<_>>().join("\n")
    };

    let api_note = if api_ready {
        "DeepSeek API key is configured. Processing this prompt automatically."
    } else {
        "⚠ No DeepSeek API key configured. The user will process this prompt manually."
    };

    format!(
        r#"You are DeepSeek, designing an embedded firmware project for CortexForge.
Your job is to DESIGN THE COMPLETE MODULE TREE from the product description below,
then create all node.toml files, verify.sh scripts, and per-module CLAUDE.md files.

## Product Description
{product_desc}

## Clarifying Q&A
{qa_summary}

## Hardware Context
- Target MCU: {mcu}
- Toolchain: {toolchain}
- Feedback channels (for runtime monitoring):
{fb_summary}
- {api_note}

## CortexForge Architecture Reference

### Module Tree Design Rules
1. Start with a root Domain Agent at `modules/firmware/` (depth=1)
2. Design sub-modules under `modules/firmware/submodules/<name>/` (depth=2+)
3. Use standard embedded layers: HAL (peripheral drivers) → BSP (board config) → MW (middleware) → APP (application logic)
4. Each module gets its own directory with node.toml, verify.sh, CLAUDE.md
5. Interface keys are UPPER_SNAKE_CASE (e.g. APB1_CLK, UART1_TX_PIN, TIM2_CH1_PWM)

### node.toml Format
```toml
[node]
name = "<unique-name>"           # e.g. "hal-clock", "bsp-uart", "app-controller"
role = "<role>"                  # domain | module | submodule
cwd = "modules/<name>"           # relative path from project root
parent = "<parent-node-name>"    # empty for root domain agent
depth = <n>                      # 1 = domain, 2 = module, 3+ = submodule

[children]
declared = ["child-a", "child-b"] # sub-modules this domain manages
spawn_strategy = "lazy"           # lazy (on-demand) | eager (at start)

[provides]
declared = ["INTERFACE_KEY_1", "INTERFACE_KEY_2"]
# UPPER_SNAKE_CASE identifiers other modules can depend on.

[budget]
max_tokens = 200_000
max_wallclock_sec = 1800
max_retries = 3
max_subprocess = 4

[runtime]
model = "claude-sonnet-4-6"
```

### verify.sh Requirements
- Exit code 0 = pass, non-zero = fail
- Compile the module, run tests, check memory/flash constraints
- Include telemetry output stubs for the configured feedback channels
- Example for {toolchain}:
```sh
#!/bin/sh
set -e
echo "[<module>] building..."
# arm-none-eabi-gcc -mcpu=cortex-m3 -mthumb -std=c11 -Wall -Werror -c src/*.c
echo "[<module>] running tests..."
echo "[<module>] verify PASS"
# Telemetry: outputs are parsed by the host collector for runtime monitoring
```

### CLAUDE.md Content
Document for each module:
1. Toolchain assumptions (compiler, flags, linker script)
2. MCU-specific details (register maps, clock tree, peripheral config)
3. Coding conventions used in this module
4. Dependencies on other modules (do NOT duplicate needs.toml)
5. Test methodology

### Dependency Protocol
- shared/needs.toml declares what a module NEEDS from other modules
- shared/provides.toml declares what a module PROVIDES (with seq=1)
- Orchestrator matches needs → provides and writes resolved values

## Your Task

1. **Design the module tree**: Based on the product description, decide what
   modules are needed. Use MCU datasheet knowledge for {mcu}
   (e.g., STM32F1: APB1 max 36MHz, APB2 max 72MHz, 4 timers, 2 SPI, 2 I2C, 3 USART).
   Write a `modules/firmware/node.toml` (domain agent) with `children.declared`
   listing all sub-module names.

2. **Create each sub-module**: For each sub-module, write its node.toml with
   concrete `provides.declared` interface keys, verify.sh, and CLAUDE.md.

3. **Define dependencies**: Write shared/needs.toml and shared/provides.toml
   for each module so the Orchestrator can resolve the dependency graph.

4. **Include telemetry**: The firmware should output runtime telemetry over
   the configured feedback channels. Include telemetry_declaration.toml and
   expectations.toml in the root domain agent's .forge/telemetry/ directory.

## Output Format

=== modules/firmware/node.toml ===
<complete domain agent node.toml with children.declared list>

=== modules/firmware/verify.sh ===
<complete verify.sh>

=== modules/firmware/CLAUDE.md ===
<complete CLAUDE.md>

=== modules/firmware/submodules/<name1>/node.toml ===
<complete node.toml>

... (repeat for each sub-module: node.toml, verify.sh, CLAUDE.md, needs.toml, provides.toml)

=== modules/firmware/.forge/telemetry/telemetry_declaration.toml ===
<complete telemetry declaration>

=== modules/firmware/.forge/telemetry/expectations.toml ===
<complete expectations with rules for runtime anomaly detection>

CRITICAL: Use exactly "=== path ===" markers. Design production-quality,
MCU-accurate configurations. Design the module tree from scratch based on
the product description — do NOT ask the user to specify layers.
"#
    )
}

// ─── Product-first init helpers ──────────────────────────────────────────

/// Collect a multi-line product description from the user.
/// Input ends when the user enters an empty line.
fn collect_product_description() -> String {
    println!("What product are you building?");
    println!("  Describe your product in detail — its purpose, what it does, any");
    println!("  constraints (power budget, flash size, real-time requirements, etc.).");
    println!("  Press Enter twice when done:\n");
    use std::io::Write;
    let mut lines: Vec<String> = Vec::new();
    loop {
        print!("  > ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            if lines.is_empty() {
                continue; // skip leading empty lines
            }
            break;
        }
        lines.push(trimmed);
    }
    if lines.is_empty() {
        println!("  → No description provided. Using placeholder.\n");
        String::from("An embedded MCU firmware project")
    } else {
        println!();
        lines.join("\n")
    }
}

/// Ask DeepSeek to analyze the product description and generate clarifying questions.
/// Returns user answers as (question, answer) pairs.
fn run_clarifying_questions(api_key: &str, product_desc: &str) -> Vec<(String, String)> {
    use std::io::Write;

    let analysis_prompt = format!(
        "You are helping to set up a CortexForge embedded firmware project.\n\n\
         ## Product Description\n{product_desc}\n\n\
         ## Your Task\n\
         Analyze this product description and generate 3-5 clarifying questions that will help
         design the embedded firmware module tree. Focus on:\n\
         1. Hardware peripherals needed (UART, SPI, I2C, timers, ADC, PWM, etc.)\n\
         2. Real-time constraints (interrupt latency, control loop frequency)\n\
         3. Power management requirements\n\
         4. Communication protocols (BLE, WiFi, CAN, RS485, etc.)\n\
         5. Storage / logging requirements\n\n\
         Return ONLY the questions, one per line. No numbering, no commentary.\n\
         Each line should be a clear, specific question."
    );

    let questions = match call_deepseek_flesh_out(api_key, &analysis_prompt) {
        Ok(response) => response
            .lines()
            .filter(|l| !l.trim().is_empty() && l.contains('?'))
            .map(|l| l.trim().to_string())
            .collect::<Vec<_>>(),
        Err(e) => {
            println!("  ⚠ Could not generate questions: {e}");
            return Vec::new();
        }
    };

    if questions.is_empty() {
        return Vec::new();
    }

    println!("Based on your product description, a few clarifying questions:\n");
    let mut answers = Vec::new();
    for q in &questions {
        println!("  {q}");
        print!("  > ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).ok();
        let answer = answer.trim().to_string();
        if !answer.is_empty() {
            answers.push((q.clone(), answer));
        }
    }
    println!();
    answers
}

/// Multi-select feedback channels with pin entry per channel.
fn select_feedback_channels() -> Vec<(String, String)> {
    use std::io::Write;

    println!("Runtime feedback channels — how will you monitor the MCU at runtime?");
    println!("  Select channels for runtime telemetry (comma-separated, e.g. '1,3,6'):\n");
    for (key, name, desc) in FEEDBACK_CHANNEL_OPTIONS {
        println!("  [{key}] {name} — {desc}");
    }
    print!("\n  channels (default: 1=UART): ");
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).ok();
    let choices: Vec<String> = input
        .trim()
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    let mut selected = Vec::new();
    if choices.is_empty() {
        // Default: UART
        println!("\n  Configuring UART channel:");
        print!("    Pins (e.g. PA9,PA10 for TX,RX): ");
        let _ = std::io::stdout().flush();
        let mut pins = String::new();
        std::io::stdin().read_line(&mut pins).ok();
        selected.push(("UART".into(), pins.trim().to_string()));
    } else {
        for choice in &choices {
            for (key, name, _desc) in FEEDBACK_CHANNEL_OPTIONS {
                if choice == &key.to_string() || choice == &name.to_lowercase() {
                    println!("\n  Configuring {name} channel:");
                    print!("    Pins: ");
                    let _ = std::io::stdout().flush();
                    let mut pins = String::new();
                    std::io::stdin().read_line(&mut pins).ok();
                    selected.push((name.to_string(), pins.trim().to_string()));
                }
            }
        }
    }
    selected
}

/// Feedback channel options for the init wizard.
const FEEDBACK_CHANNEL_OPTIONS: &[(char, &str, &str)] = &[
    ('1', "UART", "Serial debug output — most common. Connect via USB-UART adapter"),
    ('2', "SWO", "Serial Wire Output — ARM Cortex debug trace via SWD debugger"),
    ('3', "RTT", "Real-Time Transfer — SEGGER J-Link high-speed bidirectional channel"),
    ('4', "I2C", "I2C bus monitor — sniff sensor readings or status registers"),
    ('5', "SPI", "SPI bus monitor — capture display or flash traffic"),
    ('6', "GPIO", "GPIO toggles — timing markers, state changes, logic analyzer triggers"),
    ('7', "ADC", "Analog readback — monitor power rails, current sense, or analog signals"),
];

// ─── DeepSeek API key resolution ──────────────────────────────────────────

/// Resolve the DeepSeek API key with project-local priority:
/// 1. `forge.toml` → `[llm].deepseek_api_key` (per-project, no cross-project leakage)
/// 2. `DEEPSEEK_API_KEY` env var (fallback, with a clear warning)
///
/// This avoids polluting the user's global environment and lets different
/// projects use different API keys without conflict.
fn resolve_deepseek_key(root: &std::path::Path) -> Option<String> {
    // ── Priority 1: forge.toml [llm] section ──
    let forge_toml_path = root.join("forge.toml");
    if forge_toml_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&forge_toml_path) {
            if let Ok(config) = toml::from_str::<ForgeConfig>(&content) {
                if let Some(ref key) = config.llm.deepseek_api_key {
                    if !key.is_empty() {
                        return Some(key.clone());
                    }
                }
            }
        }
    }

    // ── Priority 2: DEEPSEEK_API_KEY env var (with warning) ──
    if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
        if !key.is_empty() {
            eprintln!();
            eprintln!("┌─────────────────────────────────────────────────────────────┐");
            eprintln!("│ ⚠  DeepSeek API Key 来源: 系统环境变量                       │");
            eprintln!("│                                                             │");
            eprintln!("│   因为我在 forge.toml 的 [llm] 中没有找到可用的 Key，         │");
            eprintln!("│   所以现在使用的是你系统环境变量中的 DEEPSEEK_API_KEY。        │");
            eprintln!("│                                                             │");
            eprintln!("│   请注意: 这意味着所有 CortexForge 项目都会共用这个 Key。      │");
            eprintln!("│   如果不同项目需要使用不同的 Key，请在 forge.toml 中添加:      │");
            eprintln!("│     [llm]                                                    │");
            eprintln!("│     deepseek_api_key = \"sk-...\"                              │");
            eprintln!("│                                                             │");
            eprintln!("│   以免造成额外花费时产生不快。                                │");
            eprintln!("└─────────────────────────────────────────────────────────────┘");
            eprintln!();
            return Some(key);
        }
    }

    None
}

// ─── DeepSeek API integration ─────────────────────────────────────────────

/// Call DeepSeek API to flesh out skeleton files. Returns the response content text.
fn call_deepseek_flesh_out(api_key: &str, prompt: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "model": "deepseek-chat",
        "messages": [
            {"role": "user", "content": prompt}
        ],
        "temperature": 0.3,
        "max_tokens": 8192
    });

    let child = std::process::Command::new("curl")
        .arg("-s")
        .arg("--max-time")
        .arg("120")
        .arg("https://api.deepseek.com/v1/chat/completions")
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-H")
        .arg(format!("Authorization: Bearer {api_key}"))
        .arg("-d")
        .arg(body.to_string())
        .output()
        .map_err(|e| format!("failed to run curl: {e}"))?;

    if !child.status.success() {
        let stderr = String::from_utf8_lossy(&child.stderr);
        return Err(format!("curl exited with error: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&child.stdout);
    let response: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("invalid JSON response: {e}"))?;

    // Check for API-level errors
    if let Some(err) = response.get("error") {
        let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown API error");
        return Err(format!("DeepSeek API error: {msg}"));
    }

    response["choices"][0]["message"]["content"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| "no content in DeepSeek response".to_string())
}

/// Parse the flesh-out response and write each section to its target file.
/// Returns the number of files written.
fn apply_flesh_out_response(
    root: &std::path::Path,
    response: &str,
) -> forge_core::error::ForgeResult<usize> {
    let mut count = 0usize;
    let mut current_path: Option<std::path::PathBuf> = None;
    let mut current_content = String::new();

    for line in response.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("=== ") && trimmed.ends_with(" ===") {
            // Flush previous file
            if let Some(ref path) = current_path {
                if !current_content.trim().is_empty() {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(path, current_content.trim())?;
                    count += 1;
                }
            }
            // Start new file
            let relative = trimmed
                .strip_prefix("=== ")
                .and_then(|s| s.strip_suffix(" ==="))
                .unwrap_or(trimmed);
            current_path = Some(root.join(relative));
            current_content = String::new();
        } else {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }

    // Flush last file
    if let Some(ref path) = current_path {
        if !current_content.trim().is_empty() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, current_content.trim())?;
            count += 1;
        }
    }

    Ok(count)
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
