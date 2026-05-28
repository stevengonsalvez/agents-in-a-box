//! MappingEngine — pure glob → path resolution for a source's
//! `target_layout` (spec §Phase C, bead v12.C.2).
//!
//! A [`crate::manifest::TargetMapping`] declares that units whose
//! repo-relative path matches `glob` live under `repo` in the source and
//! install to `home` under the tool home. [`resolve_pair`] takes a unit's
//! path, finds the **first** matching mapping (declaration order), and
//! translates the path into its concrete `(home, repo)` destinations.
//!
//! Translation re-roots the portion of `unit_path` that follows the
//! glob's static (pre-wildcard) prefix:
//!
//! ```text
//!   glob       skills/**
//!   home       .claude/skills
//!   repo       toolkit/packages/skills
//!   unit_path  skills/commit/SKILL.md
//!              └─────┘ static prefix → stripped, tail = commit/SKILL.md
//!   ⇒ home: .claude/skills/commit/SKILL.md
//!     repo: toolkit/packages/skills/commit/SKILL.md
//! ```
//!
//! The function is **pure** — no I/O, no globals — so it is trivially
//! testable and safe to call from sync planning / install path
//! computation.

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::manifest::{SourceEntry, TargetMapping};

/// Resolve a unit's `(home, repo)` destination pair against a source's
/// `target_layout`.
///
/// Returns the first mapping (in declaration order) whose `glob` matches
/// `unit_path`, with the matched path re-rooted under that mapping's
/// `home` and `repo`. Returns `None` when no mapping matches — including
/// when `target_layout` is empty. Pure: identical inputs always yield
/// identical output.
///
/// `unit_path` is interpreted as repo-relative and matched using
/// forward-slash separators regardless of host platform.
pub fn resolve_pair(source: &SourceEntry, unit_path: &Path) -> Option<(PathBuf, PathBuf)> {
    source
        .target_layout
        .iter()
        .find_map(|mapping| translate(mapping, unit_path))
}

/// Translate `unit_path` through a single mapping, or `None` if the glob
/// does not match.
fn translate(mapping: &TargetMapping, unit_path: &Path) -> Option<(PathBuf, PathBuf)> {
    let unit = normalise(unit_path);
    if !glob_matches(&mapping.glob, &unit) {
        return None;
    }
    let tail = strip_static_prefix(&mapping.glob, &unit);
    Some((join_tail(&mapping.home, &tail), join_tail(&mapping.repo, &tail)))
}

/// Path → forward-slash string for platform-independent glob matching.
fn normalise(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Join `tail` onto `base`, treating an empty tail as "base itself"
/// (a fully-literal glob points directly at one location).
fn join_tail(base: &Path, tail: &str) -> PathBuf {
    if tail.is_empty() {
        base.to_path_buf()
    } else {
        base.join(tail)
    }
}

/// Whether `glob` matches the whole of `unit` (anchored).
fn glob_matches(glob: &str, unit: &str) -> bool {
    Regex::new(&glob_to_regex(glob))
        .map(|re| re.is_match(unit))
        .unwrap_or(false)
}

/// Compile a glob into an anchored regex source string.
///
/// Supported syntax (bootstrap.js parity):
/// - `*`        — any run of non-separator chars (within one segment)
/// - `**`       — any run of chars, may cross `/` (globstar)
/// - `?`        — a single non-separator char
/// - `{a,b,c}`  — alternation
/// - everything else is matched literally (regex metachars escaped)
///
/// `**` as a whole segment matches **zero or more** path segments, so
/// `**/*.md` matches both `foo.md` and `a/b/foo.md`, and `a/**/b`
/// matches `a/b`. A malformed glob (e.g. an unbalanced `{`) compiles to
/// an invalid regex and is treated by [`glob_matches`] as a non-match
/// rather than an error.
fn glob_to_regex(glob: &str) -> String {
    let mut re = String::with_capacity(glob.len() * 2 + 2);
    re.push('^');
    let mut brace_depth: usize = 0;
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next(); // second `*` of the globstar
                    if chars.peek() == Some(&'/') {
                        // `**/` — match zero or more leading segments.
                        chars.next();
                        re.push_str("(?:.*/)?");
                    } else {
                        re.push_str(".*"); // trailing/bare `**` crosses `/`
                    }
                } else {
                    re.push_str("[^/]*"); // `*` stays within a segment
                }
            }
            '?' => re.push_str("[^/]"),
            '{' => {
                brace_depth += 1;
                re.push_str("(?:");
            }
            '}' => {
                if brace_depth > 0 {
                    brace_depth -= 1;
                    re.push(')');
                } else {
                    re.push_str("\\}");
                }
            }
            ',' if brace_depth > 0 => re.push('|'),
            // Escape every regex metacharacter so literals stay literal.
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '\\' | '[' | ']' | ',' => {
                re.push('\\');
                re.push(c);
            }
            other => re.push(other),
        }
    }
    re.push('$');
    re
}

/// The portion of `unit` that follows the glob's static (pre-wildcard)
/// prefix — the "tail" to re-root under `home`/`repo`.
///
/// The static prefix is the run of leading glob *segments* that contain
/// no wildcard/brace metacharacters. Stripping is done segment-wise (not
/// by raw string prefix) so a literal segment can never partially strip a
/// longer unit segment. Because the glob already matched `unit`, those
/// literal segments are exactly `unit`'s leading segments, so dropping
/// that many leading unit segments yields the tail.
fn strip_static_prefix(glob: &str, unit: &str) -> String {
    let static_segments = glob
        .split('/')
        .take_while(|seg| !seg.chars().any(is_glob_meta))
        .count();

    unit.split('/')
        .skip(static_segments)
        .collect::<Vec<_>>()
        .join("/")
}

fn is_glob_meta(c: char) -> bool {
    matches!(c, '*' | '?' | '{' | '}' | '[' | ']')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_to_regex_anchors_and_segments() {
        assert!(glob_matches("skills/**", "skills/a/b/c"));
        assert!(glob_matches("skills/**", "skills/a"));
        // `**` requires the literal prefix.
        assert!(!glob_matches("skills/**", "agents/a"));
    }

    #[test]
    fn single_star_is_segment_scoped() {
        assert!(glob_matches("commands/*.md", "commands/a.md"));
        assert!(!glob_matches("commands/*.md", "commands/sub/a.md"));
    }

    #[test]
    fn globstar_matches_zero_or_more_segments() {
        // Leading `**/` matches a top-level file (zero segments) and any depth.
        assert!(glob_matches("**/*.md", "foo.md"));
        assert!(glob_matches("**/*.md", "a/b/foo.md"));
        // Middle `**` matches zero intervening segments.
        assert!(glob_matches("a/**/b", "a/b"));
        assert!(glob_matches("a/**/b", "a/x/y/b"));
    }

    #[test]
    fn brace_alternation() {
        let g = "agents/{eng,uni}/*.md";
        assert!(glob_matches(g, "agents/eng/x.md"));
        assert!(glob_matches(g, "agents/uni/y.md"));
        assert!(!glob_matches(g, "agents/other/z.md"));
    }

    #[test]
    fn static_prefix_strip() {
        assert_eq!(strip_static_prefix("skills/**", "skills/commit/SKILL.md"), "commit/SKILL.md");
        assert_eq!(strip_static_prefix("commands/*.md", "commands/prime.md"), "prime.md");
        // Fully-literal glob → empty tail.
        assert_eq!(strip_static_prefix("a/b.md", "a/b.md"), "");
    }
}
