//! Committed agent-friendliness audit for the `ainb` CLI `--help` surface.
//!
//! Drives the REAL `ainb` binary (via `CARGO_BIN_EXE_ainb`) and walks its
//! `--help` tree recursively starting from the root. This is the tree an AI
//! agent actually sees, so it includes the `tui`/`diff-review` commands added
//! inline in `main.rs` and the skill-manager commands routed before clap.
//!
//! For EVERY discovered command node it asserts:
//!   * `--help` exits 0 and renders (no "unrecognized subcommand" / "error:").
//!   * the description block (everything before `Usage:`) does not leak a Rust
//!     doc-comment or maintainer note (no `long_about` leak).
//! For the ROOT and every DEPTH-1 command (except `tui`/`help`) it also asserts:
//!   * an `EXAMPLES:` block is present — the agent's multishot surface.
//!
//! Deterministic: only spawns `--help` (clap short-circuits before any runtime),
//! against an isolated `$HOME`, so there is no docker/network/filesystem
//! dependency.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Run `ainb <path...> --help` with an isolated HOME. Returns (success, output).
fn help(home: &std::path::Path, path: &[String]) -> (bool, String) {
    let mut args: Vec<String> = path.to_vec();
    args.push("--help".to_string());
    let out = Command::new(ainb_bin())
        .args(&args)
        .env("HOME", home)
        .env("AINB_HANGAR_HOME", home)
        .output()
        .expect("spawn ainb");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

/// The description block: everything before the `Usage:` line. Captures both
/// the short `about` and any multi-line `long_about`.
fn description_block(help_text: &str) -> String {
    let mut out = String::new();
    for line in help_text.lines() {
        if line.starts_with("Usage:") {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Child subcommand names parsed from a `--help` `Commands:` section (the
/// built-in `help` pseudo-command is skipped).
fn children(help_text: &str) -> Vec<String> {
    let mut kids = Vec::new();
    let mut in_cmds = false;
    for line in help_text.lines() {
        if line.starts_with("Commands:") {
            in_cmds = true;
            continue;
        }
        if !in_cmds {
            continue;
        }
        // The block ends at a blank line or the next section header.
        if line.trim().is_empty() || line.starts_with("Options:") || line.starts_with("Arguments:")
        {
            break;
        }
        // Child rows are indented two spaces then the name.
        if let Some(rest) = line.strip_prefix("  ") {
            if let Some(name) = rest.split_whitespace().next() {
                if name != "help" && name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
                    kids.push(name.to_string());
                }
            }
        }
    }
    kids
}

/// Markers that mean a Rust doc-comment / maintainer note leaked into the help.
const LEAK_MARKERS: &[&str] = &[
    "Arguments for the",
    "Subcommands for the",
    "subcommand tree",
    "Description set in",
    "intentionally NOT used",
    "augment_args",
    "augment_subcommands",
    "crate::cli",
];

#[test]
fn cli_help_tree_is_agent_friendly() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut violations: Vec<String> = Vec::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut node_count = 0usize;

    // Iterative DFS over the discoverable command tree.
    let mut stack: Vec<Vec<String>> = vec![vec![]];
    while let Some(path) = stack.pop() {
        let key = path.join(" ");
        if !visited.insert(key.clone()) {
            continue;
        }
        node_count += 1;
        let label = if path.is_empty() {
            "ainb".to_string()
        } else {
            format!("ainb {key}")
        };

        let (ok, text) = help(home.path(), &path);
        if !ok {
            violations.push(format!("`{label} --help` exited non-zero:\n{text}"));
            continue;
        }
        if text.contains("unrecognized subcommand") || text.contains("error:") {
            violations.push(format!("`{label} --help` did not render cleanly:\n{text}"));
            continue;
        }

        // No leaked doc-comment / long_about in the description block.
        let desc = description_block(&text);
        for marker in LEAK_MARKERS {
            if desc.contains(marker) {
                violations.push(format!(
                    "`{label}` description leaks a doc-comment marker {marker:?}:\n{desc}"
                ));
            }
        }

        let depth = path.len();
        let name = path.last().map(String::as_str).unwrap_or("");
        // Root + every depth-1 command (bar `tui`, which only launches the TUI)
        // must carry an EXAMPLES multishot block.
        let needs_examples = depth == 0 || (depth == 1 && name != "tui");
        if needs_examples && !text.contains("EXAMPLES:") {
            violations.push(format!("`{label} --help` is missing an EXAMPLES: block"));
        }

        for child in children(&text) {
            let mut next = path.clone();
            next.push(child);
            stack.push(next);
        }
    }

    // Sanity: we actually walked a real tree, not just the root.
    assert!(
        node_count >= 60,
        "audit only walked {node_count} nodes — the tree walker is broken"
    );

    assert!(
        violations.is_empty(),
        "CLI --help audit found {} violation(s) across {} nodes:\n\n{}",
        violations.len(),
        node_count,
        violations.join("\n\n")
    );
}
