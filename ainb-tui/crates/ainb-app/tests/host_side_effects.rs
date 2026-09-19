//! Boundary fence: `ainb-app` performs none of the side effects a host owns.
//!
//! Attaching a terminal, opening an editor or a browser, and touching the
//! clipboard are `Effect`s the reducer returns for a host to carry out. Two
//! checks keep them out of the crate:
//!
//! - no clipboard, browser-open, editor or terminal-attach crate is reachable
//!   through the crate's normal dependencies, for any target, beyond the ones
//!   listed with the step that removes them, and
//! - every line that spawns a process (`Command::new`, a PTY
//!   `CommandBuilder::new`) or touches the clipboard sits in a module on the
//!   allow-list, at exactly the count recorded there.
//!
//! Both lists are ratchets: a new call site fails, and so does a removed one
//! until its entry shrinks, so the list always says what is left.
//!
//! The source walk is a line fence, not a call-graph check: it counts lines
//! that spell a spawn or a clipboard call, per module. A helper in an
//! allow-listed module can still be called from anywhere, and a spawn spelled
//! through an alias or a macro is not seen.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// Crates whose only job is a side effect a host owns.
const HOST_EFFECT_CRATES: &[&str] = &[
    // Clipboard.
    "arboard",
    "cli-clipboard",
    "clipboard",
    "clipboard-win",
    "copypasta",
    "wl-clipboard-rs",
    "x11-clipboard",
    // Opening a browser or the platform's default handler.
    "open",
    "opener",
    "webbrowser",
    // Launching an editor.
    "edit",
    "open-editor",
    "scrawl",
    // Driving a terminal a child process attaches to.
    "expectrl",
    "portable-pty",
    "pty-process",
    "rexpect",
    "termion",
];

/// Host-effect crates still reachable, and why.
const REACHABLE_TODAY: &[(&str, &str)] = &[
    (
        "arboard",
        "the welcome panel and log-history copies; P5 returns them as Effect::Clipboard",
    ),
    (
        "clipboard-win",
        "arboard's Windows backend; leaves with arboard",
    ),
];

