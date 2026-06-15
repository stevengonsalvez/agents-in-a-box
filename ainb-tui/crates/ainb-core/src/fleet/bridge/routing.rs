// ABOUTME: Pure target-routing logic for the native phone bridge.
//
// Ported verbatim from the Python `ainb_phone_bridge.routing` (the verified
// behavioral spec). Two concerns, both pure:
//   1. `parse_target_prefix("name: hello", names)` -> `(Some("name"), "hello")`.
//      Bare text (no recognised "name:" prefix) -> `(None, text)`.
//   2. `resolve_target(parsed_name, sessions)` -> the session to relay to.
//      Conductor-first when present; degrades to any named session otherwise so
//      the bridge is useful BEFORE the separate ATC/conductor track lands.
//
// A `TargetSession` is the lightweight relay target the transport builds from
// `ainb list --format json`. Keeping these functions free of I/O makes the
// routing contract testable without a live fleet.

use std::sync::LazyLock;

use regex::Regex;

/// A target name is a leading `<name>:` token. Names mirror ainb workspace /
/// tmux names: alphanumerics plus the separators ainb actually uses. The
/// `(?s)` flag makes `.` match newlines so a multi-line message body survives.
static PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^([A-Za-z0-9][A-Za-z0-9._-]*)\s*:\s*(.*)$").unwrap());

/// A relay target resolved from `ainb list --format json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSession {
    /// `workspace_name` — the human-facing routing key.
    pub name: String,
    /// tmux session name used by send-keys.
    pub tmux_session: String,
    /// `worktree_path` — locates the JSONL transcript.
    pub cwd: String,
    /// ainb session id.
    pub session_id: String,
    /// Whether the name looks like a conductor/ATC session.
    pub is_conductor: bool,
}

impl TargetSession {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        tmux_session: impl Into<String>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        let name = name.into();
        let is_conductor = is_conductor_name(&name);
        Self {
            name,
            tmux_session: tmux_session.into(),
            cwd: cwd.into(),
            session_id: session_id.into(),
            is_conductor,
        }
    }
}

/// Split an inbound message into `(target_name, message)`.
///
/// A leading `"<name>:"` selects a named session ONLY when `<name>` matches one
/// of `known_names` (case-insensitive). Otherwise the whole string is the
/// message and the target is `None` (caller falls back to the default).
///
/// The known-names guard is what stops a normal sentence like `"note: fix this"`
/// from being mis-routed to a session called `note`.
#[must_use]
pub fn parse_target_prefix(text: &str, known_names: &[String]) -> (Option<String>, String) {
    if text.is_empty() {
        return (None, String::new());
    }
    let Some(caps) = PREFIX_RE.captures(text) else {
        return (None, text.to_string());
    };
    let candidate = caps.get(1).map_or("", |m| m.as_str());
    let rest = caps.get(2).map_or("", |m| m.as_str());
    // Match case-insensitively but return the session's canonical name so
    // downstream routing/logging stays consistent regardless of how it was typed.
    for name in known_names {
        if name.eq_ignore_ascii_case(candidate) {
            return (Some(name.clone()), rest.trim().to_string());
        }
    }
    // Looks like a prefix but isn't a real session — treat as plain text.
    (None, text.to_string())
}

/// True if a session name looks like a conductor/ATC session.
#[must_use]
pub fn is_conductor_name(name: &str) -> bool {
    let lo = name.to_ascii_lowercase();
    lo == "conductor" || lo.starts_with("conductor-")
}

/// Conductors first (0), then alphabetical by lowercased name for a stable
/// default. Mirrors the Python `_conductor_sort_key`.
fn conductor_sort_key(s: &TargetSession) -> (u8, String) {
    (u8::from(!s.is_conductor), s.name.to_ascii_lowercase())
}

