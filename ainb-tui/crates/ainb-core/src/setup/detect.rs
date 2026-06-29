// ABOUTME: Detection engine for the setup catalog. Probes the host for each
// dependency and joins the spec with its detected state, ready to render or
// serialize. Reuses the testable `Env` host abstraction from `cli/deps.rs`.

use serde::Serialize;

pub use crate::cli::deps::{Env, RealEnv};
use crate::setup::catalog::{Consumer, Detect, Tier, Topic, catalog};

/// Detected state of a single dependency.
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
    /// Cannot be detected programmatically (e.g. marketplace plugin presence).
    Unknown,
}

impl DepState {
    /// True when the dependency is usable (present, or present via alternative).
    pub fn satisfied(&self) -> bool {
        matches!(self, DepState::Ok(_) | DepState::Alt(_))
    }
}

/// A dependency spec joined with its detected state.
#[derive(Debug, Clone, Serialize)]
pub struct DepReport {
    pub id: &'static str,
    pub name: &'static str,
    pub why: &'static str,
    pub tier: Tier,
    pub consumers: Vec<Consumer>,
    /// Copy-paste install command.
    pub install_hint: String,
    /// Whether ainb can run the installer automatically (after consent).
    pub auto_installable: bool,
    pub state: DepState,
    pub satisfied: bool,
}

impl DepReport {
    /// A missing required dependency — the things that actually block setup.
    pub fn is_blocking(&self) -> bool {
        self.tier == Tier::Required && !self.satisfied
    }
}

/// A topic joined with its per-dependency reports.
#[derive(Debug, Clone, Serialize)]
pub struct TopicReport {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub deps: Vec<DepReport>,
}

/// Overall detection result across the whole catalog.
#[derive(Debug, Clone, Serialize)]
pub struct SetupStatus {
    pub topics: Vec<TopicReport>,
}

impl SetupStatus {
    fn all_deps(&self) -> impl Iterator<Item = &DepReport> {
        self.topics.iter().flat_map(|t| &t.deps)
    }

    /// Every required dependency is satisfied.
    pub fn required_met(&self) -> bool {
        self.all_deps().filter(|d| d.tier == Tier::Required).all(|d| d.satisfied)
    }

    /// Required + recommended all satisfied.
    pub fn recommended_met(&self) -> bool {
        self.all_deps()
            .filter(|d| matches!(d.tier, Tier::Required | Tier::Recommended))
            .all(|d| d.satisfied)
    }

    /// Missing required deps (the blockers).
    pub fn blocking(&self) -> Vec<&DepReport> {
        self.all_deps().filter(|d| d.is_blocking()).collect()
    }

    pub fn satisfied_count(&self) -> usize {
        self.all_deps().filter(|d| d.satisfied).count()
    }

    /// Total deps that apply on this platform (i.e. were probed).
    pub fn total_count(&self) -> usize {
        self.all_deps().count()
    }
}

/// Probe one dependency's `Detect` spec against the host.
fn detect_state(detect: &Detect, env: &dyn Env) -> DepState {
    match detect {
        Detect::Bin(name) => {
            if env.which(name) {
                DepState::Ok(None)
            } else {
                DepState::Missing
            }
        }
        Detect::BinAlt { primary, alts } => {
            if env.which(primary) {
                DepState::Ok(None)
            } else if let Some(alt) = alts.iter().find(|a| env.which(a)) {
                DepState::Alt(format!("{alt} (not on PATH as `{primary}`)"))
            } else {
                DepState::Missing
            }
        }
        Detect::MinVersion {
            bin,
            probe_args,
            min_major,
        } => {
            if !env.which(bin) {
                return DepState::Missing;
            }
            match env.run(bin, probe_args) {
                Some(v) => {
                    let major: u32 = v.trim().parse().unwrap_or(0);
                    if major >= *min_major {
                        DepState::Ok(Some(format!("{bin} {major}.x")))
                    } else {
                        DepState::TooOld(format!("{bin} {major}.x (need >={min_major})"))
                    }
                }
                // present but version probe failed — assume usable.
                None => DepState::Ok(None),
            }
        }
        Detect::CommandOk { cmd, args } => match env.run(cmd, args) {
            Some(out) => DepState::Ok(out.lines().next().map(|s| s.trim().to_string())),
            None => DepState::Missing,
        },
        Detect::Custom(name) => detect_custom(name, env),
    }
}

