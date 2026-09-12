// ABOUTME: Per-provider status event normalizers and the unknown-name counters
// that make an unrecognised provider event visible instead of silent (D14).
//
// Every provider spells the same five or six facts differently. Codex says
// `agent-turn-complete`, Claude says `Stop`, an OSC frame says `turn_complete`.
// The reducer speaks ONE vocabulary — Claude's, because it was first and the
// tables already hold its tokens — so every other spelling is translated here,
// in one place, rather than in a match arm inside whatever consumer noticed the
// difference first.
//
// # Order
//
// The spec fixes the order the normalizers are tried, and it is not arbitrary:
//
// 1. the Claude-compatible family, which is every provider whose hooks emit
//    Claude's own event names or a near-alias of them,
// 2. the published OSC in-band frame schema, which carries its own explicit
//    event names and therefore needs no guessing,
// 3. the session-state family, the generic `state: running|waiting|done`
//    spellings a provider emits when it has no lifecycle hooks at all.
//
// A provider is tried in that order because each step is strictly less certain
// than the one before it. Reversing it would let a generic `waiting` outrank a
// provider's own explicit `request_user_input`.
//
// # Unknown names are counted, never an error
//
// A normalizer answers `None` for a name it does not know. It never errors, and
// the caller never drops the event: the row still gets its clocks, its pane
// binding and its provenance, it simply gains no lifecycle transition. That is
// the correct behaviour for a provider that ships a new event name in a point
// release — the alternative is a daemon that refuses lines it could have stored.
//
// Silence, though, is how an unmapped name stays unmapped for a year. Every
// `None` increments `status_unknown_event{provider,name}`, which `fleet/status`
// returns and `ainb doctor` prints, so "Codex 0.160 added
// `agent-turn-aborted`" shows up as a number an operator can see rather than as
// sessions that mysteriously stop advancing.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// Providers whose hook vocabulary is Claude's, or a documented alias of it.
///
/// Order matters to a reader, not to the lookup: it is the order the spec lists
/// them in, kept so a diff against the spec is a diff and not a puzzle.
pub const CLAUDE_COMPATIBLE_FAMILY: [&str; 6] =
    ["claude", "codex", "droid", "copilot", "devin", "kimi"];

/// The canonical event names the reducer understands.
///
/// Committed as a list so a test can assert that every normalizer's output is
/// one of them: a normalizer that invents a seventh name would otherwise fail
/// silently, because `states_for_hook` answers `(None, None)` for anything it
/// does not recognise and the row would simply never advance.
pub const CANONICAL_EVENTS: [&str; 12] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PostToolBatch",
    "SubagentStart",
    "AskUserQuestion",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
];

/// One provider event name the daemon could not map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownEventCount {
    /// The provider that emitted it.
    pub provider: String,
    /// The raw event name, verbatim.
    pub name: String,
    /// How many times it has been seen since this daemon started.
    pub count: u64,
}

/// Process-wide `status_unknown_event{provider,name}` counters.
///
/// In memory and per-incarnation on purpose. The number an operator needs is
/// "is this daemon, now, seeing names it cannot map" — a persisted total would
/// keep reporting a name that a provider upgrade already fixed.
fn counters() -> &'static Mutex<BTreeMap<(String, String), u64>> {
    static COUNTERS: OnceLock<Mutex<BTreeMap<(String, String), u64>>> = OnceLock::new();
    COUNTERS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Translate one provider event name into the reducer's vocabulary.
///
/// `None` means "this provider said something we have no mapping for", which is
/// counted and then ignored. It is never an error: the event still reaches the
/// store with its clocks and its identity, it just asserts no transition.
#[must_use]
pub fn normalize(provider: &str, raw_event: &str) -> Option<&'static str> {
    // A matcher suffix (`Notification:idle_prompt`) is presentation, not
    // lifecycle, so it is stripped before the lookup rather than multiplying
    // every table by the matchers a provider happens to use.
    let name = raw_event.split(':').next().unwrap_or(raw_event);
    if name.is_empty() {
        return None;
    }
    claude_family(provider, name)
        .or_else(|| osc_frame(name))
        .or_else(|| session_state(name))
        .or_else(|| {
            record_unknown(provider, name);
            None
        })
}

/// Step 1 — the Claude-compatible family.
///
/// Claude's own names pass through unchanged. Every other member maps its
/// aliases onto them; a member whose name is already canonical needs no arm.
fn claude_family(provider: &str, name: &str) -> Option<&'static str> {
    if !CLAUDE_COMPATIBLE_FAMILY
        .iter()
        .any(|known| known.eq_ignore_ascii_case(provider))
    {
        return None;
    }
    if let Some(canonical) = CANONICAL_EVENTS.iter().find(|canonical| **canonical == name) {
        return Some(canonical);
    }
    Some(match name {
        // Codex 0.148 and 0.154, both spellings observed live.
        "request_user_input" => "AskUserQuestion",
        "wait_for_user" => "Notification",
        "agent-turn-complete" | "agentStop" | "task_complete" => "Stop",
        "permission_request" | "exec_approval_request" | "apply_patch_approval_request" => {
            "PermissionRequest"
        }
        "agent-turn-start" | "task_started" => "UserPromptSubmit",
        "session_configured" => "SessionStart",
        // Copilot and Devin both emit a generic tool pair.
        "tool_use_start" => "PreToolUse",
        "tool_use_end" => "PostToolUse",
        "tool_use_error" => "PostToolUseFailure",
        _ => return None,
    })
}

