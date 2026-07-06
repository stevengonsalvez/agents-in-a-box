// ABOUTME: Inbound reply routing — a phone "reply N" answers the daemon (D18).
//
// When the bridge has proactively pushed "session X asks: … ①②③", the human
// replies "reply 2" (or "X: reply 2", or free text). Instead of blindly relaying
// that into a session's composer, the bridge routes it through the daemon's
// `attention/answer` RPC so the daemon performs the ONE verified last-mile send
// into the session's OPEN picker (INV-2) with the first-answer-wins + C1 guards.
//
// This module owns the PURE routing decision: parse the reply (reuse the
// conductor-prefix target routing the relay already uses), then resolve which
// OPEN attention row the answer targets — refusing on an ambiguous cwd rather
// than mis-routing a safety-critical answer (C1). The daemon call itself is a
// thin wrapper (`daemon::DaemonClient::answer`); the decision is unit-tested
// without a socket.

use ainb_hangar_proto::events::AttentionRow;

use super::routing::{TargetSession, parse_target_prefix, resolve_target};

/// A parsed inbound reply: an optional target name (from a `name:` prefix) plus
/// the answer text to deliver (with any leading `reply`/`reply:` keyword removed
/// so `reply 2` becomes `2`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyIntent {
    /// The addressed session's canonical run-name, or `None` for the default.
    pub target: Option<String>,
    /// The answer text delivered into the session's picker.
    pub answer: String,
}

/// Strip a leading `reply` / `reply:` keyword (case-insensitive) so a phone
/// "reply 2" answers with `2` and "reply yes please" with `yes please`. Text
/// that does not start with the keyword is returned trimmed and unchanged (free
/// text is a valid answer too).
#[must_use]
pub fn strip_reply_keyword(text: &str) -> String {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    for prefix in ["reply:", "reply"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            // Only treat it as the keyword when followed by whitespace / a colon
            // boundary (so a word like "replying" is left intact).
            let boundary = t.get(prefix.len()..=prefix.len());
            if rest.is_empty() || boundary.is_none_or(|b| b.trim().is_empty() || b == ":") {
                return t[prefix.len()..].trim_start_matches([':', ' ', '\t']).trim().to_string();
            }
        }
    }
    t.to_string()
}

/// Parse an inbound message into a [`ReplyIntent`]: split off a `name:` target
/// prefix that addresses a known session (reusing the relay's routing), then
/// strip the `reply` keyword from the remainder.
#[must_use]
pub fn parse_reply_intent(text: &str, sessions: &[TargetSession]) -> ReplyIntent {
    let (target, body) = parse_target_prefix(text, sessions);
    ReplyIntent {
        target,
        answer: strip_reply_keyword(&body),
    }
}

/// The outcome of resolving a reply to a concrete open attention row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyRoute {
    /// The reply resolves to exactly one open attention row — answer it.
    Answer {
        /// The attention id the `attention/answer` RPC targets.
        attention_id: String,
        /// The answer text to deliver.
        answer: String,
    },
    /// The target session has more than one open request (or the target itself
    /// is ambiguous) — REFUSE rather than mis-route (C1). The caller falls back
    /// to the plain relay or reports the ambiguity.
    Ambiguous {
        /// A human-readable reason.
        reason: String,
    },
    /// No open attention row matches the target — the caller falls back to the
    /// existing free-text relay (the message was not an answer to a live ASK).
    NoOpenAttention,
}