/// Modules that spawn a process or touch the clipboard, with the number of
/// lines that do and why each belongs in the crate. Paths are under `src/`.
const CALL_SITES: &[(&str, usize, &str)] = &[
    (
        "app/events.rs",
        2,
        "onboarding: OSC 52 copy of the installer command (P5: Effect::Clipboard); \
         `sh -c` for a user-confirmed catalog install, output captured",
    ),
    (
        "app/snapshot.rs",
        4,
        "snapshot probes: tmux and git reads, output captured",
    ),
    (
        "app/state.rs",
        21,
        "session lifecycle run from the tick: tmux new-session -d, list and kill; docker \
         inspect, build and run; gh auth status; git worktree prune. Detached or captured",
    ),
    (
        "cli/daemon.rs",
        3,
        "`ainb daemon` subcommand, not the TUI reducer",
    ),
    (
        "cli/deps.rs",
        1,
        "dependency version probe, output captured",
    ),
    (
        "cli/hangar.rs",
        4,
        "`ainb hangar` subcommand, not the TUI reducer",
    ),
    (
        "cli/update.rs",
        16,
        "`ainb update` subcommand, not the TUI reducer",
    ),
    (
        "clipboard.rs",
        1,
        "the OSC 52 writer onboarding uses; leaves with it in P5",
    ),
    (
        "components/log_history_viewer.rs",
        1,
        "arboard copy (P5: Effect::Clipboard)",
    ),
    (
        "components/session_recovery.rs",
        12,
        "recovery probes and detached tmux session recreation (P5)",
    ),
    (
        "components/welcome_panel.rs",
        1,
        "arboard copy (P5: Effect::Clipboard)",
    ),
    ("docker/agents_dev.rs", 1, "docker service, output captured"),
    (
        "docker/container_manager.rs",
        1,
        "docker service, output captured",
    ),
    (
        "fleet/atc/timer.rs",
        7,
        "launchd and systemd timer install, output discarded",
    ),
    (
        "fleet/bridge/secrets.rs",
        1,
        "keychain read through /usr/bin/security",
    ),
    (
        "fleet/bridge/service.rs",
        7,
        "launchd and systemd service install, output discarded",
    ),
    (
        "fleet/daemons/probe.rs",
        1,
        "process liveness probe through ps",
    ),
    ("fleet/read/claude_probe.rs", 1, "process probe through ps"),
    ("git/branch_list.rs", 2, "git service, output captured"),
    ("git/operations.rs", 5, "git service, output captured"),
    (
        "git/remote_repo_manager.rs",
        35,
        "git clone, fetch and ls-remote, output captured",
    ),
    (
        "git/worktree_manager.rs",
        6,
        "git worktree service, output captured",
    ),
    ("headroom/mod.rs", 1, "headroom proxy process, detached"),
    (
        "interactive/session_manager.rs",
        17,
        "session creation: tmux new-session -d, send-keys, has-session and kill, captured",
    ),
    ("mcp_pool/client.rs", 1, "MCP pool daemon, detached"),
    ("mcp_pool/proxy.rs", 2, "pooled MCP server processes, piped"),
    (
        "otel/mod.rs",
        4,
        "telemetry setup probes and installs, output captured",
    ),
    ("rtk/mod.rs", 6, "rtk install and init, output captured"),
    ("setup/provision.rs", 3, "dependency provisioning commands"),
    ("tmux/capture.rs", 1, "tmux capture-pane, output captured"),
    ("tmux/mod.rs", 5, "tmux service, output captured"),
    (
        "tmux/process_detection.rs",
        4,
        "process detection through ps and tmux, including the host session lookup",
    ),
    ("tmux/session.rs", 8, "tmux session service, detached"),
];

/// Reducer modules that still write to disk themselves, with the number of
/// lines that do and why. Paths are under `src/`. No line here calls
/// `.save()`: every store save runs through `Effect::Persist`, so a `.save()`
/// in a reducer module changes a count and fails. The writes left are the
/// session defaults and skill manifest the same step reads back, and files
/// that are not stores.
const REDUCER_DISK_WRITES: &[(&str, usize, &str)] = &[
    (
        "app/events.rs",
        4,
        "session defaults on picker back, configure back and launch, read back \
         in the same step; the skill manifest on adding a source",
    ),
    (
        "app/snapshot.rs",
        2,
        "the periodic session snapshot files, written by the snapshot task",
    ),
    (
        "app/state.rs",
        3,
        "the abtop setup-dismissed marker, session defaults on advancing to \
         configure, and the API key env file auth setup writes",
    ),
    (
        "components/new_session/configure.rs",
        1,
        "a named preset on Ctrl+S, which the same step adds to the list and selects",
    ),
    (
        "components/new_session/pick_repo.rs",
        1,
        "session defaults on Ctrl+R, which the next key reads",
    ),
    (
        "components/session_recovery.rs",
        2,
        "archiving an orphaned session's metadata",
    ),
    (
        "components/skill_manager_screen.rs",
        3,
        "the skill manifest and the discovery skip marker",
    ),
];

/// What a line has to contain to count as a reducer writing to disk.
const DISK_WRITE_PATTERNS: &[&str] = &[
    ".save()",
    ".save_to(",
    "fs::write(",
    "save_keys(",
    "save_external_keys(",
    "save_tree_expansion(",
    "save_preset(",
];

/// What a line has to contain to count as a host side effect.
const PATTERNS: &[&str] = &[
    "Command::new(",
    "CommandBuilder::new(",
    "arboard",
    "copy_osc52(",
    "webbrowser",
    "open::that",
];