/// Step 2 — the published OSC in-band frame schema.
///
/// A frame carries an explicit event name from a fixed set, so this is a
/// closed mapping and a name outside it is a protocol error on the emitter's
/// side, reported as unknown like any other.
fn osc_frame(name: &str) -> Option<&'static str> {
    Some(match name {
        "session_start" => "SessionStart",
        "turn_start" => "UserPromptSubmit",
        "turn_complete" => "Stop",
        "turn_failed" => "StopFailure",
        "needs_input" => "AskUserQuestion",
        "needs_approval" => "PermissionRequest",
        _ => return None,
    })
}

/// Step 3 — the generic session-state family.
///
/// The last resort, for a provider with no lifecycle hooks that publishes only
/// a coarse state word. Deliberately does NOT map anything to a needs-input
/// event: a generic `waiting` is not evidence that a human is required, and
/// tier 2 and below may not assert that at all.
fn session_state(name: &str) -> Option<&'static str> {
    Some(match name {
        "running" | "busy" => "UserPromptSubmit",
        "idle" | "done" | "complete" => "Stop",
        "error" | "failed" => "StopFailure",
        _ => return None,
    })
}

/// Count one unmapped `(provider, name)` sighting.
fn record_unknown(provider: &str, name: &str) {
    let Ok(mut counters) = counters().lock() else {
        // A poisoned counter must never take the daemon down: the whole point
        // of this module is that an unknown name is survivable.
        return;
    };
    *counters.entry((provider.to_ascii_lowercase(), name.to_string())).or_insert(0) += 1;
}

/// Every `status_unknown_event{provider,name}` seen since this daemon started,
/// most frequent first.
#[must_use]
pub fn unknown_events() -> Vec<UnknownEventCount> {
    let Ok(counters) = counters().lock() else {
        return Vec::new();
    };
    let mut rows: Vec<_> = counters
        .iter()
        .map(|((provider, name), count)| UnknownEventCount {
            provider: provider.clone(),
            name: name.clone(),
            count: *count,
        })
        .collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    rows
}

/// Clear the counters. Test-only: the process-wide map would otherwise carry
/// one test's sightings into the next one's assertions.
#[cfg(test)]
pub(crate) fn reset_unknown_events() {
    if let Ok(mut counters) = counters().lock() {
        counters.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A normalizer that invented a name outside the reducer's vocabulary would
    /// fail silently: `states_for_hook` answers `(None, None)` for an unknown
    /// token, so the row would simply never advance.
    #[test]
    fn every_normalizer_answers_in_the_reducers_vocabulary() {
        let sources = [
            ("codex", "request_user_input"),
            ("codex", "agent-turn-complete"),
            ("copilot", "tool_use_start"),
            ("devin", "tool_use_error"),
            ("kimi", "session_configured"),
            ("droid", "Stop"),
            ("unknown", "turn_complete"),
            ("unknown", "needs_input"),
            ("unknown", "running"),
            ("unknown", "failed"),
        ];
        for (provider, raw) in sources {
            let mapped = normalize(provider, raw)
                .unwrap_or_else(|| panic!("{provider}/{raw} must map to something"));
            assert!(
                CANONICAL_EVENTS.contains(&mapped),
                "{provider}/{raw} mapped to {mapped}, which the reducer does not know"
            );
        }
    }

    /// The order is the contract. A provider's own explicit request must beat
    /// the generic state word, which is what reversing the order would break.
    #[test]
    fn a_providers_own_name_outranks_the_generic_state_family() {
        // `done` is a session-state word; Codex's `task_complete` is its own.
        assert_eq!(normalize("codex", "task_complete"), Some("Stop"));
        // And a Claude-family provider never falls through to the generic
        // family for a name its own family already owns.
        assert_eq!(normalize("claude", "Stop"), Some("Stop"));
    }

    /// The generic family must not be able to say "a human is needed". Only
    /// tiers 0 and 1 may assert that, and a coarse state word is neither.
    #[test]
    fn the_generic_state_family_never_asserts_needs_input() {
        for name in [
            "running", "busy", "idle", "done", "complete", "error", "failed",
        ] {
            let mapped = normalize("some-new-agent", name);
            assert!(
                !matches!(mapped, Some("AskUserQuestion") | Some("PermissionRequest")),
                "{name} mapped to {mapped:?}, which claims a human is needed"
            );
        }
    }

    /// An unknown name is survivable and visible: no error, and a counter that
    /// names exactly what was not understood.
    #[test]
    fn an_unknown_name_is_counted_and_never_an_error() {
        reset_unknown_events();
        assert_eq!(normalize("codex", "agent-turn-aborted"), None);
        assert_eq!(normalize("codex", "agent-turn-aborted"), None);
        assert_eq!(normalize("claude", "SomeFutureHook"), None);

        let unknown = unknown_events();
        assert_eq!(
            unknown,
            vec![
                UnknownEventCount {
                    provider: "codex".to_string(),
                    name: "agent-turn-aborted".to_string(),
                    count: 2,
                },
                UnknownEventCount {
                    provider: "claude".to_string(),
                    name: "SomeFutureHook".to_string(),
                    count: 1,
                },
            ],
            "most frequent first, so the name worth mapping is at the top"
        );
        reset_unknown_events();
    }

    /// A matcher suffix is presentation. Keeping it in the lookup key would
    /// multiply every table by the matchers a provider happens to emit.
    #[test]
    fn a_matcher_suffix_is_stripped_before_the_lookup() {
        assert_eq!(
            normalize("claude", "Notification:idle_prompt"),
            Some("Notification")
        );
    }
}
