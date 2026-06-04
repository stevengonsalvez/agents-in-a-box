// ABOUTME: `ainb reflect` — reflect plugin lifecycle commands. Today this is
// `bootstrap`, the one-step installer for the reflect toolchain.
//
// Bootstrap is HYBRID by design (see the install design decision):
//   * AUTO  — the reflect-owned layer (`uv tool install reflect-kb[graph]`,
//     full GraphRAG stack) after one consent prompt.
//   * PRINT — system tools (bash>=4, coreutils, jq) that touch the OS / PATH /
//     sudo are never auto-run; we print annotated copy-paste commands.

use std::io::{self, Write};
use std::process::Command;

use anyhow::{Context, Result};
use clap::Subcommand;

use super::OutputFormat;
use crate::cli::deps::{self, DepKind, RealEnv};

/// `git+...#subdirectory=reflect-kb[graph]` — passed as ONE argv element to uv
/// (no shell), so the `[graph]` extra and `#` fragment need no quoting.
const REFLECT_KB_URL: &str =
    "git+https://github.com/stevengonsalvez/agents-in-a-box.git#subdirectory=reflect-kb[graph]";

#[derive(Subcommand)]
pub enum ReflectCommands {
    /// One-step install: auto-install reflect-kb[graph]; print missing system
    /// tools.
    Bootstrap(BootstrapArgs),
    /// Classified dependency check (reflect-focused; same engine as `ainb
    /// doctor`).
    Check,
}

#[derive(clap::Args)]
pub struct BootstrapArgs {
    /// Install the reflect-owned layer without prompting.
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Detect + print every command; install nothing.
    #[arg(long)]
    pub print_only: bool,
}

/// Entry point for `ainb reflect`.
pub async fn execute(cmd: ReflectCommands, format: OutputFormat) -> Result<()> {
    match cmd {
        ReflectCommands::Bootstrap(args) => bootstrap(args).await,
        ReflectCommands::Check => {
            let reports = deps::detect(&RealEnv);
            match format {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&reports)?),
                OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
                    deps::print_text(&reports)
                }
            }
            Ok(())
        }
    }
}

#[allow(clippy::unused_async)]
async fn bootstrap(args: BootstrapArgs) -> Result<()> {
    let env = RealEnv;
    let reports = deps::detect(&env);

    println!("reflect bootstrap — one-step install\n");
    println!("  {}", deps::reflect_summary_line(&reports));

    let reflect_kb_ok = reports.iter().any(|r| r.name == "reflect-kb" && r.satisfied);
    let uv_ok = reports.iter().any(|r| r.name == "uv" && r.satisfied);

    // --- The auto step: install the reflect-owned layer via uv. -------------
    if reflect_kb_ok {
        println!("\n\u{2713} reflect-kb already installed — nothing to auto-install.");
    } else if !uv_ok {
        println!(
            "\n\u{2717} `uv` is not installed (required to install reflect-kb).\n  \
             Install it yourself first (it modifies your shell profile):\n    \
             curl -LsSf https://astral.sh/uv/install.sh | sh\n  \
             Then re-run:  ainb reflect bootstrap"
        );
    } else {
        println!("\nReflect-owned layer to install (full GraphRAG stack — qmd + nano-graphrag):");
        println!("  uv tool install --force --upgrade {REFLECT_KB_URL}");
        println!("  (sentence-transformers/torch ~2GB — this can take a few minutes)");

        if args.print_only {
            println!("\n--print-only: nothing installed. Copy the command above to run it.");
        } else if args.yes || confirm("\nProceed with the install above?")? {
            run_reflect_kb_install()?;
            println!("\n\u{2713} reflect-kb installed.");
        } else {
            println!("Skipped the reflect-owned install.");
        }
    }

    // --- The print step: system tools we won't touch. -----------------------
    let sys_missing: Vec<&deps::DepReport> =
        reports.iter().filter(|r| !r.satisfied && r.kind == DepKind::System).collect();
    if sys_missing.is_empty() {
        println!("\n\u{2713} No system tools missing.");
    } else {
        println!(
            "\nSystem tools still needed — run these yourself (ainb won't touch your OS/PATH):"
        );
        for r in sys_missing {
            println!("  # {} — {}", r.name, r.why);
            println!("  {}", r.install_hint);
        }
    }

    println!("\nVerify the result any time with:  ainb doctor");
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N] ");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line).context("reading stdin")?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Run `uv tool install ... reflect-kb[graph]`, inheriting stdio so the user
/// sees uv's progress live.
fn run_reflect_kb_install() -> Result<()> {
    println!("\n$ uv tool install --force --upgrade {REFLECT_KB_URL}\n");
    let status = Command::new("uv")
        .args(["tool", "install", "--force", "--upgrade", REFLECT_KB_URL])
        .status()
        .context("failed to launch `uv` — is it on PATH?")?;
    if !status.success() {
        anyhow::bail!(
            "uv tool install failed. If it's the nano-graphrag dep chain (graspologic \u{2192} \
             numba \u{2192} llvmlite, py<3.10), install the base then inject nano-graphrag \
             without deps:\n  \
             uv tool install --force reflect-kb\n  \
             uv tool run --from reflect-kb pip install --no-deps nano-graphrag"
        );
    }
    Ok(())
}
