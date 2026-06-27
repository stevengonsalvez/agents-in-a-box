// ABOUTME: Generates a single, idempotent, agent-specific install script from the
// setup catalog. The onboarding `G` key, `ainb init --script`, and `ainb doctor`
// all point users at this — instead of installing inline (consent-gated, can fail
// mid-wizard), we write a reviewable shell script the user runs once. A script
// the user runs themselves can safely bootstrap brew + run brew/curl/sudo.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::setup::catalog::{Detect, Install, Tier, catalog};
use crate::setup::detect::{Env, detect_dep};

/// The AI agent the generated script targets — decides which CLI + statusline +
/// hooks get wired (the shared deps are the same for all).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Copilot,
}

impl Agent {
    pub fn slug(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Copilot => "copilot",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Copilot => "GitHub Copilot",
        }
    }

    /// Parse from a CLI flag value.
    pub fn parse(s: &str) -> Option<Agent> {
        match s.trim().to_lowercase().as_str() {
            "claude" | "c" | "claude-code" | "claudecode" => Some(Agent::Claude),
            "codex" | "x" => Some(Agent::Codex),
            "copilot" | "p" | "gh-copilot" => Some(Agent::Copilot),
            _ => None,
        }
    }

    /// The catalog dep id of this agent's CLI.
    fn cli_id(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Copilot => "copilot",
        }
    }

    /// This agent's statusline dep id, if it has one.
    fn statusline_id(self) -> Option<&'static str> {
        match self {
            Agent::Claude => Some("claudecode-statusline"),
            Agent::Codex => Some("codex-statusline"),
            Agent::Copilot => None,
        }
    }
}

/// All AI-CLI dep ids — used to keep only the chosen agent's CLI in the script.
const AI_CLI_IDS: &[&str] = &["claude", "codex", "gemini", "copilot"];
/// All statusline dep ids — keep only the chosen agent's.
const STATUSLINE_IDS: &[&str] = &["claudecode-statusline", "codex-statusline"];

/// The binary to guard an install on (`command -v <bin> || install`), if any.
fn probe_bin(detect: &Detect) -> Option<&'static str> {
    match detect {
        Detect::Bin(n) => Some(n),
        Detect::BinAlt { primary, .. } => Some(primary),
        Detect::MinVersion { bin, .. } => Some(bin),
        Detect::CommandOk { cmd, .. } => Some(cmd),
        Detect::Custom("reflect-kb") => Some("reflect"),
        Detect::Custom("brew") => Some("brew"),
        Detect::Custom("ainb-self") => Some("ainb"),
        Detect::Custom("rtk-wired") => Some("rtk"),
        Detect::Custom(_) => None,
    }
}

/// The runnable install command for a dep, or `None` if it's manual/bundled/
/// multi-step (emitted as a comment instead). `ainb-hooks` is rewritten to the
/// chosen agent's notifyd target.
fn install_cmd(id: &str, install: &Install, agent: Agent) -> Option<String> {
    if id == "ainb-hooks" {
        return Some(format!("ainb notifyd install --{}", agent.slug()));
    }
    match install {
        Install::Brew(_)
        | Install::Npm(_)
        | Install::Uv(_)
        | Install::Cargo(_)
        | Install::Curl(_)
        | Install::Ainb(_)
        | Install::ClaudePlugin(_) => Some(install.hint()),
        // Multi-step / no automatic installer — comment only.
        Install::Toolkit | Install::Manual(_) | Install::BundledWith(_) => None,
    }
}

/// Whether a dep should appear, and whether ACTIVE (uncommented) or commented.
enum Inclusion {
    /// Active install line.
    Active,
    /// Commented-out (optional/suggested — uncomment to install).
    Commented,
    /// Skip entirely (other agents' CLI/statusline, or satisfied).
    Skip,
}

