// ABOUTME: Shared dependency catalog + on-system detection for the reflect /
// statusline toolchain. One source of truth consumed by `ainb doctor`,
// `ainb reflect bootstrap`, and the `ainb init` setup summary so the
// "what's installed · what needs it · how to install it" logic never drifts
// between the check, the installer, and the docs.
//
// Detection is injected via the `Env` trait so unit tests can feed a canned
// machine (which() answers + run() stdout) without touching the real PATH.

use std::process::Command;

use serde::Serialize;

/// Canonical install command for the reflect-kb CLI (full GraphRAG stack).
/// The `[graph]` extra pulls qmd (BM25) + nano-graphrag (GraphRAG) +
/// sentence-transformers. `--force --upgrade` makes re-runs idempotent.
pub const REFLECT_KB_INSTALL: &str = "uv tool install --force --upgrade \
'git+https://github.com/stevengonsalvez/agents-in-a-box.git#subdirectory=reflect-kb[graph]'";

/// Which feature needs a given dependency. A dep may have several consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Consumer {
    /// Core ainb / Claude Code session basics.
    Core,
    /// The reflect plugin + `reflect` CLI (recall, ingest, drain, error badge).
    Reflect,
    /// The Claude Code statusline renderer (segments + 4-row dashboard).
    Statusline,
    /// The beads `bd` issue-tracker segment.
    Beads,
    /// ccusage-backed 5h / weekly usage bars.
    Usage,
}

impl Consumer {
    pub fn label(self) -> &'static str {
        match self {
            Consumer::Core => "core",
            Consumer::Reflect => "reflect",
            Consumer::Statusline => "statusline",
            Consumer::Beads => "beads",
            Consumer::Usage => "usage",
        }
    }

    /// Stable display order for grouped reports.
    pub fn order() -> &'static [Consumer] {
        &[
            Consumer::Core,
            Consumer::Reflect,
            Consumer::Statusline,
            Consumer::Beads,
            Consumer::Usage,
        ]
    }
}

/// How a dependency is installed — decides whether `ainb reflect bootstrap`
/// may install it automatically or must only print the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DepKind {
    /// OS-level package (brew/apt) or anything that touches PATH / sudo.
    /// Bootstrap PRINTS the command; it never runs it.
    System,
    /// Reflect-owned, installed through `uv`. Bootstrap MAY auto-install
    /// after consent.
    ReflectOwned,
}

/// A dependency the toolchain expects, plus how to detect and install it.
#[derive(Debug, Clone, Copy)]
pub struct DepSpec {
    pub name: &'static str,
    pub kind: DepKind,
    pub consumers: &'static [Consumer],
    /// Hard requirement for its consumer (vs. nice-to-have).
    pub required: bool,
    /// One-line "what it's for".
    pub why: &'static str,
    /// Copy-paste install command(s), annotated.
    pub install_hint: &'static str,
}

/// Detection outcome for one dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum DepState {
    /// Present and satisfies any version/variant requirement.
    Ok(Option<String>),
    /// Satisfied by an alternative binary (e.g. `gtimeout` for `timeout`).
    Alt(String),
    /// Present but too old (e.g. bash 3.2 < 4).
    TooOld(String),
    /// Not found.
    Missing,
}

impl DepState {
    /// True when the dependency is usable (present, or present via
    /// alternative).
    pub fn satisfied(&self) -> bool {
        matches!(self, DepState::Ok(_) | DepState::Alt(_))
    }
}

/// A dependency spec joined with its detected state, ready to render/serialize.
#[derive(Debug, Clone, Serialize)]
pub struct DepReport {
    pub name: &'static str,
    pub kind: DepKind,
    pub consumers: Vec<Consumer>,
    pub required: bool,
    pub why: &'static str,
    pub install_hint: &'static str,
    pub state: DepState,
    /// Convenience flag for JSON consumers / templates.
    pub satisfied: bool,
}

impl DepReport {
    /// A missing-or-too-old REQUIRED dep — the things that actually break a
    /// feature (as opposed to optional niceties).
    pub fn is_blocking(&self) -> bool {
        self.required && !self.satisfied
    }
}

