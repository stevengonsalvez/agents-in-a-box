// ABOUTME: Pure target-routing logic for the native phone bridge.
//
// Ported verbatim from the Python `ainb_phone_bridge.routing` (the verified
// behavioral spec). Two concerns, both pure:
//   1. `parse_target_prefix("name: hello", sessions)` -> `(Some("name"), "hello")`.
//      A leading `<name>:` matches a session by its `ainb run --name` first, then
//      its workspace name (case-insensitive). Bare/unmatched -> `(None, text)`.
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
    /// The session's `ainb run --name` (recovered from `tmux_session`), or the
    /// workspace name when no distinct run-name exists. This is the PRIMARY
    /// routing key — what the user typed at `ainb run --name X` and what they
    /// address in `default_target` / a `name:` prefix.
    pub name: String,
    /// The workspace/repo folder name (`workspace_name` from discovery). Kept as
    /// a FALLBACK routing alias so addressing a session by its repo folder still
    /// works, preserving the original (pre-fix) behaviour.
    pub workspace_name: String,
    /// tmux session name used by send-keys.
    pub tmux_session: String,
    /// `worktree_path` — locates the JSONL transcript.
    pub cwd: String,
    /// ainb session id.
    pub session_id: String,
    /// Whether the name looks like a conductor/ATC session.
    pub is_conductor: bool,
}

/// How strongly a `TargetSession` matched a routing candidate. Used to break
/// ties when several sessions answer to the same string: an exact run-name
/// match always beats a workspace-name match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NameMatch {
    /// Matched the session's run-name (`name`). Strongest.
    SessionName,
    /// Matched only the workspace/repo folder name. Weaker fallback.
    WorkspaceName,
}

impl TargetSession {
    /// Construct with an explicit run-name and workspace-name. The run-name is
    /// the primary routing key; the workspace-name is the fallback alias.
    #[must_use]
    pub fn with_workspace(
        name: impl Into<String>,
        workspace_name: impl Into<String>,
        tmux_session: impl Into<String>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        let name = name.into();
        let is_conductor = is_conductor_name(&name);
        Self {
            name,
            workspace_name: workspace_name.into(),
            tmux_session: tmux_session.into(),
            cwd: cwd.into(),
            session_id: session_id.into(),
            is_conductor,
        }
    }

    /// Back-compat constructor: the routing name and workspace name are the
    /// same string. Retained so existing call sites / tests that pre-date the
    /// run-name routing fix keep compiling and behaving identically.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        tmux_session: impl Into<String>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        let name = name.into();
        let workspace_name = name.clone();
        Self::with_workspace(name, workspace_name, tmux_session, cwd, session_id)
    }

    /// Does `candidate` address this session, and how strongly?
    ///
    /// Matches the run-name first (case-insensitive), then the workspace name.
    /// Returns `None` when neither matches. Comparing the run-name first means
    /// an exact `ainb run --name` always wins over a coincidental workspace
    /// collision.
    #[must_use]
    pub fn match_strength(&self, candidate: &str) -> Option<NameMatch> {
        if self.name.eq_ignore_ascii_case(candidate) {
            Some(NameMatch::SessionName)
        } else if self.workspace_name.eq_ignore_ascii_case(candidate) {
            Some(NameMatch::WorkspaceName)
        } else {
            None
        }
    }
}

/// Split an inbound message into `(matched_session, message)`.
///
/// A leading `"<name>:"` selects a session ONLY when `<name>` addresses one of
/// `sessions` — by run-name OR workspace name, case-insensitive (see
/// [`TargetSession::match_strength`]). On a match the returned name is the
/// session's canonical run-name (`name`) so downstream routing/logging stays
/// consistent regardless of how it was typed or which alias matched. Otherwise
/// the whole string is the message and the target is `None` (caller falls back
/// to the default).
///
/// The known-sessions guard is what stops a normal sentence like
/// `"note: fix this"` from being mis-routed to a session called `note`.
///
/// When several sessions answer to the same prefix, the strongest match wins
/// (run-name beats workspace-name); ties within the same strength resolve to
/// the first session in `sessions` for determinism.
#[must_use]
pub fn parse_target_prefix(text: &str, sessions: &[TargetSession]) -> (Option<String>, String) {
    if text.is_empty() {
        return (None, String::new());
    }
    let Some(caps) = PREFIX_RE.captures(text) else {
        return (None, text.to_string());
    };
    let candidate = caps.get(1).map_or("", |m| m.as_str());
    let rest = caps.get(2).map_or("", |m| m.as_str());
    match best_match(candidate, sessions) {
        Some(session) => (Some(session.name.clone()), rest.trim().to_string()),
        // Looks like a prefix but isn't a real session — treat as plain text.
        None => (None, text.to_string()),
    }
}