fn classify(id: &str, tier: Tier, agent: Agent) -> Inclusion {
    // Keep only the chosen agent's CLI; drop the others.
    if AI_CLI_IDS.contains(&id) {
        return if id == agent.cli_id() {
            Inclusion::Active
        } else {
            Inclusion::Skip
        };
    }
    // Keep only the chosen agent's statusline.
    if STATUSLINE_IDS.contains(&id) {
        return if Some(id) == agent.statusline_id() {
            Inclusion::Active
        } else {
            Inclusion::Skip
        };
    }
    // reflect's Claude Code plugin only applies to Claude.
    if id == "reflect-plugin" && agent != Agent::Claude {
        return Inclusion::Skip;
    }
    match tier {
        Tier::Required | Tier::Recommended => Inclusion::Active,
        Tier::Optional | Tier::Suggested => Inclusion::Commented,
    }
}

/// Build the install script for `agent`, including only deps unsatisfied on the
/// host (`env`). Pure (no IO) so it's unit-testable.
pub fn build_script(agent: Agent, env: &dyn Env) -> String {
    let mut required = String::new();
    let mut optional = String::new();
    let mut brew_missing = false;

    for topic in catalog() {
        let mut topic_header_written = (false, false); // (required, optional)
        for dep in &topic.deps {
            if !dep.applies_here() {
                continue;
            }
            // Only install what's missing.
            if detect_dep(dep, env).satisfied() {
                continue;
            }
            // brew is bootstrapped specially at the top.
            if dep.id == "brew" {
                brew_missing = true;
                continue;
            }
            match classify(dep.id, dep.tier, agent) {
                Inclusion::Skip => continue,
                Inclusion::Active => {
                    let (buf, flag) = (&mut required, &mut topic_header_written.0);
                    if !*flag {
                        buf.push_str(&format!("\n# --- {} ---\n", topic.label));
                        *flag = true;
                    }
                    buf.push_str(&dep_line(dep, agent, false));
                }
                Inclusion::Commented => {
                    let (buf, flag) = (&mut optional, &mut topic_header_written.1);
                    if !*flag {
                        buf.push_str(&format!("\n# --- {} (optional) ---\n", topic.label));
                        *flag = true;
                    }
                    buf.push_str(&dep_line(dep, agent, true));
                }
            }
        }
    }

    let mut s = String::new();
    s.push_str("#!/usr/bin/env bash\n");
    s.push_str(&format!(
        "# ainb installer for {} — generated by `ainb`.\n",
        agent.label()
    ));
    s.push_str("# Idempotent: safe to re-run. Installs what ainb needs that's missing.\n");
    s.push_str("set -euo pipefail\n\n");

    // Homebrew bootstrap — most install lines below use brew.
    s.push_str("# --- Homebrew (bootstrap if missing) ---\n");
    if brew_missing {
        s.push_str("if ! command -v brew >/dev/null 2>&1; then\n");
        s.push_str("  /bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"\n");
        s.push_str("fi\n");
    } else {
        s.push_str("# brew already installed.\n");
    }
    // Load brew into this script's environment regardless (covers a fresh install
    // and shells that haven't sourced shellenv yet).
    s.push_str("for p in /opt/homebrew/bin/brew /home/linuxbrew/.linuxbrew/bin/brew /usr/local/bin/brew; do\n");
    s.push_str("  [ -x \"$p\" ] && eval \"$(\"$p\" shellenv)\" && break\n");
    s.push_str("done\n");

    if required.is_empty() {
        s.push_str("\n# Nothing required/recommended is missing — you're set.\n");
    } else {
        s.push_str(&required);
    }

    if !optional.is_empty() {
        s.push_str("\n# ===== Optional / suggested — uncomment to install =====\n");
        s.push_str(&optional);
    }

    s.push_str("\necho \"\u{2713} ainb installer finished. Run 'ainb init --check' to verify.\"\n");
    s
}

/// One script line for a dep — guarded `command -v` when a probe binary exists,
/// the raw idempotent installer otherwise, or a `#` comment for manual deps.
fn dep_line(dep: &crate::setup::catalog::Dep, agent: Agent, commented: bool) -> String {
    let prefix = if commented { "# " } else { "" };
    match install_cmd(dep.id, &dep.install, agent) {
        Some(cmd) => match probe_bin(&dep.detect) {
            Some(bin) => format!(
                "{prefix}command -v {bin} >/dev/null 2>&1 || {cmd}   # {}\n",
                dep.why
            ),
            // No simple binary to guard on (ainb subcommands, plugins) — the
            // installer is itself idempotent.
            None => format!("{prefix}{cmd}   # {}\n", dep.why),
        },
        // Manual / multi-step — always a comment with the hint.
        None => format!("# {} — {}: {}\n", dep.name, dep.why, dep.install.hint()),
    }
}

/// Generate the script for `agent` and write it to
/// `~/.agents-in-a-box/installer/install-<agent>.sh` (executable). Returns the path.
pub fn generate(agent: Agent, env: &dyn Env) -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let dir = home.join(".agents-in-a-box/installer");
    fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let path = dir.join(format!("install-{}.sh", agent.slug()));
    fs::write(&path, build_script(agent, env))
        .with_context(|| format!("Failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockEnv {
        present: Vec<&'static str>,
    }
    impl Env for MockEnv {
        fn which(&self, name: &str) -> bool {
            self.present.contains(&name)
        }
        fn run(&self, _cmd: &str, _args: &[&str]) -> Option<String> {
            None
        }
    }

    #[test]
    fn agent_parse() {
        assert_eq!(Agent::parse("claude"), Some(Agent::Claude));
        assert_eq!(Agent::parse("x"), Some(Agent::Codex));
        assert_eq!(Agent::parse("copilot"), Some(Agent::Copilot));
        assert_eq!(Agent::parse("nope"), None);
    }

    #[test]
    fn empty_host_script_installs_required_guarded_and_picks_agent_cli() {
        // NOTE: brew/notifyd/statusline are Custom probes against the real FS,
        // so MockEnv can't force them missing — assert only on mock-controlled
        // (Bin/CommandOk) deps here.
        let env = MockEnv { present: vec![] };
        let s = build_script(Agent::Claude, &env);
        assert!(s.starts_with("#!/usr/bin/env bash"));
        assert!(s.contains("# --- Homebrew (bootstrap if missing) ---"));
        // required deps present + guarded
        assert!(s.contains("command -v git >/dev/null 2>&1 || brew install git"));
        assert!(s.contains("command -v tmux >/dev/null 2>&1 || brew install tmux"));
        // chosen agent's CLI in, others out
        assert!(s.contains("@anthropic-ai/claude-code"));
        assert!(!s.contains("@openai/codex"));
        assert!(!s.contains("@google/gemini-cli"));
    }

    #[test]
    fn codex_script_picks_codex_cli_not_claude() {
        let env = MockEnv { present: vec![] };
        let s = build_script(Agent::Codex, &env);
        assert!(s.contains("@openai/codex"));
        assert!(!s.contains("@anthropic-ai/claude-code"));
        // reflect Claude plugin is excluded for non-Claude agents (deterministic,
        // independent of host state).
        assert!(!s.contains("claude plugin install reflect@agents-in-a-box"));
    }

    #[test]
    fn satisfied_deps_are_skipped_and_brew_not_bootstrapped() {
        // Everything the script would install is already present.
        let env = MockEnv {
            present: vec![
                "brew",
                "git",
                "tmux",
                "jq",
                "bash",
                "timeout",
                "gtimeout",
                "node",
                "npm",
                "gh",
                "ainb",
                "uv",
                "reflect",
                "claude",
                "rtk",
                "headroom",
                "witr",
                "abtop",
                "alloy",
                "ccusage",
                "docker",
                "colima",
                "reattach-to-user-namespace",
                "bd",
                "gemini",
                "codex",
                "copilot",
            ],
        };
        let s = build_script(Agent::Claude, &env);
        assert!(s.contains("brew already installed."));
        assert!(!s.contains("brew install git"));
    }

    #[test]
    fn optional_deps_are_commented_not_active() {
        let env = MockEnv { present: vec![] };
        let s = build_script(Agent::Claude, &env);
        // headroom is optional -> commented
        let line = s.lines().find(|l| l.contains("headroom")).unwrap_or("");
        assert!(
            line.trim_start().starts_with('#'),
            "optional dep should be commented: {line}"
        );
    }
}