/// Abstraction over the host so detection is testable.
pub trait Env {
    /// Is `name` resolvable on `$PATH`?
    fn which(&self, name: &str) -> bool;
    /// Run `cmd args...`; return trimmed stdout on success, `None` otherwise.
    fn run(&self, cmd: &str, args: &[&str]) -> Option<String>;
}

/// Real host: `which` crate for PATH lookup, `std::process::Command` for
/// probes.
pub struct RealEnv;

impl Env for RealEnv {
    fn which(&self, name: &str) -> bool {
        which::which(name).is_ok()
    }

    fn run(&self, cmd: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(cmd).args(args).output().ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            None
        }
    }
}

/// The full dependency catalog, classified by consumer + install kind.
pub fn catalog() -> &'static [DepSpec] {
    use Consumer::*;
    use DepKind::*;
    &[
        DepSpec {
            name: "git",
            kind: System,
            consumers: &[Core, Statusline],
            required: true,
            why: "branch segment + worktree/session git ops",
            install_hint: "brew install git        # macOS  ·  apt install git  (Debian/Ubuntu)",
        },
        DepSpec {
            name: "tmux",
            kind: System,
            consumers: &[Core],
            required: true,
            why: "session multiplexing for agent panes",
            install_hint: "brew install tmux       # macOS  ·  apt install tmux",
        },
        DepSpec {
            name: "jq",
            kind: System,
            consumers: &[Statusline, Reflect],
            required: true,
            why: "parse statusline JSON + count reflect errors (badge fast-path)",
            install_hint: "brew install jq         # macOS  ·  apt install jq",
        },
        DepSpec {
            name: "bash",
            kind: System,
            consumers: &[Statusline],
            required: true,
            why: "bash >=4 for the 4-row reflect timeline dashboard (macOS ships 3.2)",
            install_hint: "brew install bash       # then put $(brew --prefix)/bin BEFORE /bin in PATH",
        },
        DepSpec {
            name: "timeout",
            kind: System,
            consumers: &[Statusline],
            required: true,
            why: "guards every statusline shell-out (git/jq/bd/ccusage)",
            install_hint: "brew install coreutils  # gtimeout; add $(brew --prefix coreutils)/libexec/gnubin to PATH for bare `timeout`",
        },
        DepSpec {
            name: "uv",
            kind: ReflectOwned,
            consumers: &[Reflect],
            required: true,
            why: "runs every reflect hook and installs the reflect-kb CLI",
            install_hint: "curl -LsSf https://astral.sh/uv/install.sh | sh   # the uv installer",
        },
        DepSpec {
            name: "reflect-kb",
            kind: ReflectOwned,
            consumers: &[Reflect],
            required: true,
            why: "the `reflect` CLI: recall/ingest/search + qmd (BM25) + nano-graphrag (GraphRAG)",
            install_hint: REFLECT_KB_INSTALL,
        },
        DepSpec {
            name: "ccusage",
            kind: System,
            consumers: &[Usage],
            required: false,
            why: "5h + weekly rate-limit bars (skippable if your plan feeds rate_limits on stdin)",
            install_hint: "npm i -g ccusage",
        },
        DepSpec {
            name: "bd",
            kind: System,
            consumers: &[Beads],
            required: false,
            why: "beads ready-issue count segment",
            install_hint: "# bd (beads): install per your beads setup",
        },
        DepSpec {
            name: "claude",
            kind: System,
            consumers: &[Core, Reflect],
            required: false,
            why: "the Claude Code CLI the reflect drainer shells out to",
            install_hint: "# install Claude Code: https://claude.com/claude-code",
        },
    ]
}

