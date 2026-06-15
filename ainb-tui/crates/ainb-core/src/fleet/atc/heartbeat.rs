// ABOUTME: Pure heartbeat logic — the part of ATC that decides without an LLM.
//
// Three responsibilities, all I/O-free so they unit-test cleanly:
//   1. `build_heartbeat` — turn a `Vec<NeedsRow>` (the output of the LLM-free
//      `fleet needs --format json`) into the compact `[HEARTBEAT]` nudge text
//      that gets tmux-sent into the ATC session. ATC spends tokens deciding,
//      not scanning, because the scan already happened here.
//   2. `should_pause_for_idle` — when the fleet has been quiet past the
//      idle-pause threshold, the heartbeat downgrades to a cheap idle ping.
//   3. `RetryLedger` — the per-session auto-continue retry cap the standalone
//      `daemon` skill lacks. ATC's ERR rule consults this before sending
//      `continue`, so a permanently-broken session escalates instead of
//      looping forever.

use std::collections::HashMap;

use crate::fleet::read::{NeedsContext, NeedsRow};

/// Maximum auto-`continue` attempts per session before ATC must escalate
/// instead of retrying. Bounds the daemon's previously-unbounded behaviour.
pub const DEFAULT_ERR_RETRY_CAP: u32 = 3;

/// Decide whether the heartbeat should be a cheap idle ping rather than a full
/// fleet briefing.
///
/// Returns `true` when there is nothing needing attention (`needs_count == 0`)
/// AND the fleet has been quiet for at least `idle_pause_min` minutes. A
/// `last_activity_ms` of `None` is treated as "no recorded activity" → eligible
/// to pause once the window elapses is impossible to prove, so we stay active
/// (conservative: never pause on unknown).
#[must_use]
pub fn should_pause_for_idle(
    needs_count: usize,
    last_activity_ms: Option<i64>,
    idle_pause_min: u32,
    now_ms: i64,
) -> bool {
    if needs_count > 0 {
        return false;
    }
    let Some(last) = last_activity_ms else {
        return false;
    };
    let elapsed_min = (now_ms - last).max(0) / 60_000;
    elapsed_min >= i64::from(idle_pause_min)
}

/// A short label for one needs row used in the heartbeat summary line.
fn row_label(row: &NeedsRow) -> &str {
    row.session
        .tmux_session
        .as_deref()
        .or(row.session.workspace_name.as_deref())
        .unwrap_or(&row.session.id)
}

fn kind_glyph(ctx: &NeedsContext) -> &'static str {
    match ctx {
        NeedsContext::Ask(_) => "ASK",
        NeedsContext::Err(_) => "ERR",
        NeedsContext::Idle(_) => "IDLE",
        NeedsContext::Wait(_) => "WAIT",
    }
}

/// Build the `[HEARTBEAT]` nudge text from the current `fleet needs` rows.
///
/// The body is deterministic and compact: a header with per-kind counts, then
/// one line per blocked session (kind + name + a short context excerpt). When
/// nothing needs attention it returns a single quiet line so ATC can no-op
/// cheaply. The trailing instruction reminds ATC to apply its CLAUDE.md policy
/// (auto-clear safe, escalate uncertain).
#[must_use]
pub fn build_heartbeat(rows: &[NeedsRow], now_ms: i64) -> String {
    build_heartbeat_with_ledger(rows, now_ms, None)
}

