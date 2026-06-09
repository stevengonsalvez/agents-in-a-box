//! `ainb witr <target>` CLI surface + output formatters.
//!
//! The host CLI shim routes `ainb witr …` argv to the plugin's
//! `cli_dispatch` (namespace `witr`). `--format text|json` is a
//! host-global flag — we extract + strip it before clap parses the
//! witr-specific surface, per [[reference_plugin_clap_strip_global_flags]].
//!
//! Output modes:
//! - default / `--format text` — human summary (target, primary
//!   process, source, warning count)
//! - `--format json` — the parsed [`WitrSnapshot`] re-emitted via
//!   serde (canonicalises whatever witr printed)
//! - `--tree` — ancestry chain as an indented tree
//! - `--warnings` — just the warnings list
//! - `--short` — passthrough of raw `witr <target>` text (handled in
//!   the dispatcher, not here, since it execs without `--json`)

use clap::{ArgGroup, Parser};

use crate::exec::WitrTarget;
use crate::model::WitrSnapshot;

/// Host-global output format. witr only cares about text vs json.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Human-readable text (default).
    #[default]
    Text,
    /// Canonicalised JSON re-emit.
    Json,
}

/// Parsed `ainb witr` arguments (after `--format` is stripped).
///
/// Target addressing mirrors witr's own model: a positional arg is a
/// process **name**; PIDs/ports/files/containers use explicit flags
/// (`--pid`/`--port`/`--file`/`--container`). Exactly one target
/// source is required — enforced by the `target` [`ArgGroup`], so a
/// missing or duplicated target is a clap usage error (exit 2).
///
/// `--short` (raw witr passthrough) is mutually exclusive with the
/// parsed views `--tree`/`--warnings`. `--short` vs `--format json`
/// can't be a clap conflict (`--format` is stripped before clap sees
/// the args), so the dispatcher guards that pair explicitly.
#[derive(Debug, Parser, PartialEq, Eq)]
#[command(name = "witr")]
#[command(group = ArgGroup::new("target")
    .required(true)
    .args(["name", "pid", "port", "file", "container"]))]
pub struct WitrArgs {
    /// Process name to trace (positional; witr fuzzy-matches).
    pub name: Option<String>,
    /// Trace a process by PID.
    #[arg(long)]
    pub pid: Option<String>,
    /// Trace the process listening on a port.
    #[arg(long)]
    pub port: Option<String>,
    /// Trace the process holding a file open.
    #[arg(long)]
    pub file: Option<String>,
    /// Trace a container by name/id.
    #[arg(long)]
    pub container: Option<String>,
    /// Passthrough raw `witr <target>` text (no JSON re-parse).
    /// Mutually exclusive with --tree / --warnings.
    #[arg(long, conflicts_with_all = ["tree", "warnings"])]
    pub short: bool,
    /// Show only the ancestry chain as a tree.
    #[arg(long)]
    pub tree: bool,
    /// Show only the warnings.
    #[arg(long)]
    pub warnings: bool,
}

impl WitrArgs {
    /// Resolve the typed target. The `target` ArgGroup guarantees
    /// exactly one source is set, so this never returns a default in
    /// practice — the `Name("")` fallback is purely defensive.
    #[must_use]
    pub fn resolve_target(&self) -> WitrTarget {
        if let Some(v) = &self.pid {
            WitrTarget::Pid(v.clone())
        } else if let Some(v) = &self.port {
            WitrTarget::Port(v.clone())
        } else if let Some(v) = &self.file {
            WitrTarget::File(v.clone())
        } else if let Some(v) = &self.container {
            WitrTarget::Container(v.clone())
        } else if let Some(v) = &self.name {
            WitrTarget::Name(v.clone())
        } else {
            WitrTarget::Name(String::new())
        }
    }
}

/// Extract the host-global `--format` value from raw argv.
#[must_use]
pub fn extract_format(argv: &[String]) -> OutputFormat {
    let mut iter = argv.iter().peekable();
    while let Some(a) = iter.next() {
        if let Some(rest) = a.strip_prefix("--format=") {
            return parse_format(rest);
        }
        if a == "--format" {
            if let Some(next) = iter.peek() {
                return parse_format(next);
            }
        }
    }
    OutputFormat::default()
}