/// Probe a single dependency's state on the given host.
fn detect_one(spec: &DepSpec, env: &dyn Env) -> DepState {
    match spec.name {
        // bash needs >=4 (associative arrays in the timeline dashboard).
        // The statusline runs via `#!/usr/bin/env bash`, so the PATH bash is
        // what matters — probe it, not /bin/bash.
        "bash" => {
            if !env.which("bash") {
                return DepState::Missing;
            }
            match env.run("bash", &["-c", "echo ${BASH_VERSINFO[0]:-0}"]) {
                Some(v) => {
                    let major: u32 = v.trim().parse().unwrap_or(0);
                    if major >= 4 {
                        DepState::Ok(Some(format!("bash {major}.x")))
                    } else {
                        DepState::TooOld(format!("bash {major}.x (need >=4)"))
                    }
                }
                // bash present but version probe failed — assume usable.
                None => DepState::Ok(None),
            }
        }
        // timeout (GNU coreutils) or its `g`-prefixed Homebrew alias.
        "timeout" => {
            if env.which("timeout") {
                DepState::Ok(None)
            } else if env.which("gtimeout") {
                DepState::Alt("gtimeout (coreutils, not on PATH as `timeout`)".to_string())
            } else {
                DepState::Missing
            }
        }
        // reflect-kb is "installed" if the `reflect` binary is on PATH
        // (uv tool install) OR system python3 can import it (pip install).
        "reflect-kb" => {
            if env.which("reflect") {
                DepState::Ok(Some("reflect CLI on PATH".to_string()))
            } else if env.run("python3", &["-c", "import reflect_kb"]).is_some() {
                DepState::Ok(Some("importable by system python3".to_string()))
            } else {
                DepState::Missing
            }
        }
        // Everything else: simple PATH presence.
        other => {
            if env.which(other) {
                DepState::Ok(None)
            } else {
                DepState::Missing
            }
        }
    }
}

/// Detect every dependency in the catalog against the host.
pub fn detect(env: &dyn Env) -> Vec<DepReport> {
    catalog()
        .iter()
        .map(|spec| {
            let state = detect_one(spec, env);
            DepReport {
                name: spec.name,
                kind: spec.kind,
                consumers: spec.consumers.to_vec(),
                required: spec.required,
                why: spec.why,
                install_hint: spec.install_hint,
                satisfied: state.satisfied(),
                state,
            }
        })
        .collect()
}

/// ✓ / ✗ / ⚠ marker for a report.
fn marker(r: &DepReport) -> &'static str {
    if r.satisfied {
        "\u{2713}" // ✓
    } else if r.required {
        "\u{2717}" // ✗
    } else {
        "\u{26A0}" // ⚠
    }
}

fn state_detail(state: &DepState) -> String {
    match state {
        DepState::Ok(Some(d)) => format!(" ({d})"),
        DepState::Ok(None) => String::new(),
        DepState::Alt(d) => format!(" (via {d})"),
        DepState::TooOld(d) => format!(" — {d}"),
        DepState::Missing => " — missing".to_string(),
    }
}

/// Render the report grouped by consumer ("reflect needs: …", "statusline
/// needs: …"), followed by the annotated install block for anything missing.
pub fn print_text(reports: &[DepReport]) {
    println!("Dependency check (classified by what needs each tool):");
    println!("{}", "\u{2501}".repeat(64));

    for consumer in Consumer::order() {
        let group: Vec<&DepReport> =
            reports.iter().filter(|r| r.consumers.contains(consumer)).collect();
        if group.is_empty() {
            continue;
        }
        println!("\n  {} needs:", consumer.label());
        for r in group {
            let tag = if r.required { "required" } else { "optional" };
            println!(
                "    {} {:<10} [{tag}]{}  — {}",
                marker(r),
                r.name,
                state_detail(&r.state),
                r.why,
            );
        }
    }

    print_install_block(reports);
}

/// Print only the actionable install instructions for missing deps, split into
/// the auto-installable reflect layer and the print-only system layer.
pub fn print_install_block(reports: &[DepReport]) {
    let missing: Vec<&DepReport> = reports.iter().filter(|r| !r.satisfied).collect();
    if missing.is_empty() {
        println!("\n\u{2713} Everything is installed. You're fully set up.");
        return;
    }

    let reflect_owned: Vec<&&DepReport> =
        missing.iter().filter(|r| r.kind == DepKind::ReflectOwned).collect();
    let system: Vec<&&DepReport> = missing.iter().filter(|r| r.kind == DepKind::System).collect();

    println!("\n{}", "\u{2501}".repeat(64));
    println!("To install what's missing:");

    if !reflect_owned.is_empty() {
        println!("\n  Reflect layer  (or just run: ainb reflect bootstrap):");
        for r in &reflect_owned {
            println!("    # {} — {}", r.name, r.why);
            println!("    {}", r.install_hint);
        }
    }

    if !system.is_empty() {
        println!("\n  System tools  (run these yourself — ainb won't touch your OS/PATH):");
        for r in &system {
            println!("    # {} — {}", r.name, r.why);
            println!("    {}", r.install_hint);
        }
    }

    println!("\nThen verify with:  ainb doctor");
}