/// Build the `[HEARTBEAT]` nudge, optionally consulting a [`RetryLedger`] so any
/// ERR row whose session has already exhausted its auto-`continue` budget is
/// machine-flagged `ESCALATE (retry cap)` rather than left to the model's
/// judgement. This is how the daemon's previously-unbounded auto-continue gains
/// a hard cap.
#[must_use]
pub fn build_heartbeat_with_ledger(
    rows: &[NeedsRow],
    now_ms: i64,
    ledger: Option<&RetryLedger>,
) -> String {
    if rows.is_empty() {
        return format!(
            "[HEARTBEAT {now_ms}] fleet quiet — 0 sessions need attention. \
No action required; update state.json last-check and stand by."
        );
    }

    let (mut ask, mut err, mut idle, mut wait) = (0u32, 0u32, 0u32, 0u32);
    for r in rows {
        match r.context {
            NeedsContext::Ask(_) => ask += 1,
            NeedsContext::Err(_) => err += 1,
            NeedsContext::Idle(_) => idle += 1,
            NeedsContext::Wait(_) => wait += 1,
        }
    }

    let mut out = format!(
        "[HEARTBEAT {now_ms}] {} session(s) need attention — \
ERR {err} · ASK {ask} · IDLE {idle} · WAIT {wait}\n",
        rows.len()
    );

    for r in rows {
        let name = row_label(r);
        let kind = kind_glyph(&r.context);
        let excerpt = match &r.context {
            NeedsContext::Ask(aq) => excerpt(&aq.question, 120),
            NeedsContext::Err(e) => format!("{}: {}", e.pattern, excerpt(&e.snippet, 80)),
            NeedsContext::Idle(i) => format!("idle {}m", i.idle_minutes),
            NeedsContext::Wait(w) => format!("{} {}", w.marker, excerpt(&w.text, 80)),
        };
        // On ERR, if the ledger says this session is out of retries, mark it for
        // escalation so the model does not auto-continue past the cap.
        let suffix = match (&r.context, ledger) {
            (NeedsContext::Err(_), Some(l)) if l.is_exhausted(&r.session.id) => {
                " — ⚠ ESCALATE (retry cap reached; do not auto-continue)"
            }
            _ => "",
        };
        out.push_str(&format!("- [{kind}] {name} — {excerpt}{suffix}\n"));
    }

    out.push_str(
        "Apply ATC policy: auto-clear the safe cases (confident ASK → broadcast; \
ERR → continue within retry cap), escalate the uncertain ones to the phone bridge. \
Then persist state.json + task-log.md.",
    );
    out
}

