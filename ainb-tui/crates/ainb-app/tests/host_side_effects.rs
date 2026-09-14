//! Boundary fence: `ainb-app` performs none of the side effects a host owns.
//!
//! Attaching a terminal, opening an editor or a browser, and touching the
//! clipboard are `Effect`s the reducer returns for a host to carry out. Two
//! checks keep them out of the crate:
//!
//! - the manifest declares no clipboard, browser-open, editor or
//!   terminal-attach crate beyond the ones listed with the step that removes
//!   them, and
//! - every line that spawns a process (`Command::new`, a PTY
//!   `CommandBuilder::new`) or touches the clipboard sits in a module on the
//!   allow-list, at exactly the count recorded there.
//!
//! Both lists are ratchets: a new call site fails, and so does a removed one
//! until its entry shrinks, so the list always says what is left.

use std::collections::BTreeMap;
use std::path::Path;

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

/// Host-effect crates the manifest still declares, and why.
const DECLARED_TODAY: &[(&str, &str)] = &[
    (
        "arboard",
        "the welcome panel and log-history copies; P5 returns them as Effect::Clipboard",
    ),
    (
        "portable-pty",
        "the session preview embed: a tmux client in a PTY whose screen the TUI \
         draws, started by the host's AttachTerminal(InPlace) and its read-only \
         observer",
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
        22,
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
        "components/daemons.rs",
        1,
        "daemon start and stop verbs, output captured (P3)",
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
        2,
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
    (
        "tmux/embed_client.rs",
        3,
        "the preview embed's tmux client: a PTY attach whose screen the TUI draws, \
         and its version probe",
    ),
    ("tmux/mod.rs", 5, "tmux service, output captured"),
    (
        "tmux/process_detection.rs",
        2,
        "process detection through ps and tmux",
    ),
    ("tmux/session.rs", 8, "tmux session service, detached"),
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

/// The `[dependencies]` names in a manifest (not dev or build dependencies).
fn normal_dependencies(manifest: &str) -> Vec<String> {
    let mut in_dependencies = false;
    let mut names = Vec::new();
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_dependencies = line == "[dependencies]";
            continue;
        }
        if !in_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            names.push(name.trim().trim_matches('"').to_string());
        }
    }
    names
}

#[test]
fn the_manifest_declares_no_host_effect_crate_beyond_the_listed_ones() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read ainb-app manifest");
    let declared: std::collections::BTreeSet<String> = normal_dependencies(&manifest)
        .into_iter()
        .filter(|name| HOST_EFFECT_CRATES.contains(&name.as_str()))
        .collect();
    let listed: std::collections::BTreeSet<String> =
        DECLARED_TODAY.iter().map(|(name, _)| (*name).to_string()).collect();
    assert_eq!(
        declared, listed,
        "ainb-app's host-effect dependencies changed. A new one belongs in a host \
         executing an Effect; a removed one comes off DECLARED_TODAY"
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

fn count_call_sites(dir: &Path, root: &Path, counts: &mut BTreeMap<String, usize>) {
    for entry in std::fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            count_call_sites(&path, root, counts);
            continue;
        }
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") || name == "test_support.rs" {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read source file");
        let hits = non_test_lines(&source)
            .into_iter()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| PATTERNS.iter().any(|pattern| line.contains(pattern)))
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
    count_call_sites(&root, &root, &mut found);
    let allowed: BTreeMap<String, usize> = CALL_SITES
        .iter()
        .map(|(path, count, _)| ((*path).to_string(), *count))
        .collect();

    let mismatches: Vec<String> = found
        .keys()
        .chain(allowed.keys())
        .collect::<std::collections::BTreeSet<_>>()
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