/// Bespoke probes for the `Custom(..)` detection specs.
fn detect_custom(name: &str, env: &dyn Env) -> DepState {
    match name {
        // Homebrew — on PATH, or at a standard prefix even when the shell hasn't
        // loaded `brew shellenv` yet (Linuxbrew / Apple-silicon / Intel mac).
        "brew" => {
            if env.which("brew")
                || std::path::Path::new("/home/linuxbrew/.linuxbrew/bin/brew").exists()
                || std::path::Path::new("/opt/homebrew/bin/brew").exists()
                || std::path::Path::new("/usr/local/bin/brew").exists()
            {
                DepState::Ok(None)
            } else {
                DepState::Missing
            }
        }
        // ainb is whatever is running this wizard — always present. A PATH probe
        // would false-negative for an off-PATH / freshly-downloaded binary and
        // trap the user on the dependency step (the only "install" is Manual).
        "ainb-self" => {
            if std::env::current_exe().is_ok() || env.which("ainb") {
                DepState::Ok(None)
            } else {
                DepState::Missing
            }
        }
        // reflect-kb is installed only when the `reflect` binary is on PATH —
        // a bare system-python import is not enough (skills shell out to it).
        "reflect-kb" => {
            if env.which("reflect") {
                DepState::Ok(Some("reflect CLI on PATH".to_string()))
            } else {
                DepState::Missing
            }
        }
        // gh is logged in — `gh auth status` exits 0 only with a valid token.
        "gh-authed" => {
            if env.run("gh", &["auth", "status"]).is_some() {
                DepState::Ok(Some("logged in".to_string()))
            } else {
                DepState::Missing
            }
        }
        // rtk binary present (full "wired" check happens in the provisioner).
        "rtk-wired" => {
            if env.which("rtk") {
                DepState::Ok(None)
            } else {
                DepState::Missing
            }
        }
        // toolkit deployed = the skill-manager manifest exists.
        "toolkit-deployed" => {
            if home_path_exists(".agents-in-a-box/manifest.yaml") {
                DepState::Ok(Some("manifest present".to_string()))
            } else {
                DepState::Missing
            }
        }
        // Claude Code statusline wired into settings.json.
        "claudecode-statusline" => {
            if home_file_contains(".claude/settings.json", "statusline") {
                DepState::Ok(Some("wired in settings.json".to_string()))
            } else {
                DepState::Missing
            }
        }
        // Codex statusline is pull-based: it works once you're logged into Codex
        // (~/.codex/auth.json), no settings wiring needed.
        "codex-statusline" => {
            if home_path_exists(".codex/auth.json") {
                DepState::Ok(Some("codex logged in".to_string()))
            } else {
                DepState::Missing
            }
        }
        // notifyd hook installed for any agent (Claude / Codex / Copilot).
        "notifyd-installed" => {
            if home_path_exists(".claude/plugins/ainb-hooks")
                || home_file_contains(".claude/settings.json", "ainb-hooks")
                || home_path_exists(".codex/hooks.json")
                || home_path_exists(".copilot/hooks/ainb.json")
            {
                DepState::Ok(None)
            } else {
                DepState::Missing
            }
        }
        // Claude Code marketplace plugins are keyed `<plugin>@<marketplace>` in
        // ~/.claude/plugins/installed_plugins.json and unpacked to
        // cache/<marketplace>/<plugin>/<version> — NOT plugins/<plugin>. The
        // registry is the only reliable presence signal across marketplace
        // renames, so probe it instead of guessing a directory.
        s if s.starts_with("claude-plugin:") => {
            let plugin = &s["claude-plugin:".len()..];
            if claude_plugin_installed(plugin) {
                DepState::Ok(None)
            } else {
                DepState::Missing
            }
        }
        _ => DepState::Unknown,
    }
}

/// True if a Claude Code plugin named `plugin` is in the installed registry
/// (any marketplace). Reads `~/.claude/plugins/installed_plugins.json`, whose
/// `plugins` object is keyed `<plugin>@<marketplace>`.
fn claude_plugin_installed(plugin: &str) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    match std::fs::read_to_string(home.join(".claude/plugins/installed_plugins.json")) {
        Ok(text) => plugin_in_registry(&text, plugin),
        Err(_) => false,
    }
}

/// Pure registry lookup: does the `installed_plugins.json` text list a plugin
/// whose name (the part before `@<marketplace>`) equals `plugin`? Split out so
/// it's unit-testable without touching the real `~/.claude`.
fn plugin_in_registry(registry_json: &str, plugin: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(registry_json)
        .ok()
        .as_ref()
        .and_then(|json| json.get("plugins"))
        .and_then(|p| p.as_object())
        .is_some_and(|plugins| plugins.keys().any(|k| k.split('@').next() == Some(plugin)))
}

fn home_path_exists(rel: &str) -> bool {
    dirs::home_dir().map(|h| h.join(rel).exists()).unwrap_or(false)
}

fn home_file_contains(rel: &str, needle: &str) -> bool {
    dirs::home_dir()
        .map(|h| h.join(rel))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|c| c.contains(needle))
        .unwrap_or(false)
}

/// Detect a single dependency's state against the host (public for the CLI
/// install loop, which needs to re-probe one dep after provisioning it).
pub fn detect_dep(dep: &crate::setup::catalog::Dep, env: &dyn Env) -> DepState {
    detect_state(&dep.detect, env)
}

/// Build a `DepReport` for one catalog dep against the host.
fn report_dep(dep: &crate::setup::catalog::Dep, env: &dyn Env) -> DepReport {
    let state = detect_state(&dep.detect, env);
    DepReport {
        id: dep.id,
        name: dep.name,
        why: dep.why,
        tier: dep.tier,
        consumers: dep.consumers.to_vec(),
        install_hint: dep.install.hint(),
        auto_installable: dep.install.auto_runnable(),
        satisfied: state.satisfied(),
        state,
    }
}