/// Package names reachable from `ainb-app` through normal dependency edges,
/// for every target. Walks `cargo metadata`'s resolve graph, the same walk
/// `renderer_free.rs` uses, so a target-gated, dotted or renamed dependency is
/// seen by its real package name.
fn reachable_through_normal_dependencies() -> BTreeSet<String> {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("metadata json");
    let names: BTreeMap<&str, &str> = metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .filter_map(|package| Some((package["id"].as_str()?, package["name"].as_str()?)))
        .collect();
    let root = names
        .iter()
        .find_map(|(id, name)| (*name == "ainb-app").then_some(*id))
        .expect("ainb-app in metadata");
    let nodes: BTreeMap<&str, &serde_json::Value> = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve nodes")
        .iter()
        .filter_map(|node| Some((node["id"].as_str()?, node)))
        .collect();

    let mut seen = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(deps) = nodes.get(id).and_then(|node| node["deps"].as_array()) else {
            continue;
        };
        for dep in deps {
            let normal = dep["dep_kinds"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind["kind"].is_null()));
            if let (true, Some(pkg)) = (normal, dep["pkg"].as_str()) {
                stack.push(pkg);
            }
        }
    }
    seen.iter()
        .filter_map(|id| names.get(id))
        .map(|name| (*name).to_string())
        .collect()
}

#[test]
fn no_host_effect_crate_is_reachable_beyond_the_listed_ones() {
    let reached: BTreeSet<String> = reachable_through_normal_dependencies()
        .into_iter()
        .filter(|name| HOST_EFFECT_CRATES.contains(&name.as_str()))
        .collect();
    let listed: BTreeSet<String> =
        REACHABLE_TODAY.iter().map(|(name, _)| (*name).to_string()).collect();
    assert_eq!(
        reached, listed,
        "host-effect crates reachable from ainb-app changed. A new one belongs in a \
         host executing an Effect (`cargo tree -p ainb-app -e normal -i <crate>` shows \
         the path); a removed one comes off REACHABLE_TODAY"
    );
}

/// Lines of `source` outside `#[cfg(test)] mod` blocks. The blocks are found
/// by indentation, which rustfmt keeps exact: a module ends at the first `}`
/// indented like its `mod` line.
fn non_test_lines(source: &str) -> Vec<&str> {
    let lines: Vec<&str> = source.lines().collect();
    let mut kept = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index].trim() == "#[cfg(test)]" {
            let mut next = index + 1;
            while next < lines.len()
                && (lines[next].trim().is_empty() || lines[next].trim().starts_with("#["))
            {
                next += 1;
            }
            let module = lines.get(next).map(|line| line.trim_start());
            let is_module = module.is_some_and(|line| {
                let line = line
                    .strip_prefix("pub(crate) ")
                    .or_else(|| line.strip_prefix("pub "))
                    .unwrap_or(line);
                line.starts_with("mod ") && line.ends_with('{')
            });
            if is_module {
                let indent = lines[next].len() - lines[next].trim_start().len();
                let close = format!("{}}}", " ".repeat(indent));
                let end = (next + 1..lines.len())
                    .find(|&line| lines[line] == close)
                    .unwrap_or(lines.len());
                index = end + 1;
                continue;
            }
        }
        kept.push(lines[index]);
        index += 1;
    }
    kept
}

fn count_call_sites(
    dir: &Path,
    root: &Path,
    patterns: &[&str],
    counts: &mut BTreeMap<String, usize>,
) {
    for entry in std::fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            count_call_sites(&path, root, patterns, counts);
            continue;
        }
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        let is_rust = path.extension().is_some_and(|extension| extension == "rs");
        if !is_rust || name.ends_with("_tests.rs") || name == "test_support.rs" {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read source file");
        let hits = non_test_lines(&source)
            .into_iter()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| patterns.iter().any(|pattern| line.contains(pattern)))
            .count();
        if hits > 0 {
            let relative = path
                .strip_prefix(root)
                .expect("under src")
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            counts.insert(relative, hits);
        }
    }
}