/// Find the session `candidate` most strongly addresses, or `None`.
///
/// Picks the strongest [`NameMatch`] (run-name beats workspace-name); among
/// equally-strong matches the first in `sessions` wins so the choice is
/// deterministic across discovery orderings.
#[must_use]
pub fn best_match<'a>(candidate: &str, sessions: &'a [TargetSession]) -> Option<&'a TargetSession> {
    sessions
        .iter()
        .filter_map(|s| s.match_strength(candidate).map(|m| (m, s)))
        .min_by_key(|(m, _)| *m)
        .map(|(_, s)| s)
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
/// - `parsed_name` given: the best (run-name-first, then workspace-name) match;
///   if no session answers to it, returns `None` (caller reports "no such
///   session" rather than silently relaying to the wrong place).
/// - `parsed_name` absent: the conductor-first default (degrades to any named
///   session).
#[must_use]
pub fn resolve_target<'a>(
    parsed_name: Option<&str>,
    sessions: &'a [TargetSession],
) -> Option<&'a TargetSession> {
    if let Some(wanted) = parsed_name {
        return best_match(wanted, sessions);
    }
    default_target(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions(v: &[&str]) -> Vec<TargetSession> {
        v.iter().map(|s| sess(s)).collect()
    }

    fn sess(name: &str) -> TargetSession {
        TargetSession::new(
            name,
            format!("tmux-{name}"),
            format!("/cwd/{name}"),
            format!("id-{name}"),
        )
    }

    /// A session whose run-name differs from its workspace — mirrors the real
    /// discovery shape (run-name recovered from `tmux_session`, workspace from
    /// the repo folder). This is the case the bridge mis-routed live.
    fn sess_ws(run_name: &str, workspace: &str) -> TargetSession {
        TargetSession::with_workspace(
            run_name,
            workspace,
            format!("tmux_{run_name}"),
            format!("/cwd/{workspace}"),
            format!("id-{run_name}"),
        )
    }

    #[test]
    fn parses_named_prefix_case_insensitively() {
        let known = sessions(&["Backend", "frontend"]);
        let (target, msg) = parse_target_prefix("backend: run the tests", &known);
        assert_eq!(target.as_deref(), Some("Backend")); // canonical casing returned
        assert_eq!(msg, "run the tests");
    }

    #[test]
    fn bare_text_has_no_target() {
        let known = sessions(&["backend"]);
        let (target, msg) = parse_target_prefix("just do it", &known);
        assert_eq!(target, None);
        assert_eq!(msg, "just do it");
    }

    #[test]
    fn unknown_prefix_is_treated_as_plain_text() {
        // "note:" is NOT a session — the whole string stays the message.
        let known = sessions(&["backend"]);
        let (target, msg) = parse_target_prefix("note: fix this bug", &known);
        assert_eq!(target, None);
        assert_eq!(msg, "note: fix this bug");
    }

    #[test]
    fn multiline_body_survives_prefix_split() {
        let known = sessions(&["worker"]);
        let (target, msg) = parse_target_prefix("worker: line one\nline two", &known);
        assert_eq!(target.as_deref(), Some("worker"));
        assert_eq!(msg, "line one\nline two");
    }

    #[test]
    fn empty_text_yields_none_and_empty() {
        assert_eq!(
            parse_target_prefix("", &sessions(&["x"])),
            (None, String::new())
        );
    }

    #[test]
    fn prefix_matches_run_name_not_just_workspace() {
        // Run-name `bridgetest`, workspace `agents-in-a-box` — the real shape.
        let known = vec![sess_ws("bridgetest", "agents-in-a-box")];
        let (target, msg) = parse_target_prefix("bridgetest: ship it", &known);
        assert_eq!(target.as_deref(), Some("bridgetest"));
        assert_eq!(msg, "ship it");
    }

    #[test]
    fn prefix_still_matches_workspace_name_as_fallback() {
        let known = vec![sess_ws("bridgetest", "agents-in-a-box")];
        let (target, msg) = parse_target_prefix("agents-in-a-box: status?", &known);
        // Matched via workspace fallback but returns the canonical run-name.
        assert_eq!(target.as_deref(), Some("bridgetest"));
        assert_eq!(msg, "status?");
    }

    #[test]
    fn run_name_match_beats_workspace_collision() {
        // Two sessions; the candidate is `beta`. Session A's workspace is `beta`
        // (weak match), session B's run-name is `beta` (strong match). B wins.
        let a = sess_ws("alpha", "beta");
        let b = sess_ws("beta", "repo-x");
        let known = vec![a, b];
        let (target, _) = parse_target_prefix("beta: go", &known);
        assert_eq!(target.as_deref(), Some("beta"));
    }

    #[test]
    fn best_match_prefers_run_name_over_workspace() {
        let a = sess_ws("alpha", "shared");
        let b = sess_ws("shared", "repo-x");
        let known = vec![a, b];
        assert_eq!(best_match("shared", &known).unwrap().name, "shared");
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
