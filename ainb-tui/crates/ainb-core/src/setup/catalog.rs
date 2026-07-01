// ABOUTME: Single source of truth for the ainb first-time-setup model — topics,
// dependencies, detection specs and install provisioners. Consumed by BOTH the
// TUI onboarding wizard and the `ainb init` CLI so the check, the installer and
// the docs never drift. Generalizes the older `cli/deps.rs` Consumer catalog.

use serde::Serialize;

pub use crate::cli::deps::Consumer;

/// Importance tier of a dependency within its topic. Drives whether onboarding
/// blocks (Required), warns (Recommended), surfaces (Optional) or merely
/// mentions (Suggested) a missing dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Blocks core functionality if missing — onboarding cannot complete.
    Required,
    /// Strongly advised; onboarding warns but can proceed.
    Recommended,
    /// Extends features; surfaced with an install hint, never warns.
    Optional,
    /// Nice-to-have ecosystem extras (marketplace skills, etc.).
    Suggested,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Required => "required",
            Tier::Recommended => "recommended",
            Tier::Optional => "optional",
            Tier::Suggested => "suggested",
        }
    }
}

/// Platforms a dependency applies to. Empty `platforms` slice = all platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    MacArm,
    MacIntel,
    Linux,
}

impl Platform {
    /// The platform this binary was built for.
    pub fn current() -> Option<Platform> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Some(Platform::MacArm)
        }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            Some(Platform::MacIntel)
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            Some(Platform::Linux)
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
        )))]
        {
            None
        }
    }
}

/// How to detect whether a dependency is already present/satisfied. Resolved by
/// `setup::detect`.
#[derive(Debug, Clone)]
pub enum Detect {
    /// `name` is resolvable on `$PATH`.
    Bin(&'static str),
    /// `primary` on PATH, or any of `alts` (e.g. `timeout` | `gtimeout`).
    BinAlt {
        primary: &'static str,
        alts: &'static [&'static str],
    },
    /// `bin` present AND `bin <probe_args>` reports major version >= `min_major`.
    MinVersion {
        bin: &'static str,
        probe_args: &'static [&'static str],
        min_major: u32,
    },
    /// `cmd args...` exits 0.
    CommandOk {
        cmd: &'static str,
        args: &'static [&'static str],
    },
    /// A named probe with bespoke logic resolved in `setup::detect`
    /// (e.g. "reflect-kb", "claudecode-statusline", "rtk-wired",
    /// "toolkit-deployed", "claude-plugin:<spec>"). Undetectable specs report
    /// `Unknown`.
    Custom(&'static str),
}