/// Resolve a parsed reply to the open attention row it answers.
///
/// Resolution + the C1 safety guard:
///   1. Pick the target SESSION via the same conductor-prefix routing the relay
///      uses (`intent.target`, else the conductor-first default).
///   2. Prefer an EXACT session-id match among the open rows — always
///      unambiguous.
///   3. Otherwise correlate by cwd, but ONLY when that cwd carries exactly ONE
///      open row. Two open rows sharing the cwd (or no target session) is
///      ambiguous → REFUSE, never guess which agent the answer is for.
#[must_use]
pub fn resolve_reply(
    rows: &[AttentionRow],
    intent: &ReplyIntent,
    sessions: &[TargetSession],
) -> ReplyRoute {
    let Some(target) = resolve_target(intent.target.as_deref(), sessions) else {
        // No live session to correlate against — not an answerable reply.
        return ReplyRoute::NoOpenAttention;
    };

    // 1. Exact session-id match is unambiguous.
    if let Some(row) = rows.iter().find(|r| r.session_id == target.session_id) {
        return ReplyRoute::Answer {
            attention_id: row.id.clone(),
            answer: intent.answer.clone(),
        };
    }

    // 2. cwd correlation, guarded by the C1 ambiguity check.
    if target.cwd.is_empty() {
        return ReplyRoute::NoOpenAttention;
    }
    let in_cwd: Vec<&AttentionRow> = rows.iter().filter(|r| r.cwd == target.cwd).collect();
    match in_cwd.len() {
        0 => ReplyRoute::NoOpenAttention,
        1 => ReplyRoute::Answer {
            attention_id: in_cwd[0].id.clone(),
            answer: intent.answer.clone(),
        },
        n => ReplyRoute::Ambiguous {
            reason: format!(
                "{n} open requests in {} — cannot safely route the answer",
                target.cwd
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, session_id: &str, cwd: &str) -> AttentionRow {
        AttentionRow {
            id: id.into(),
            session_id: session_id.into(),
            cwd: cwd.into(),
            workspace_id: None,
            kind: "ask_user_question".into(),
            payload: "{}".into(),
            degraded: false,
            created_at: 1,
            channels: ainb_hangar_proto::ChannelSet::NONE,
        }
    }

    fn sess(name: &str, cwd: &str, id: &str) -> TargetSession {
        TargetSession::with_workspace(name, name, format!("tmux_{name}"), cwd, id)
    }

    // ── keyword stripping ──────────────────────────────────────────────────

    #[test]
    fn strips_reply_keyword_variants() {
        assert_eq!(strip_reply_keyword("reply 2"), "2");
        assert_eq!(strip_reply_keyword("REPLY: yes"), "yes");
        assert_eq!(
            strip_reply_keyword("  reply   later please "),
            "later please"
        );
        // Not the keyword — free-text answers pass through untouched.
        assert_eq!(strip_reply_keyword("yes do it"), "yes do it");
        assert_eq!(
            strip_reply_keyword("replying to your point"),
            "replying to your point"
        );
    }

    // ── intent parsing ─────────────────────────────────────────────────────

    #[test]
    fn bare_reply_has_no_target() {
        let sessions = vec![sess("backend", "/w/b", "id-b")];
        let intent = parse_reply_intent("reply 2", &sessions);
        assert_eq!(intent.target, None);
        assert_eq!(intent.answer, "2");
    }

    #[test]
    fn targeted_reply_splits_name_and_answer() {
        let sessions = vec![sess("backend", "/w/b", "id-b")];
        let intent = parse_reply_intent("backend: reply yes", &sessions);
        assert_eq!(intent.target.as_deref(), Some("backend"));
        assert_eq!(intent.answer, "yes");
    }

    #[test]
    fn free_text_reply_is_the_answer() {
        let sessions = vec![sess("backend", "/w/b", "id-b")];
        let intent = parse_reply_intent("just ship it", &sessions);
        assert_eq!(intent.target, None);
        assert_eq!(intent.answer, "just ship it");
    }

    // ── resolution + C1 ────────────────────────────────────────────────────

    #[test]
    fn single_open_row_in_cwd_resolves_to_answer() {
        let sessions = vec![sess("backend", "/w/b", "hook-differs")];
        let rows = vec![row("att-1", "daemon-sid", "/w/b")];
        let intent = ReplyIntent {
            target: Some("backend".into()),
            answer: "2".into(),
        };
        assert_eq!(
            resolve_reply(&rows, &intent, &sessions),
            ReplyRoute::Answer {
                attention_id: "att-1".into(),
                answer: "2".into()
            }
        );
    }

    #[test]
    fn exact_session_id_match_wins_even_with_cwd_collision() {
        // The discovered session id matches an attention row exactly → answer it,
        // even though a second open row shares the cwd.
        let sessions = vec![sess("backend", "/w/b", "exact-sid")];
        let rows = vec![
            row("att-exact", "exact-sid", "/w/b"),
            row("att-other", "another-sid", "/w/b"),
        ];
        let intent = ReplyIntent {
            target: Some("backend".into()),
            answer: "1".into(),
        };
        assert_eq!(
            resolve_reply(&rows, &intent, &sessions),
            ReplyRoute::Answer {
                attention_id: "att-exact".into(),
                answer: "1".into()
            }
        );
    }

    #[test]
    fn two_open_rows_in_cwd_is_ambiguous_and_refused() {
        let sessions = vec![sess("backend", "/w/b", "hook-differs")];
        let rows = vec![row("att-1", "sid-1", "/w/b"), row("att-2", "sid-2", "/w/b")];
        let intent = ReplyIntent {
            target: Some("backend".into()),
            answer: "2".into(),
        };
        match resolve_reply(&rows, &intent, &sessions) {
            ReplyRoute::Ambiguous { reason } => assert!(reason.contains("2 open requests")),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn no_open_row_falls_through_to_relay() {
        let sessions = vec![sess("backend", "/w/b", "hook-differs")];
        let rows = vec![row("att-1", "sid-1", "/w/other")];
        let intent = ReplyIntent {
            target: Some("backend".into()),
            answer: "2".into(),
        };
        assert_eq!(
            resolve_reply(&rows, &intent, &sessions),
            ReplyRoute::NoOpenAttention
        );
    }

    #[test]
    fn no_matching_target_session_is_no_open_attention() {
        // The named target does not address any live session → not an answerable
        // reply; the caller relays it as plain text.
        let sessions = vec![sess("backend", "/w/b", "id-b")];
        let rows = vec![row("att-1", "sid-1", "/w/b")];
        let intent = ReplyIntent {
            target: Some("ghost".into()),
            answer: "2".into(),
        };
        assert_eq!(
            resolve_reply(&rows, &intent, &sessions),
            ReplyRoute::NoOpenAttention
        );
    }
}