/// One-line summary suitable for the `ainb init` setup output / TUI banner,
/// e.g. "Reflect tooling: 1/2 ready — run `ainb reflect bootstrap`".
pub fn reflect_summary_line(reports: &[DepReport]) -> String {
    let reflect: Vec<&DepReport> =
        reports.iter().filter(|r| r.consumers.contains(&Consumer::Reflect)).collect();
    let ready = reflect.iter().filter(|r| r.satisfied).count();
    let total = reflect.len();
    if ready == total {
        format!("Reflect tooling: {ready}/{total} ready \u{2713}")
    } else {
        format!("Reflect tooling: {ready}/{total} ready — run `ainb reflect bootstrap`")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Canned host: a set of "installed" binaries + scripted run() answers.
    struct MockEnv {
        present: Vec<&'static str>,
        runs: HashMap<String, String>,
    }

    impl Env for MockEnv {
        fn which(&self, name: &str) -> bool {
            self.present.contains(&name)
        }
        fn run(&self, cmd: &str, args: &[&str]) -> Option<String> {
            self.runs.get(&format!("{cmd} {}", args.join(" "))).cloned()
        }
    }

    fn find<'a>(reports: &'a [DepReport], name: &str) -> &'a DepReport {
        reports.iter().find(|r| r.name == name).expect("dep present")
    }

    #[test]
    fn bash_3_2_reports_too_old() {
        let env = MockEnv {
            present: vec!["bash"],
            runs: HashMap::from([(
                "bash -c echo ${BASH_VERSINFO[0]:-0}".to_string(),
                "3".to_string(),
            )]),
        };
        let reports = detect(&env);
        let bash = find(&reports, "bash");
        assert!(matches!(bash.state, DepState::TooOld(_)));
        assert!(!bash.satisfied);
        assert!(bash.is_blocking());
    }

    #[test]
    fn bash_5_is_ok() {
        let env = MockEnv {
            present: vec!["bash"],
            runs: HashMap::from([(
                "bash -c echo ${BASH_VERSINFO[0]:-0}".to_string(),
                "5".to_string(),
            )]),
        };
        assert!(find(&detect(&env), "bash").satisfied);
    }

    #[test]
    fn gtimeout_satisfies_timeout_as_alt() {
        let env = MockEnv {
            present: vec!["gtimeout"],
            runs: HashMap::new(),
        };
        let reports = detect(&env);
        let timeout = find(&reports, "timeout");
        assert!(timeout.satisfied);
        assert!(matches!(timeout.state, DepState::Alt(_)));
    }

    #[test]
    fn reflect_kb_via_path_binary() {
        let env = MockEnv {
            present: vec!["reflect"],
            runs: HashMap::new(),
        };
        assert!(find(&detect(&env), "reflect-kb").satisfied);
    }

    #[test]
    fn reflect_kb_via_system_python_import() {
        let env = MockEnv {
            present: vec![],
            runs: HashMap::from([("python3 -c import reflect_kb".to_string(), String::new())]),
        };
        assert!(find(&detect(&env), "reflect-kb").satisfied);
    }

    #[test]
    fn empty_host_blocks_required_deps_only() {
        let env = MockEnv {
            present: vec![],
            runs: HashMap::new(),
        };
        let reports = detect(&env);
        assert!(find(&reports, "git").is_blocking());
        assert!(find(&reports, "uv").is_blocking());
        // optional deps are missing but never "blocking".
        assert!(!find(&reports, "ccusage").is_blocking());
        assert!(!find(&reports, "bd").is_blocking());
    }

    #[test]
    fn summary_line_counts_reflect_consumers() {
        let env = MockEnv {
            present: vec!["uv"],
            runs: HashMap::new(),
        };
        let reports = detect(&env);
        let line = reflect_summary_line(&reports);
        // uv is the only reflect-consumer present here; assert "1/<total>"
        // robustly so the test survives catalog growth.
        let total = reports.iter().filter(|r| r.consumers.contains(&Consumer::Reflect)).count();
        assert!(line.contains(&format!("1/{total}")));
        assert!(line.contains("bootstrap"));
    }
}