/// How to install/provision a dependency.
#[derive(Debug, Clone)]
pub enum Install {
    /// Homebrew package(s) / tap-qualified formula. Touches OS/PATH —
    /// consent-gated, never auto-run under a bare `--yes`.
    Brew(&'static [&'static str]),
    /// `npm install -g` package(s).
    Npm(&'static [&'static str]),
    /// `uv tool install <spec>`.
    Uv(&'static str),
    /// `curl <url> | sh`-style installer. Print-only (pipes a remote script to a
    /// shell) — shown, never auto-run.
    Curl(&'static str),
    /// `cargo install <crate>`.
    Cargo(&'static str),
    /// `ainb <args...>` — first-party provisioner (e.g. `reflect bootstrap
    /// --yes`, `rtk install`, `claudecode statusline install`).
    Ainb(&'static [&'static str]),
    /// `claude plugin install <spec>` (Claude Code marketplace plugin). When
    /// `marketplace` is set, the marketplace is added first via its **HTTPS**
    /// clone URL — the `owner/repo` shorthand defaults to SSH and fails with
    /// "Host key verification failed" on hosts without GitHub SSH keys.
    ClaudePlugin {
        spec: &'static str,
        marketplace: Option<&'static str>,
    },
    /// ainb-toolkit deploy via the skill manager: `ainb skill sync` reconciles
    /// the manifest's declared units onto disk **additively** — it never wipes
    /// existing config. Handled specially by the provisioner.
    Toolkit,
    /// Satisfied automatically once the named dependency is installed (e.g.
    /// bundled inside the ainb binary).
    BundledWith(&'static str),
    /// No installer — display this hint / URL only.
    Manual(&'static str),
}

impl Install {
    /// May ainb run this automatically after consent (vs. print-only)? brew and
    /// curl touch the OS / pipe remote scripts, so they are surfaced but only
    /// run on explicit per-item consent — never under a bare headless `--yes`.
    pub fn auto_runnable(&self) -> bool {
        matches!(
            self,
            Install::Npm(_)
                | Install::Uv(_)
                | Install::Cargo(_)
                | Install::Ainb(_)
                | Install::ClaudePlugin { .. }
                | Install::Toolkit
        )
    }

    /// A copy-paste install command string for display.
    pub fn hint(&self) -> String {
        match self {
            Install::Brew(p) => format!("brew install {}", p.join(" ")),
            Install::Npm(p) => format!("npm install -g {}", p.join(" ")),
            Install::Uv(s) => format!("uv tool install {s}"),
            Install::Curl(url) => format!("curl -LsSf {url} | sh"),
            Install::Cargo(c) => format!("cargo install {c}"),
            Install::Ainb(a) => format!("ainb {}", a.join(" ")),
            Install::ClaudePlugin {
                spec,
                marketplace: Some(url),
            } => format!(
                "claude plugin marketplace add {url} 2>/dev/null || true\nclaude plugin install {spec}"
            ),
            Install::ClaudePlugin {
                spec,
                marketplace: None,
            } => {
                format!("claude plugin install {spec}")
            }
            Install::Toolkit => "ainb skill sync".to_string(),
            Install::BundledWith(x) => format!("(bundled with {x})"),
            Install::Manual(h) => h.to_string(),
        }
    }
}

/// A single dependency: what it is, how to detect it, how to install it.
#[derive(Debug, Clone)]
pub struct Dep {
    /// Stable identifier (kebab-case), unique across the catalog.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// One-line "what it's for".
    pub why: &'static str,
    pub tier: Tier,
    pub detect: Detect,
    pub install: Install,
    /// Cross-cutting features that consume this dep (a dep may serve several).
    /// Kept for the reflect/statusline grouping logic that predates topics.
    pub consumers: &'static [Consumer],
    /// Platforms this dep applies to. Empty = all platforms.
    pub platforms: &'static [Platform],
}

impl Dep {
    /// Does this dependency apply to the current platform?
    pub fn applies_here(&self) -> bool {
        if self.platforms.is_empty() {
            return true;
        }
        match Platform::current() {
            Some(p) => self.platforms.contains(&p),
            None => true, // unknown platform: don't hide anything
        }
    }
}

/// A topic groups related dependencies for display + provisioning order.
#[derive(Debug, Clone)]
pub struct Topic {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub deps: Vec<Dep>,
}

// Construction helpers keep the catalog literal readable.
const fn dep(
    id: &'static str,
    name: &'static str,
    why: &'static str,
    tier: Tier,
    detect: Detect,
    install: Install,
    consumers: &'static [Consumer],
    platforms: &'static [Platform],
) -> Dep {
    Dep {
        id,
        name,
        why,
        tier,
        detect,
        install,
        consumers,
        platforms,
    }
}

/// The full setup catalog: every topic and its dependencies. This is the single
/// list the TUI wizard renders and the CLI drives.
pub fn catalog() -> Vec<Topic> {
    use Consumer::*;
    use Detect::*;
    use Install::*;
    use Platform::*;
    use Tier::*;

    vec![
        Topic {
            id: "core",
            label: "Core Tools",
            description: "Required for ainb to function",
            deps: vec![
                dep(
                    "brew",
                    "Homebrew",
                    "package manager used to install ainb + most tools here",
                    Recommended,
                    Custom("brew"),
                    Manual(
                        "/bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"",
                    ),
                    &[Consumer::Core],
                    &[],
                ),
                dep(
                    "git",
                    "git",
                    "branch + worktree/session git ops",
                    Required,
                    Bin("git"),
                    Brew(&["git"]),
                    &[Consumer::Core, Statusline],
                    &[],
                ),
                dep(
                    "tmux",
                    "tmux",
                    "session multiplexing for agent panes",
                    Required,
                    Bin("tmux"),
                    Brew(&["tmux"]),
                    &[Consumer::Core],
                    &[],
                ),
                dep(
                    "jq",
                    "jq",
                    "parse statusline JSON + reflect error badge",
                    Recommended,
                    Bin("jq"),
                    Brew(&["jq"]),
                    &[Statusline, Reflect],
                    &[],
                ),
                dep(
                    "bash",
                    "bash (>=4)",
                    "bash >=4 for the reflect timeline dashboard (macOS ships 3.2)",
                    Recommended,
                    MinVersion {
                        bin: "bash",
                        probe_args: &["-c", "echo ${BASH_VERSINFO[0]:-0}"],
                        min_major: 4,
                    },
                    Brew(&["bash"]),
                    &[Statusline],
                    &[],
                ),
                dep(
                    "timeout",
                    "timeout",
                    "guards every statusline shell-out",
                    Recommended,
                    BinAlt {
                        primary: "timeout",
                        alts: &["gtimeout"],
                    },
                    Brew(&["coreutils"]),
                    &[Statusline],
                    &[],
                ),
                dep(
                    "reattach",
                    "reattach-to-user-namespace",
                    "say/audio, notifications, keychain inside tmux",
                    Optional,
                    Bin("reattach-to-user-namespace"),
                    Brew(&["reattach-to-user-namespace"]),
                    &[],
                    &[MacArm, MacIntel],
                ),
            ],
        },
        // Runtimes the rest of the catalog installs through. Brew-installed so
        // the script's `brew shellenv` eval puts them on PATH for the npm /
        // cargo / uv lines that follow. Ordered before AI CLIs and plugin
        // binaries on purpose.
        Topic {
            id: "toolchains",
            label: "Language toolchains",
            description: "Runtimes the agent CLIs + tools install through",
            deps: vec![
                dep(
                    "node",
                    "node (>=20)",
                    "Node + npm runtime the agent CLIs install through (Copilot needs 22+)",
                    Recommended,
                    MinVersion {
                        bin: "node",
                        probe_args: &["-p", "process.versions.node.split('.')[0]"],
                        min_major: 20,
                    },
                    Brew(&["node"]),
                    &[Consumer::Core],
                    &[],
                ),
                dep(
                    "cargo",
                    "cargo (Rust)",
                    "builds cargo-installed plugin tools (abtop)",
                    Recommended,
                    Bin("cargo"),
                    Brew(&["rust"]),
                    &[],
                    &[],
                ),
            ],
        },
        Topic {
            id: "ai-clis",
            label: "AI CLIs",
            description: "Coding agents ainb drives (need at least one)",
            deps: vec![
                dep(
                    "claude",
                    "Claude CLI",
                    "Anthropic's Claude Code CLI",
                    Recommended,
                    CommandOk {
                        cmd: "claude",
                        args: &["--version"],
                    },
                    Npm(&["@anthropic-ai/claude-code"]),
                    &[Consumer::Core, Reflect],
                    &[],
                ),
                dep(
                    "codex",
                    "Codex",
                    "OpenAI's Codex CLI",
                    Optional,
                    Bin("codex"),
                    Npm(&["@openai/codex"]),
                    &[],
                    &[],
                ),
                dep(
                    "gemini",
                    "Gemini CLI",
                    "Google's Gemini CLI",
                    Optional,
                    Bin("gemini"),
                    Npm(&["@google/gemini-cli"]),
                    &[],
                    &[],
                ),
                dep(
                    "copilot",
                    "GitHub Copilot CLI",
                    "GitHub's agentic CLI (Node 22+)",
                    Optional,
                    Bin("copilot"),
                    Npm(&["@github/copilot"]),
                    &[],
                    &[],
                ),
            ],
        },
        // ponytail: container runtime topic (docker/colima) removed from the
        // setup surface — container/Boss sessions aren't wired up cleanly yet.
        // Re-add this Topic when the Docker session path is restored.
        Topic {
            id: "github",
            label: "GitHub",
            description: "PR / issue operations",
            deps: vec![
                dep(
                    "gh",
                    "GitHub CLI",
                    "agents shell out to gh for PR/issue work",
                    Recommended,
                    Bin("gh"),
                    Brew(&["gh"]),
                    &[],
                    &[],
                ),
                dep(
                    "gh-auth",
                    "gh auth",
                    "logged in (gh auth status) so agents can open PRs/issues",
                    Recommended,
                    Custom("gh-authed"),
                    Manual("gh auth login"),
                    &[],
                    &[],
                ),
            ],
        },
        Topic {
            id: "ainb",
            label: "agents-in-a-box",
            description: "The ainb binary + bundled plugins",
            deps: vec![dep(
                "ainb",
                "ainb",
                "the TUI/CLI + bundled burndown/session-reader/hangar/notifyd/mcp",
                Required,
                Custom("ainb-self"),
                Manual("brew tap stevengonsalvez/agents-in-a-box && brew install ainb"),
                &[Consumer::Core],
                &[],
            )],
        },
        Topic {
            id: "reflect",
            label: "Reflect (long-term memory)",
            description: "GraphRAG + BM25 recall across sessions",
            deps: vec![
                dep(
                    "uv",
                    "uv",
                    "runs reflect hooks + installs the reflect CLI / headroom",
                    Recommended,
                    Bin("uv"),
                    // brew (not the astral curl installer) so the script's brew
                    // shellenv puts uv on PATH for the `uv tool install` lines.
                    Brew(&["uv"]),
                    &[Reflect],
                    &[],
                ),
                dep(
                    "reflect-kb",
                    "reflect",
                    "the reflect CLI: recall/ingest/search (BM25 + GraphRAG)",
                    Recommended,
                    Custom("reflect-kb"),
                    Ainb(&["reflect", "bootstrap", "--yes"]),
                    &[Reflect],
                    &[],
                ),
                dep(
                    "reflect-plugin",
                    "reflect plugin",
                    "Claude Code plugin: SessionStart recall + PreCompact capture hooks (Codex: reflect codex adapter)",
                    Recommended,
                    Custom("claude-plugin:reflect"),
                    ClaudePlugin {
                        spec: "reflect@ainb-reflect-memory",
                        marketplace: Some(
                            "https://github.com/stevengonsalvez/ainb-reflect-memory.git",
                        ),
                    },
                    &[Reflect],
                    &[],
                ),
            ],
        },
        Topic {
            id: "toolkit",
            label: "ainb-toolkit",
            description: "Skills + agents deployed to your tool homes",
            deps: vec![dep(
                "toolkit",
                "ainb-toolkit",
                "91 skills / 37 agents for claude/codex/copilot",
                Recommended,
                Custom("toolkit-deployed"),
                Toolkit,
                &[],
                &[],
            )],
        },
        Topic {
            id: "plugin-bins",
            label: "Plugin binaries",
            description: "External binaries the TUI plugins wrap",
            deps: vec![
                dep(
                    "witr",
                    "witr",
                    "process causality tracing (witr plugin)",
                    Optional,
                    Bin("witr"),
                    Brew(&["witr"]),
                    &[],
                    &[],
                ),
                dep(
                    "abtop",
                    "abtop",
                    "top-for-agents snapshot (abtop plugin)",
                    Optional,
                    Bin("abtop"),
                    Cargo("abtop"),
                    &[],
                    &[],
                ),
            ],
        },
        Topic {
            id: "token-opt",
            label: "Token optimization",
            description: "Compress output / proxy — opt-in token savings",
            deps: vec![
                dep(
                    "rtk",
                    "rtk",
                    "compress Bash/test/diff output via PreToolUse hook",
                    Optional,
                    Custom("rtk-wired"),
                    Ainb(&["rtk", "install"]),
                    &[TokenOpt],
                    &[],
                ),
                dep(
                    "headroom",
                    "headroom",
                    "per-session LLM-API compression proxy",
                    Optional,
                    Bin("headroom"),
                    Uv("headroom-ai[proxy]"),
                    &[TokenOpt],
                    &[],
                ),
            ],
        },
        Topic {
            id: "telemetry",
            label: "Telemetry",
            description: "OTLP export to Grafana Cloud",
            deps: vec![dep(
                "alloy",
                "Grafana Alloy",
                "forwards local OTLP telemetry (run `ainb otel setup`)",
                Optional,
                Bin("alloy"),
                Brew(&["grafana/grafana/alloy"]),
                &[Otel],
                &[],
            )],
        },
        Topic {
            id: "statusline",
            label: "Statusline",
            description: "Live burn / cost / context in the prompt (Claude Code + Codex)",
            deps: vec![
                dep(
                    "claudecode-statusline",
                    "Claude Code statusline",
                    "5h burn + 7d window + cost in the Claude Code prompt bar",
                    Recommended,
                    Custom("claudecode-statusline"),
                    // Idempotent wire into ~/.claude/settings.json — runnable from
                    // the deps screen (`i`) and the generated install script.
                    Ainb(&["claudecode", "statusline", "--install"]),
                    &[],
                    &[],
                ),
                dep(
                    "codex-statusline",
                    "Codex statusline",
                    "5h + weekly OAuth quota in the Codex prompt bar",
                    Optional,
                    Custom("codex-statusline"),
                    Manual("automatic via `ainb codex statusline` once you're logged into Codex"),
                    &[],
                    &[],
                ),
                dep(
                    "ccusage",
                    "ccusage",
                    "5h + weekly rate-limit bars (non-OAuth plans)",
                    Suggested,
                    Bin("ccusage"),
                    Npm(&["ccusage"]),
                    &[Usage],
                    &[],
                ),
            ],
        },
        Topic {
            id: "hooks",
            label: "Hooks & notifications",
            description: "Inbox screen + session-state badges (Claude Code + Codex + Copilot)",
            deps: vec![dep(
                "ainb-hooks",
                "ainb-hooks (notifyd)",
                "lifecycle events powering the Inbox + badges",
                Recommended,
                Custom("notifyd-installed"),
                // `--all` covers Claude + Codex + Copilot. The notifyd installer
                // bakes plugins/ainb-hooks/{codex,copilot}/hooks.json for the
                // non-Claude agents; dropping a target silently loses their Inbox.
                Ainb(&["notifyd", "install", "--all"]),
                &[],
                &[],
            )],
        },
        Topic {
            id: "suggested-plugins",
            label: "Suggested plugins",
            description: "Claude: claude plugin install <name>@agents-in-a-box · Codex/Copilot: skill plugins (ainb-fleet, illustration) via ainb skill install <uri> --targets codex,copilot; caveman-stats is Claude-only",
            deps: vec![
                dep(
                    "ainb-fleet",
                    "ainb-fleet",
                    "multi-session fleet orchestration skills",
                    Suggested,
                    Custom("claude-plugin:ainb-fleet"),
                    ClaudePlugin {
                        spec: "ainb-fleet@agents-in-a-box",
                        marketplace: Some("https://github.com/stevengonsalvez/agents-in-a-box.git"),
                    },
                    &[],
                    &[],
                ),
                dep(
                    "caveman",
                    "caveman",
                    "ultra-compressed token-saving response mode",
                    Suggested,
                    Custom("claude-plugin:caveman"),
                    // External marketplace (not stevengonsalvez/*) — no known HTTPS
                    // URL to pin, so leave the bare install; user adds it themselves.
                    ClaudePlugin {
                        spec: "caveman@caveman",
                        marketplace: None,
                    },
                    &[],
                    &[],
                ),
                dep(
                    "caveman-stats",
                    "caveman-stats",
                    "caveman token-savings tracker + compaction survival",
                    Suggested,
                    Custom("claude-plugin:caveman-stats"),
                    ClaudePlugin {
                        spec: "caveman-stats@agents-in-a-box",
                        marketplace: Some("https://github.com/stevengonsalvez/agents-in-a-box.git"),
                    },
                    &[],
                    &[],
                ),
                dep(
                    "illustration",
                    "illustration",
                    "mascot-driven brand illustration workflow",
                    Suggested,
                    Custom("claude-plugin:illustration"),
                    ClaudePlugin {
                        spec: "illustration@agents-in-a-box",
                        marketplace: Some("https://github.com/stevengonsalvez/agents-in-a-box.git"),
                    },
                    &[],
                    &[],
                ),
                dep(
                    "bd",
                    "beads (bd)",
                    "git-backed issue tracker agents read/write; powers the beads segment",
                    Suggested,
                    Bin("bd"),
                    Manual(
                        "brew install beads (or npm i -g @beads/bd), then: bd setup claude · bd setup codex",
                    ),
                    &[Beads],
                    &[],
                ),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_nonempty_and_ids_unique() {
        let topics = catalog();
        assert!(!topics.is_empty());
        let mut ids = std::collections::HashSet::new();
        for t in &topics {
            assert!(!t.deps.is_empty(), "topic {} has no deps", t.id);
            for d in &t.deps {
                assert!(ids.insert(d.id), "duplicate dep id: {}", d.id);
            }
        }
    }

    #[test]
    fn core_has_required_git_and_tmux() {
        let topics = catalog();
        let core = topics.iter().find(|t| t.id == "core").unwrap();
        let git = core.deps.iter().find(|d| d.id == "git").unwrap();
        let tmux = core.deps.iter().find(|d| d.id == "tmux").unwrap();
        assert_eq!(git.tier, Tier::Required);
        assert_eq!(tmux.tier, Tier::Required);
    }

    #[test]
    fn gemini_install_hint_is_google_not_anthropic() {
        let topics = catalog();
        let gemini = topics.iter().flat_map(|t| &t.deps).find(|d| d.id == "gemini").unwrap();
        assert_eq!(gemini.install.hint(), "npm install -g @google/gemini-cli");
    }

    #[test]
    fn suggested_plugins_offer_codex_and_copilot() {
        // F2 guard: the skill-plugin topic must surface the cross-harness
        // install path for BOTH Codex and Copilot, not Claude-only.
        let topics = catalog();
        let sp = topics.iter().find(|t| t.id == "suggested-plugins").unwrap();
        assert!(
            sp.description.contains("codex"),
            "must mention codex: {}",
            sp.description
        );
        assert!(
            sp.description.contains("copilot"),
            "must mention copilot: {}",
            sp.description
        );
    }

    #[test]
    fn hooks_install_covers_every_agent() {
        // F1 guard: onboarding must install ainb-hooks for ALL agents in one
        // shot. `--all` is the only spec that covers every agent; `--claude
        // --codex` (the prior regression) or any single-agent subset would
        // silently drop an Inbox, so assert `--all` exactly rather than a weak
        // "mentions copilot" that a Claude-dropping subset could also satisfy.
        let topics = catalog();
        let hooks = topics.iter().flat_map(|t| &t.deps).find(|d| d.id == "ainb-hooks").unwrap();
        let hint = hooks.install.hint();
        assert!(
            hint.contains("--all"),
            "hooks install must use --all, got: {hint}"
        );
    }

    #[test]
    fn brew_and_curl_are_not_auto_runnable() {
        assert!(!Install::Brew(&["git"]).auto_runnable());
        assert!(!Install::Curl("x").auto_runnable());
        assert!(Install::Uv("x").auto_runnable());
        assert!(Install::Ainb(&["reflect", "bootstrap"]).auto_runnable());
    }
}