#[test]
fn process_and_clipboard_call_sites_match_the_allow_list() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = BTreeMap::new();
    count_call_sites(&root, &root, PATTERNS, &mut found);
    let allowed: BTreeMap<String, usize> = CALL_SITES
        .iter()
        .map(|(path, count, _)| ((*path).to_string(), *count))
        .collect();

    let mismatches: Vec<String> = found
        .keys()
        .chain(allowed.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| found.get(*path) != allowed.get(*path))
        .map(|path| {
            format!(
                "{path}: found {}, allowed {}",
                found.get(path).copied().unwrap_or(0),
                allowed.get(path).copied().unwrap_or(0)
            )
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "process or clipboard call sites changed:\n  {}\nA new host side effect \
         belongs in an Effect; a removed one shrinks its CALL_SITES entry",
        mismatches.join("\n  ")
    );
}

/// Terminal-host modules that read `AppState.host`, with the number of lines
/// that do and why. Paths are under `ainb-core/src/`. `HostOnlyState` is this
/// process's handles and timers; a line here that draws from it is something
/// a mirrored host cannot draw, so a new one belongs in a versioned section.
const HOST_STATE_READS: &[(&str, usize, &str)] = &[
    (
        "host.rs",
        9,
        "`App`, the process that owns HostOnlyState: init and tick start the log \
         streaming and live window workers and stamp their timers; nothing here draws",
    ),
    (
        "components/layout.rs",
        6,
        "accepted draw inputs until D1: the session log handle, the Pal dial and the \
         daemon start offer are drawn from the handles their workers write (3 lines); \
         the Log tab starts the session log worker and the Pal tab ticks its dial (3 lines)",
    ),
];

#[test]
fn terminal_host_reads_of_host_only_state_match_the_allow_list() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ainb-core/src");
    let mut found = BTreeMap::new();
    count_call_sites(&root, &root, &["state.host."], &mut found);
    let allowed: BTreeMap<String, usize> = HOST_STATE_READS
        .iter()
        .map(|(path, count, _)| ((*path).to_string(), *count))
        .collect();
    assert_eq!(
        found, allowed,
        "reads of AppState.host in the terminal host changed; a new draw input \
         belongs in a versioned section, a removed one shrinks HOST_STATE_READS"
    );
}

/// The plugin runtime handle is the host's (#1045): no line of `AppState`'s
/// struct or of any section names it, so the reducer cannot reach the runtime
/// again by holding it.
#[test]
fn no_plugin_runtime_handle_lives_in_app_state_or_a_section() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app");
    let state = std::fs::read_to_string(src.join("state.rs")).expect("state.rs");
    let start = state.find("pub struct AppState {").expect("AppState struct");
    let end = start + state[start..].find("\n}\n").expect("AppState struct end");
    let sections = std::fs::read_to_string(src.join("sections.rs")).expect("sections.rs");
    for (file, text) in [
        ("state.rs AppState", &state[start..end]),
        ("sections.rs", sections.as_str()),
    ] {
        let holding: Vec<&str> = non_test_lines(text)
            .into_iter()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| {
                [
                    "RuntimeHandle",
                    "ainb_plugin_runtime::Runtime>",
                    "plugin_runtime: ",
                ]
                .iter()
                .any(|name| line.contains(name))
            })
            .collect();
        assert!(
            holding.is_empty(),
            "{file} holds the plugin runtime again: {holding:?}"
        );
    }
}

/// No field in `ainb-app` owns the plugin runtime or its handle, and the crate
/// defines no `App` to hold them (#1086): the terminal host's `App` owns both
/// in `ainb-core`, and any other host brings its own.
#[test]
fn no_module_in_the_crate_owns_the_plugin_runtime() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = BTreeMap::new();
    count_call_sites(
        &src,
        &src,
        &[
            "Option<ainb_plugin_runtime::Runtime>",
            "Option<ainb_plugin_runtime::RuntimeHandle>",
            "Option<RuntimeHandle>",
            "Option<Runtime>",
            "pub struct App ",
            "struct App {",
        ],
        &mut found,
    );
    assert!(
        found.is_empty(),
        "the plugin runtime is held in ainb-app again: {found:?}"
    );
}