/// Remove `--format <v>` / `--format=<v>` from argv so clap (which
/// doesn't declare the flag) parses the rest cleanly.
#[must_use]
pub fn strip_format_flag(argv: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--format" {
            i += 2; // skip flag + value
            continue;
        }
        if a.starts_with("--format=") {
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

fn parse_format(s: &str) -> OutputFormat {
    match s {
        "json" => OutputFormat::Json,
        _ => OutputFormat::Text,
    }
}

/// What `parse_args` resolved the argv to.
#[derive(Debug)]
pub enum ParseOutcome {
    /// Parsed successfully into args.
    Parsed(Box<WitrArgs>),
    /// `--help` / `--version` — clap wants to print to stdout, exit 0.
    HelpOrVersion(String),
    /// A genuine usage error — print to stderr, exit 2.
    UsageError(String),
}

/// Parse stripped argv into a [`ParseOutcome`]. `argv` is the
/// already-format-stripped token list (no program name).
///
/// `--help`/`--version` are distinguished from real usage errors so
/// the dispatcher can route help text to stdout with exit 0 (the
/// conventional shape) rather than stderr/exit-2.
#[must_use]
pub fn parse_args(argv: &[String]) -> ParseOutcome {
    // clap's `try_parse_from` wants argv[0] = program name.
    let mut full = Vec::with_capacity(argv.len() + 1);
    full.push("witr".to_string());
    full.extend(argv.iter().cloned());
    match WitrArgs::try_parse_from(full) {
        Ok(args) => ParseOutcome::Parsed(Box::new(args)),
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                ParseOutcome::HelpOrVersion(e.to_string())
            } else {
                ParseOutcome::UsageError(e.to_string())
            }
        }
    }
}

/// Human-readable summary of a snapshot.
#[must_use]
pub fn format_text(snap: &WitrSnapshot) -> String {
    let p = &snap.process;
    let mut s = String::new();
    s.push_str(&format!(
        "target     {} ({})\n",
        snap.resolved_target, snap.target.kind
    ));
    s.push_str(&format!(
        "process    {} (pid {}, ppid {})\n",
        p.command, p.pid, p.ppid
    ));
    if !p.user.is_empty() {
        s.push_str(&format!("user       {}\n", p.user));
    }
    s.push_str(&format!(
        "source     {} {}\n",
        snap.source.kind, snap.source.name
    ));
    if !snap.ancestry.is_empty() {
        s.push_str(&format!("ancestry   {} levels\n", snap.ancestry.len()));
    }
    if snap.warnings.is_empty() {
        s.push_str("warnings   none\n");
    } else {
        s.push_str(&format!("warnings   {}\n", snap.warnings.len()));
        for w in &snap.warnings {
            s.push_str(&format!("  ! {w}\n"));
        }
    }
    s
}

/// Ancestry chain as an indented tree (root at top).
#[must_use]
pub fn format_tree(snap: &WitrSnapshot) -> String {
    let mut s = String::new();
    // Ancestry is ordered closest-parent-first; reverse so the root
    // (closest to PID 1) is at the top and the target is the leaf.
    let mut indent = 0usize;
    for anc in snap.ancestry.iter().rev() {
        s.push_str(&format!(
            "{}└─ {} (pid {})\n",
            "  ".repeat(indent),
            anc.command,
            anc.pid
        ));
        indent += 1;
    }
    s.push_str(&format!(
        "{}└─ {} (pid {})  ◀ target\n",
        "  ".repeat(indent),
        snap.process.command,
        snap.process.pid
    ));
    s
}

/// Just the warnings, one per line. Empty string when none.
#[must_use]
pub fn format_warnings(snap: &WitrSnapshot) -> String {
    if snap.warnings.is_empty() {
        return "no warnings\n".to_string();
    }
    let mut s = String::new();
    for w in &snap.warnings {
        s.push_str(&format!("{w}\n"));
    }
    s
}