/// Pick the default relay target.
///
/// Conductor sessions win; among equals the alphabetically-first name is chosen
/// so the default is stable across restarts. Returns `None` when the fleet is
/// empty.
#[must_use]
pub fn default_target(sessions: &[TargetSession]) -> Option<&TargetSession> {
    sessions.iter().min_by(|a, b| conductor_sort_key(a).cmp(&conductor_sort_key(b)))
}

/// Resolve a relay target from an optional name + the live session list.
///
/// - `parsed_name` given: exact (case-insensitive) match on `name`; if no such
///   session is running, returns `None` (caller reports "no such session"
///   rather than silently relaying to the wrong place).
/// - `parsed_name` absent: the conductor-first default (degrades to any named
///   session).
#[must_use]
pub fn resolve_target<'a>(
    parsed_name: Option<&str>,
    sessions: &'a [TargetSession],
) -> Option<&'a TargetSession> {
    if let Some(wanted) = parsed_name {
        return sessions.iter().find(|s| s.name.eq_ignore_ascii_case(wanted));
    }
    default_target(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    fn sess(name: &str) -> TargetSession {
        TargetSession::new(
            name,
            format!("tmux-{name}"),
            format!("/cwd/{name}"),
            format!("id-{name}"),
        )
    }

    #[test]
    fn parses_named_prefix_case_insensitively() {
        let known = names(&["Backend", "frontend"]);
        let (target, msg) = parse_target_prefix("backend: run the tests", &known);
        assert_eq!(target.as_deref(), Some("Backend")); // canonical casing returned
        assert_eq!(msg, "run the tests");
    }

    #[test]
    fn bare_text_has_no_target() {
        let known = names(&["backend"]);
        let (target, msg) = parse_target_prefix("just do it", &known);
        assert_eq!(target, None);
        assert_eq!(msg, "just do it");
    }

    #[test]
    fn unknown_prefix_is_treated_as_plain_text() {
        // "note:" is NOT a session — the whole string stays the message.
        let known = names(&["backend"]);
        let (target, msg) = parse_target_prefix("note: fix this bug", &known);
        assert_eq!(target, None);
        assert_eq!(msg, "note: fix this bug");
    }

    #[test]
    fn multiline_body_survives_prefix_split() {
        let known = names(&["worker"]);
        let (target, msg) = parse_target_prefix("worker: line one\nline two", &known);
        assert_eq!(target.as_deref(), Some("worker"));
        assert_eq!(msg, "line one\nline two");
    }

    #[test]
    fn empty_text_yields_none_and_empty() {
        assert_eq!(
            parse_target_prefix("", &names(&["x"])),
            (None, String::new())
        );
    }

    #[test]
    fn conductor_names_detected() {
        assert!(is_conductor_name("conductor"));
        assert!(is_conductor_name("Conductor-main"));
        assert!(is_conductor_name("conductor-atc"));
        assert!(!is_conductor_name("backend"));
        assert!(!is_conductor_name("conductors")); // no hyphen, not bare "conductor"
    }

    #[test]
    fn default_prefers_conductor() {
        let sessions = vec![sess("zebra"), sess("conductor-main"), sess("alpha")];
        assert_eq!(default_target(&sessions).unwrap().name, "conductor-main");
    }

    #[test]
    fn default_degrades_to_alphabetical_when_no_conductor() {
        let sessions = vec![sess("zebra"), sess("alpha"), sess("mango")];
        assert_eq!(default_target(&sessions).unwrap().name, "alpha");
    }

    #[test]
    fn default_on_empty_is_none() {
        assert!(default_target(&[]).is_none());
    }

    #[test]
    fn resolve_named_hit_and_miss() {
        let sessions = vec![sess("backend"), sess("frontend")];
        assert_eq!(
            resolve_target(Some("FRONTEND"), &sessions).unwrap().name,
            "frontend"
        );
        assert!(resolve_target(Some("missing"), &sessions).is_none());
    }

    #[test]
    fn resolve_no_name_uses_default() {
        let sessions = vec![sess("zebra"), sess("conductor")];
        assert_eq!(resolve_target(None, &sessions).unwrap().name, "conductor");
    }
}