/// The workspace load and the token refresh are policy the state owns
/// (#1107): hosts call `start_workspace_load`, `pace_workspace_load` and
/// `refresh_oauth_tokens_if_due`,
/// and the pieces under them stay private so no host re-implements the timeout
/// or calls the Docker probe from a draw path.
#[test]
fn the_workspace_load_and_token_refresh_pieces_stay_private() {
    let state =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/state.rs"))
            .expect("state.rs");
    let reopened: Vec<&str> = [
        "pub async fn load_workspaces_async(",
        "pub fn oauth_token_needs_refresh(",
        "pub const DOCKER_TIMEOUT_SECS",
        "pub async fn is_docker_available(",
        "pub async fn refresh_oauth_tokens(",
        "pub fn start_background_workspace_loading(",
        "pub fn check_workspace_loading_complete(",
        "pub const fn workspace_rescan_floor(",
    ]
    .into_iter()
    .filter(|signature| state.contains(signature))
    .collect();
    assert!(reopened.is_empty(), "made public again: {reopened:?}");
}

/// `HostOnlyState` never serialises: nothing in it may reach a frame.
#[test]
fn host_only_state_is_not_serialize() {
    trait NotSerialize {
        const SERIALIZE: bool = false;
    }
    impl<T> NotSerialize for T {}
    struct Probe<T>(std::marker::PhantomData<T>);
    #[allow(dead_code)]
    impl<T: serde::Serialize> Probe<T> {
        const SERIALIZE: bool = true;
    }
    assert!(!Probe::<ainb_app::app::sections::HostOnlyState>::SERIALIZE);
    assert!(
        Probe::<String>::SERIALIZE,
        "the probe detects a Serialize type"
    );
}

/// The own-session rule reads the session the host reported (#1077). The
/// lookup spawns `tmux` and `ps`, so the crate only defines it: no module
/// calls it, and no dispatch path can reach a spawn to answer the rule.
#[test]
fn nothing_in_the_crate_looks_up_the_tmux_session_the_host_runs_in() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = BTreeMap::new();
    count_call_sites(&src, &src, &["host_tmux_session_name("], &mut found);
    assert_eq!(
        found,
        BTreeMap::from([("tmux/process_detection.rs".to_string(), 1)]),
        "only the definition names the lookup; a host calls it and reports the \
         name through AppState::set_host_tmux_session"
    );
}

#[test]
fn no_reducer_module_saves_a_store_outside_the_persistence_effect() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = BTreeMap::new();
    for module in ["app", "components"] {
        count_call_sites(&src.join(module), &src, DISK_WRITE_PATTERNS, &mut found);
    }
    let allowed: BTreeMap<String, usize> = REDUCER_DISK_WRITES
        .iter()
        .map(|(path, count, _)| ((*path).to_string(), *count))
        .collect();
    assert_eq!(
        found, allowed,
        "disk writes in reducer modules changed. A store save belongs in an \
         Effect::Persist the host runs after the step; a removed write shrinks \
         its REDUCER_DISK_WRITES entry"
    );
}

#[test]
fn the_walk_skips_test_modules_and_comments() {
    let source = "\
fn real() {
    std::process::Command::new(\"tmux\");
}

#[cfg(test)]
mod tests {
    fn fake() {
        std::process::Command::new(\"tmux\");
    }
}

// Command::new( in a comment
fn after() {}
";
    let hits = non_test_lines(source)
        .into_iter()
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter(|line| PATTERNS.iter().any(|pattern| line.contains(pattern)))
        .count();
    assert_eq!(hits, 1);
    assert!(non_test_lines(source).contains(&"fn after() {}"));
}