/// Collapse whitespace and hard-cap to `max` chars with an ellipsis.
fn excerpt(s: &str, max: usize) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        one
    } else {
        let cut: String = one.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// Per-session auto-continue retry accounting — the cap the standalone daemon
/// lacks. ATC records a `continue` send here; once a session reaches the cap it
/// must escalate instead of retrying.
#[derive(Debug, Default)]
pub struct RetryLedger {
    cap: u32,
    counts: HashMap<String, u32>,
}

impl RetryLedger {
    /// New ledger with the given per-session cap (`0` is clamped to 1).
    #[must_use]
    pub fn new(cap: u32) -> Self {
        Self {
            cap: cap.max(1),
            counts: HashMap::new(),
        }
    }

    /// New ledger with the default cap.
    #[must_use]
    pub fn with_default_cap() -> Self {
        Self::new(DEFAULT_ERR_RETRY_CAP)
    }

    /// Whether another auto-`continue` is permitted for `session_id`.
    #[must_use]
    pub fn may_retry(&self, session_id: &str) -> bool {
        self.counts.get(session_id).copied().unwrap_or(0) < self.cap
    }

    /// Record a `continue` send; returns the new attempt count.
    pub fn record(&mut self, session_id: &str) -> u32 {
        let n = self.counts.entry(session_id.to_string()).or_insert(0);
        *n += 1;
        *n
    }

    /// Whether `session_id` has exhausted its retries and must be escalated.
    #[must_use]
    pub fn is_exhausted(&self, session_id: &str) -> bool {
        self.counts.get(session_id).copied().unwrap_or(0) >= self.cap
    }

    /// Clear a session's counter — call when the session recovers (no longer ERR).
    pub fn clear(&mut self, session_id: &str) {
        self.counts.remove(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::read::{ErrContext, IdleContext, RouteHint};
    use crate::fleet::types::{Session, SessionSource};

    fn session(id: &str, tmux: &str) -> Session {
        Session {
            id: id.into(),
            cwd: format!("/tmp/{id}"),
            pid: None,
            git_root: None,
            tmux_session: Some(tmux.into()),
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![SessionSource::Ainb],
            summary: None,
            last_seen_ms: None,
        }
    }

    fn err_row(id: &str, tmux: &str, pattern: &str) -> NeedsRow {
        NeedsRow {
            session: session(id, tmux),
            context: NeedsContext::Err(ErrContext {
                pattern: pattern.into(),
                snippet: "overloaded_error: please retry".into(),
            }),
            route_hint: RouteHint::Tmux,
            enrich_key: String::new(),
            enriched: None,
            need_enrich: false,
        }
    }

    fn idle_row(id: &str, tmux: &str, mins: i64) -> NeedsRow {
        NeedsRow {
            session: session(id, tmux),
            context: NeedsContext::Idle(IdleContext {
                idle_minutes: mins,
                last_assistant_text: None,
            }),
            route_hint: RouteHint::Tmux,
            enrich_key: String::new(),
            enriched: None,
            need_enrich: false,
        }
    }

    #[test]
    fn empty_rows_produce_quiet_heartbeat() {
        let msg = build_heartbeat(&[], 1000);
        assert!(msg.contains("[HEARTBEAT 1000]"));
        assert!(msg.contains("fleet quiet"));
        assert!(msg.contains("0 sessions"));
        // Quiet body must not nag ATC into spending tokens.
        assert!(!msg.contains("Apply ATC policy"));
    }

    #[test]
    fn heartbeat_summarizes_counts_and_lists_rows() {
        let rows = vec![
            err_row("s1", "tmux_a", "overloaded"),
            idle_row("s2", "tmux_b", 12),
        ];
        let msg = build_heartbeat(&rows, 42);
        assert!(msg.contains("[HEARTBEAT 42]"));
        assert!(msg.contains("2 session(s) need attention"));
        assert!(msg.contains("ERR 1"));
        assert!(msg.contains("IDLE 1"));
        assert!(msg.contains("[ERR] tmux_a — overloaded"));
        assert!(msg.contains("[IDLE] tmux_b — idle 12m"));
        assert!(msg.contains("Apply ATC policy"));
    }

    #[test]
    fn ledger_flags_exhausted_err_for_escalation() {
        let rows = vec![err_row("s1", "tmux_a", "overloaded")];
        let mut ledger = RetryLedger::new(1);
        ledger.record("s1"); // s1 now at cap
        let msg = build_heartbeat_with_ledger(&rows, 7, Some(&ledger));
        assert!(
            msg.contains("ESCALATE (retry cap reached"),
            "exhausted ERR not flagged: {msg}"
        );
    }

    #[test]
    fn ledger_under_cap_does_not_flag() {
        let rows = vec![err_row("s1", "tmux_a", "overloaded")];
        let ledger = RetryLedger::new(3); // no records → under cap
        let msg = build_heartbeat_with_ledger(&rows, 7, Some(&ledger));
        assert!(!msg.contains("retry cap reached"));
    }

    #[test]
    fn idle_pause_requires_zero_needs() {
        // Needs present → never pause regardless of quiet time.
        assert!(!should_pause_for_idle(2, Some(0), 60, 60 * 60_000 * 2));
    }

    #[test]
    fn idle_pause_fires_after_threshold() {
        // 0 needs, last activity 90 min ago, threshold 60 → pause.
        let now = 90 * 60_000;
        assert!(should_pause_for_idle(0, Some(0), 60, now));
    }

    #[test]
    fn idle_pause_holds_before_threshold() {
        // 0 needs, only 30 min quiet, threshold 60 → stay active.
        let now = 30 * 60_000;
        assert!(!should_pause_for_idle(0, Some(0), 60, now));
    }

    #[test]
    fn idle_pause_conservative_on_unknown_activity() {
        assert!(!should_pause_for_idle(0, None, 60, 999_999_999));
    }

    #[test]
    fn retry_ledger_caps_then_exhausts() {
        let mut ledger = RetryLedger::new(2);
        assert!(ledger.may_retry("s1"));
        assert_eq!(ledger.record("s1"), 1);
        assert!(ledger.may_retry("s1"));
        assert_eq!(ledger.record("s1"), 2);
        assert!(!ledger.may_retry("s1"), "cap reached");
        assert!(ledger.is_exhausted("s1"));
        // A different session is independent.
        assert!(ledger.may_retry("s2"));
    }

    #[test]
    fn retry_ledger_clear_resets_after_recovery() {
        let mut ledger = RetryLedger::new(1);
        ledger.record("s1");
        assert!(ledger.is_exhausted("s1"));
        ledger.clear("s1");
        assert!(ledger.may_retry("s1"));
        assert!(!ledger.is_exhausted("s1"));
    }

    #[test]
    fn retry_ledger_cap_zero_clamped_to_one() {
        let ledger = RetryLedger::new(0);
        assert!(ledger.may_retry("s1"));
        let mut ledger = ledger;
        ledger.record("s1");
        assert!(!ledger.may_retry("s1"));
    }
}
