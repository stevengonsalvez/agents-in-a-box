//! `ainb abtop` CLI surface + argv plumbing.
//!
//! The host CLI shim routes `ainb abtop …` argv to the plugin's
//! `cli_dispatch` (namespace `abtop`). `--format text|json` is a
//! host-global flag — we extract + strip it before clap parses the
//! abtop-specific surface, per [[reference_plugin_clap_strip_global_flags]].
//!
//! abtop has no machine-readable mode, so there is no JSON re-emit and
//! no typed targets. `ainb abtop` runs `abtop --once` and prints the
//! raw text snapshot. Any trailing args (e.g. `--theme`, `--demo`) are
//! forwarded verbatim to abtop after `--once`.

use clap::Parser;

/// Host-global output format. abtop only emits text, so `json` is
/// accepted (and stripped) but has no distinct rendering — it's a
/// no-op the dispatcher tolerates rather than a clap error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Human-readable text (default, and the only thing abtop emits).
    #[default]
    Text,
    /// Requested JSON — abtop has no JSON mode, so this falls back to
    /// the same text passthrough.
    Json,
}

/// Parsed `ainb abtop` arguments (after `--format` is stripped).
///
/// Trailing args are captured verbatim and forwarded to
/// `abtop --once <args...>`. There are no required positionals — bare
/// `ainb abtop` is the common case (snapshot of all agents).
#[derive(Debug, Parser, PartialEq, Eq)]
#[command(name = "abtop")]
pub struct AbtopArgs {
    /// Extra args forwarded verbatim to `abtop --once` (e.g.
    /// `--theme`, `--demo`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub forward: Vec<String>,
}

impl AbtopArgs {
    /// The args to forward to `abtop --once`.
    #[must_use]
    pub fn forward_args(&self) -> &[String] {
        &self.forward
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

/// What [`parse_args`] resolved the argv to.
#[derive(Debug)]
pub enum ParseOutcome {
    /// Parsed successfully into args.
    Parsed(Box<AbtopArgs>),
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
    full.push("abtop".to_string());
    full.extend(argv.iter().cloned());
    match AbtopArgs::try_parse_from(full) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn extract_and_strip_format_space_form() {
        let a = argv(&["--format", "json"]);
        assert_eq!(extract_format(&a), OutputFormat::Json);
        assert_eq!(strip_format_flag(&a), Vec::<String>::new());
    }

    #[test]
    fn extract_and_strip_format_equals_form() {
        let a = argv(&["--format=json"]);
        assert_eq!(extract_format(&a), OutputFormat::Json);
        assert_eq!(strip_format_flag(&a), Vec::<String>::new());
    }

    #[test]
    fn format_defaults_to_text() {
        assert_eq!(extract_format(&argv(&[])), OutputFormat::Text);
        assert_eq!(
            extract_format(&argv(&["--format", "text"])),
            OutputFormat::Text
        );
        // Unknown format value falls back to text.
        assert_eq!(
            extract_format(&argv(&["--format", "yaml"])),
            OutputFormat::Text
        );
    }

    #[test]
    fn strip_format_preserves_forwarded_args() {
        let a = argv(&["--format", "json", "--theme", "dark"]);
        assert_eq!(strip_format_flag(&a), argv(&["--theme", "dark"]));
    }

    fn parsed(argv_in: &[&str]) -> AbtopArgs {
        match parse_args(&argv(argv_in)) {
            ParseOutcome::Parsed(a) => *a,
            other => panic!("expected Parsed, got {other:?}"),
        }
    }

    #[test]
    fn parse_args_no_args_is_empty_forward() {
        let a = parsed(&[]);
        assert!(a.forward_args().is_empty());
    }

    #[test]
    fn parse_args_forwards_trailing_args() {
        let a = parsed(&["--theme", "dark"]);
        assert_eq!(
            a.forward_args(),
            &["--theme".to_string(), "dark".to_string()]
        );
    }

    #[test]
    fn parse_args_help_is_help_outcome() {
        assert!(matches!(
            parse_args(&argv(&["--help"])),
            ParseOutcome::HelpOrVersion(_)
        ));
    }
}