/// Detect the whole catalog against the host, skipping deps that don't apply on
/// the current platform.
pub fn detect_all(env: &dyn Env) -> SetupStatus {
    let topics = catalog()
        .into_iter()
        .map(|t: Topic| TopicReport {
            id: t.id,
            label: t.label,
            description: t.description,
            deps: t.deps.iter().filter(|d| d.applies_here()).map(|d| report_dep(d, env)).collect(),
        })
        .filter(|t| !t.deps.is_empty())
        .collect();
    SetupStatus { topics }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

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

    fn find<'a>(status: &'a SetupStatus, id: &str) -> &'a DepReport {
        status
            .topics
            .iter()
            .flat_map(|t| &t.deps)
            .find(|d| d.id == id)
            .expect("dep present")
    }

    #[test]
    fn empty_host_blocks_required_only() {
        let env = MockEnv {
            present: vec![],
            runs: HashMap::new(),
        };
        let status = detect_all(&env);
        assert!(find(&status, "git").is_blocking());
        assert!(find(&status, "tmux").is_blocking());
        // optional/suggested never block
        assert!(!find(&status, "ccusage").is_blocking());
        assert!(!status.required_met());
    }

    #[test]
    fn bin_present_is_satisfied() {
        let env = MockEnv {
            present: vec!["git", "tmux", "jq", "ainb"],
            runs: HashMap::new(),
        };
        let status = detect_all(&env);
        assert!(find(&status, "git").satisfied);
        assert!(find(&status, "tmux").satisfied);
    }

    #[test]
    fn gtimeout_satisfies_timeout_as_alt() {
        let env = MockEnv {
            present: vec!["gtimeout"],
            runs: HashMap::new(),
        };
        let status = detect_all(&env);
        let t = find(&status, "timeout");
        assert!(t.satisfied);
        assert!(matches!(t.state, DepState::Alt(_)));
    }

    #[test]
    fn bash_3_too_old_5_ok() {
        let old = MockEnv {
            present: vec!["bash"],
            runs: HashMap::from([(
                "bash -c echo ${BASH_VERSINFO[0]:-0}".to_string(),
                "3".to_string(),
            )]),
        };
        assert!(matches!(
            find(&detect_all(&old), "bash").state,
            DepState::TooOld(_)
        ));
        let new = MockEnv {
            present: vec!["bash"],
            runs: HashMap::from([(
                "bash -c echo ${BASH_VERSINFO[0]:-0}".to_string(),
                "5".to_string(),
            )]),
        };
        assert!(find(&detect_all(&new), "bash").satisfied);
    }

    #[test]
    fn reflect_kb_needs_reflect_binary() {
        let env = MockEnv {
            present: vec!["reflect"],
            runs: HashMap::new(),
        };
        assert!(find(&detect_all(&env), "reflect-kb").satisfied);
        let env2 = MockEnv {
            present: vec![],
            runs: HashMap::new(),
        };
        assert!(!find(&detect_all(&env2), "reflect-kb").satisfied);
    }

    #[test]
    fn gh_auth_detected_via_auth_status() {
        // `gh auth status` exits 0 (run() -> Some) only when logged in.
        let authed = MockEnv {
            present: vec!["gh"],
            runs: HashMap::from([("gh auth status".to_string(), String::new())]),
        };
        assert!(find(&detect_all(&authed), "gh-auth").satisfied);
        // No scripted success -> run() returns None -> not authed.
        let not_authed = MockEnv {
            present: vec!["gh"],
            runs: HashMap::new(),
        };
        assert!(!find(&detect_all(&not_authed), "gh-auth").satisfied);
    }

    #[test]
    fn claude_detected_via_command() {
        let env = MockEnv {
            present: vec![],
            runs: HashMap::from([(
                "claude --version".to_string(),
                "2.1.0 (Claude Code)".to_string(),
            )]),
        };
        let status = detect_all(&env);
        let c = find(&status, "claude");
        assert!(c.satisfied);
    }

    #[test]
    fn plugin_registry_lookup_matches_name_across_marketplaces() {
        // Claude keys plugins `<plugin>@<marketplace>`; we match on the name part
        // so a marketplace rename (agents-in-a-box -> ainb-reflect-memory) still
        // detects the same plugin.
        let registry = r#"{
            "version": 2,
            "plugins": {
                "reflect@ainb-reflect-memory": [{"scope": "user"}],
                "caveman@caveman": [{"scope": "user"}]
            }
        }"#;
        assert!(super::plugin_in_registry(registry, "reflect"));
        assert!(super::plugin_in_registry(registry, "caveman"));
        assert!(!super::plugin_in_registry(registry, "abtop"));
        // Missing/garbage registry never panics, never false-positives.
        assert!(!super::plugin_in_registry("not json", "reflect"));
        assert!(!super::plugin_in_registry("{}", "reflect"));
    }
}