/// Canonicalised JSON re-emit of the snapshot.
pub fn format_json(snap: &WitrSnapshot) -> Result<String, serde_json::Error> {
    let mut s = serde_json::to_string_pretty(snap)?;
    s.push('\n');
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    fn snap(json: serde_json::Value) -> WitrSnapshot {
        serde_json::from_value(json).expect("fixture parses")
    }

    fn nginx() -> WitrSnapshot {
        snap(serde_json::json!({
            "Target": {"Type": "name", "Value": "nginx"},
            "ResolvedTarget": "nginx",
            "Process": {"PID": 1234, "PPID": 800, "Command": "nginx", "User": "root"},
            "Ancestry": [
                {"PID": 800, "PPID": 1, "Command": "systemd"},
                {"PID": 1, "PPID": 0, "Command": "init"}
            ],
            "Source": {"Type": "systemd", "Name": "nginx.service"},
            "Warnings": ["running as root"]
        }))
    }

    #[test]
    fn extract_and_strip_format_space_form() {
        let a = argv(&["1234", "--format", "json"]);
        assert_eq!(extract_format(&a), OutputFormat::Json);
        assert_eq!(strip_format_flag(&a), argv(&["1234"]));
    }

    #[test]
    fn extract_and_strip_format_equals_form() {
        let a = argv(&["--format=json", "1234"]);
        assert_eq!(extract_format(&a), OutputFormat::Json);
        assert_eq!(strip_format_flag(&a), argv(&["1234"]));
    }

    #[test]
    fn format_defaults_to_text() {
        assert_eq!(extract_format(&argv(&["1234"])), OutputFormat::Text);
        assert_eq!(
            extract_format(&argv(&["1234", "--format", "text"])),
            OutputFormat::Text
        );
        // Unknown format value falls back to text.
        assert_eq!(
            extract_format(&argv(&["--format", "yaml"])),
            OutputFormat::Text
        );
    }

    fn parsed(argv_in: &[&str]) -> WitrArgs {
        match parse_args(&argv(argv_in)) {
            ParseOutcome::Parsed(a) => *a,
            other => panic!("expected Parsed, got {other:?}"),
        }
    }

    #[test]
    fn parse_args_positional_name() {
        let a = parsed(&["nginx"]);
        assert_eq!(a.resolve_target(), WitrTarget::Name("nginx".into()));
        assert!(!a.short && !a.tree && !a.warnings);
    }

    #[test]
    fn parse_args_typed_target_flags() {
        assert_eq!(
            parsed(&["--pid", "1234"]).resolve_target(),
            WitrTarget::Pid("1234".into())
        );
        assert_eq!(
            parsed(&["--port", "5432"]).resolve_target(),
            WitrTarget::Port("5432".into())
        );
        assert_eq!(
            parsed(&["--file", "/var/x.lock"]).resolve_target(),
            WitrTarget::File("/var/x.lock".into())
        );
        assert_eq!(
            parsed(&["--container", "redis"]).resolve_target(),
            WitrTarget::Container("redis".into())
        );
    }

    #[test]
    fn parse_args_flags() {
        let a = parsed(&["nginx", "--tree"]);
        assert_eq!(a.resolve_target(), WitrTarget::Name("nginx".into()));
        assert!(a.tree);

        let a = parsed(&["--port", "5432", "--warnings"]);
        assert!(a.warnings);
    }

    #[test]
    fn parse_args_requires_exactly_one_target() {
        // Two target sources → ArgGroup conflict.
        assert!(matches!(
            parse_args(&argv(&["nginx", "--pid", "1"])),
            ParseOutcome::UsageError(_)
        ));
    }

    #[test]
    fn parse_args_short_conflicts_with_tree_and_warnings() {
        // --short is mutually exclusive with the parsed views.
        assert!(matches!(
            parse_args(&argv(&["5432", "--short", "--warnings"])),
            ParseOutcome::UsageError(_)
        ));
        assert!(matches!(
            parse_args(&argv(&["5432", "--short", "--tree"])),
            ParseOutcome::UsageError(_)
        ));
        // --short alone is fine.
        assert!(matches!(
            parse_args(&argv(&["5432", "--short"])),
            ParseOutcome::Parsed(_)
        ));
    }

    #[test]
    fn parse_args_missing_target_is_usage_error() {
        assert!(matches!(
            parse_args(&argv(&[])),
            ParseOutcome::UsageError(_)
        ));
        assert!(matches!(
            parse_args(&argv(&["--tree"])),
            ParseOutcome::UsageError(_)
        ));
    }

    #[test]
    fn parse_args_help_is_help_outcome() {
        assert!(matches!(
            parse_args(&argv(&["--help"])),
            ParseOutcome::HelpOrVersion(_)
        ));
    }

    #[test]
    fn format_text_includes_core_fields() {
        let t = format_text(&nginx());
        assert!(t.contains("target     nginx"));
        assert!(t.contains("pid 1234"));
        assert!(t.contains("ppid 800"));
        assert!(t.contains("root"));
        assert!(t.contains("systemd nginx.service"));
        assert!(t.contains("ancestry   2 levels"));
        assert!(t.contains("running as root"));
    }

    #[test]
    fn format_tree_roots_at_top_target_at_leaf() {
        let t = format_tree(&nginx());
        let lines: Vec<&str> = t.lines().collect();
        // init (root) first, target (nginx) last + marked.
        assert!(lines[0].contains("init"));
        assert!(lines.last().unwrap().contains("nginx"));
        assert!(lines.last().unwrap().contains("◀ target"));
    }

    #[test]
    fn format_warnings_lists_each() {
        let t = format_warnings(&nginx());
        assert_eq!(t.trim(), "running as root");
    }

    #[test]
    fn format_warnings_none() {
        let s = snap(serde_json::json!({
            "Target": {"Type": "pid", "Value": "1"},
            "ResolvedTarget": "1",
            "Process": {"PID": 1, "PPID": 0, "Command": "init"},
            "Ancestry": [],
            "Source": {"Type": "init", "Name": "init"},
            "Warnings": []
        }));
        assert_eq!(format_warnings(&s).trim(), "no warnings");
    }

    #[test]
    fn format_json_round_trips() {
        let original = nginx();
        let json = format_json(&original).expect("serialize");
        let back: WitrSnapshot = serde_json::from_str(&json).expect("re-parse");
        assert_eq!(original, back);
    }
}
