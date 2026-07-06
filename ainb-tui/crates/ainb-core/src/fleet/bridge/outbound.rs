// ABOUTME: Proactive outbound — push a newly-open attention request to the phone.
//
// The converged control plane (D18) turns the bridge two-way: as well as
// relaying an inbound chat message into a session, it now watches the daemon's
// open attention inbox and PUSHES a "session X asks: … ①②③" message to the
// configured channel the instant a new ASK / escalation appears — so the human
// learns a session needs input without opening the TUI.
//
// This module owns the PURE half (the event → message formatting and the
// already-notified de-dupe) plus the channel-agnostic dispatch over a `Notifier`
// seam. The live loop polls `daemon::DaemonClient::attention_list_fleet`, diffs
// against the set it has already pushed, and notifies the fresh rows. The
// formatting + de-dupe are unit-tested without a socket or a live channel.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use ainb_hangar_proto::Channel;
use ainb_hangar_proto::events::AttentionRow;
use serde_json::Value;

use super::daemon::DaemonClient;

/// Circled digits ①..⑨ for option markers; options beyond nine fall back to
/// `N)` so the reply-by-number contract still reads.
const CIRCLED: [char; 9] = ['①', '②', '③', '④', '⑤', '⑥', '⑦', '⑧', '⑨'];

/// A channel that can deliver a proactive text message to the human. Abstracted
/// so the outbound worker is unit-tested with a recording fake, and each live
/// channel (Telegram / Slack / Discord) provides its own send.
pub trait Notifier: Send + Sync {
    /// Deliver `text` to the human. Best-effort — a delivery failure is the
    /// channel's own concern and must not stall the worker.
    fn notify(&self, text: String) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// A short human label for the raising session: the basename of its cwd (the
/// most recognisable handle), falling back to the raw session id.
fn session_label(row: &AttentionRow) -> String {
    let cwd = row.cwd.trim_end_matches('/');
    if !cwd.is_empty() {
        if let Some(base) = cwd.rsplit('/').next() {
            if !base.is_empty() {
                return base.to_string();
            }
        }
    }
    row.session_id.clone()
}

/// The verb phrase for an attention `kind` wire token.
fn kind_verb(kind: &str) -> &'static str {
    match kind {
        "error" => "hit an error",
        "waiting" => "is waiting",
        "escalation" => "escalated",
        "approval" => "needs approval",
        "codex_request_user" => "needs input",
        // ask_user_question + anything else
        _ => "asks",
    }
}

/// Pull the human-facing question/prompt out of a parsed payload, trying the
/// common field names in turn.
fn payload_question(payload: &Value) -> Option<String> {
    for key in ["question", "message", "text", "prompt"] {
        if let Some(s) = payload.get(key).and_then(Value::as_str) {
            if !s.trim().is_empty() {
                return Some(s.trim().to_string());
            }
        }
    }
    None
}

/// Format one open attention row as the proactive phone message:
///
/// ```text
/// session <label> asks: <question>
/// ① <opt1>
/// ② <opt2>
/// Reply with the number (e.g. "reply 2").
/// ```
///
/// Errors / waits render without option lines; a payload that does not parse as
/// JSON is shown verbatim so the message is never blank. Degraded (pane-classifier)
/// rows are flagged so the human knows the source is a heuristic.
#[must_use]
pub fn format_attention_notification(row: &AttentionRow) -> String {
    let label = session_label(row);
    let verb = kind_verb(&row.kind);
    let payload: Value = serde_json::from_str(&row.payload).unwrap_or(Value::Null);

    let question = payload_question(&payload).unwrap_or_else(|| {
        if payload.is_null() {
            row.payload.trim().to_string()
        } else {
            String::new()
        }
    });

    let mut out = format!("session {label} {verb}");
    if !question.is_empty() {
        out.push_str(": ");
        out.push_str(&question);
    }

    if let Some(opts) = payload.get("options").and_then(Value::as_array) {
        for (i, opt) in opts.iter().enumerate() {
            let text = opt.as_str().map_or_else(|| opt.to_string(), str::to_string);
            out.push('\n');
            match CIRCLED.get(i) {
                Some(c) => out.push_str(&format!("{c} {text}")),
                None => out.push_str(&format!("{}) {text}", i + 1)),
            }
        }
        if !opts.is_empty() {
            out.push_str("\nReply with the number (e.g. \"reply 2\").");
        }
    }

    if row.degraded {
        out.push_str("\n(⚠ detected via pane heuristic)");
    }
    out
}

/// Given the ids already pushed and the current open rows, return the rows that
/// are newly open (never notified), recording them in `seen`. This is the
/// worker's de-dupe: a row stays in the open inbox until answered, so without it
/// every poll would re-push the same ASK.
pub fn take_new_rows(seen: &mut HashSet<String>, rows: &[AttentionRow]) -> Vec<AttentionRow> {
    let mut fresh = Vec::new();
    for row in rows {
        if seen.insert(row.id.clone()) {
            fresh.push(row.clone());
        }
    }
    fresh
}

/// Push a proactive message for each row the notify rules routed to the PHONE
/// channel (tcp T5). The routing decision was resolved once at raise time and
/// stamped onto `row.channels`; the bridge is the phone consumer, so it delivers
/// a row iff its set contains [`Channel::Phone`] and silently skips the rest
/// (a `waiting` board-only row, or an `error` the rules kept off the phone). The
/// dispatch the live worker calls once it has diffed out the fresh rows.
pub async fn dispatch(rows: &[AttentionRow], notifier: &dyn Notifier) {
    for row in rows {
        if !row.channels.contains(Channel::Phone) {
            continue;
        }
        notifier.notify(format_attention_notification(row)).await;
    }
}

/// The live outbound worker loop: every `interval`, poll the daemon's open
/// attention inbox and push a proactive message for each NEWLY-open row via
/// `notifier`. De-dupes across polls (an open ASK is pushed once, not every
/// tick) and prunes the seen-set to the currently-open ids so a row that is
/// later answered-then-reraised pushes again. Runs until the task is dropped; a
/// transient daemon error backs off to the next tick (the inbox is durable, so
/// nothing is lost). This is what a channel spawns to become proactive.
pub async fn run(client: DaemonClient, notifier: impl Notifier, interval: Duration) {
    let mut seen: HashSet<String> = HashSet::new();
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        match client.attention_list_fleet().await {
            Ok(rows) => {
                let fresh = take_new_rows(&mut seen, &rows);
                if !fresh.is_empty() {
                    dispatch(&fresh, &notifier).await;
                }
                // Prune answered/closed ids so `seen` tracks only the currently
                // open set — bounding the set and re-arming a re-raised row.
                let current: HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
                seen.retain(|id| current.contains(id.as_str()));
            }
            Err(e) => {
                tracing::debug!(error = %e, "outbound: attention/list poll failed; will retry");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::ChannelSet;
    use std::sync::Mutex;

    /// A phone-routed row (the default for the bridge's tests — the phone is the
    /// channel this consumer delivers). Use [`row_on`] for a specific channel set.
    fn row(id: &str, kind: &str, cwd: &str, payload: &str) -> AttentionRow {
        row_on(
            id,
            kind,
            cwd,
            payload,
            ChannelSet::from_channels([Channel::Phone]),
        )
    }

    fn row_on(
        id: &str,
        kind: &str,
        cwd: &str,
        payload: &str,
        channels: ChannelSet,
    ) -> AttentionRow {
        AttentionRow {
            id: id.into(),
            session_id: format!("sess-{id}"),
            cwd: cwd.into(),
            workspace_id: None,
            kind: kind.into(),
            payload: payload.into(),
            degraded: false,
            created_at: 1000,
            channels,
        }
    }

    #[test]
    fn formats_ask_with_numbered_options() {
        let r = row(
            "a1",
            "ask_user_question",
            "/work/backend",
            r#"{"question":"Ship it?","options":["yes","no","later"]}"#,
        );
        let msg = format_attention_notification(&r);
        assert!(
            msg.starts_with("session backend asks: Ship it?"),
            "got: {msg}"
        );
        assert!(msg.contains("① yes"), "got: {msg}");
        assert!(msg.contains("② no"), "got: {msg}");
        assert!(msg.contains("③ later"), "got: {msg}");
        assert!(
            msg.contains("reply 2"),
            "prompts the reply-by-number contract: {msg}"
        );
    }

    #[test]
    fn error_kind_has_no_options_and_its_own_verb() {
        let r = row("e1", "error", "/work/api", r#"{"message":"build failed"}"#);
        let msg = format_attention_notification(&r);
        assert_eq!(msg, "session api hit an error: build failed");
    }

    #[test]
    fn escalation_and_approval_verbs() {
        let esc = row("x", "escalation", "/w/x", r#"{"question":"stuck 20m"}"#);
        assert!(format_attention_notification(&esc).starts_with("session x escalated: stuck 20m"));
        let appr = row("y", "approval", "/w/y", r#"{"question":"rm -rf ok?"}"#);
        assert!(format_attention_notification(&appr).contains("needs approval: rm -rf ok?"));
    }

    #[test]
    fn unparseable_payload_is_shown_verbatim() {
        let r = row("p", "ask_user_question", "/w/p", "just a raw prompt");
        assert_eq!(
            format_attention_notification(&r),
            "session p asks: just a raw prompt"
        );
    }

    #[test]
    fn degraded_row_is_flagged() {
        let mut r = row("d", "ask_user_question", "/w/d", r#"{"question":"go?"}"#);
        r.degraded = true;
        assert!(format_attention_notification(&r).contains("pane heuristic"));
    }

    #[test]
    fn session_label_falls_back_to_id_when_no_cwd() {
        let r = row("only-id", "error", "", "boom");
        // cwd empty → label is the session id (sess-only-id).
        assert!(format_attention_notification(&r).starts_with("session sess-only-id hit an error"));
    }

    #[test]
    fn take_new_rows_dedupes_across_polls() {
        let mut seen = HashSet::new();
        let a = row("a", "ask_user_question", "/w/a", "{}");
        let b = row("b", "ask_user_question", "/w/b", "{}");

        // First poll: both are fresh.
        let fresh = take_new_rows(&mut seen, &[a.clone(), b.clone()]);
        assert_eq!(fresh.len(), 2);

        // Second poll, same open set: nothing fresh (would-be re-push suppressed).
        let fresh = take_new_rows(&mut seen, &[a.clone(), b.clone()]);
        assert!(
            fresh.is_empty(),
            "an already-notified open row must not re-push"
        );

        // A brand-new open row is the only fresh one.
        let c = row("c", "escalation", "/w/c", "{}");
        let fresh = take_new_rows(&mut seen, &[a, b, c]);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].id, "c");
    }

    struct RecordingNotifier(Mutex<Vec<String>>);
    impl Notifier for RecordingNotifier {
        fn notify(&self, text: String) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.0.lock().unwrap().push(text);
            Box::pin(async {})
        }
    }

    #[tokio::test]
    async fn dispatch_pushes_one_message_per_row() {
        let notifier = RecordingNotifier(Mutex::new(Vec::new()));
        let rows = [
            row(
                "a1",
                "ask_user_question",
                "/w/a",
                r#"{"question":"go?","options":["y","n"]}"#,
            ),
            row("e1", "error", "/w/e", r#"{"message":"boom"}"#),
        ];
        dispatch(&rows, &notifier).await;
        let sent = notifier.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        assert!(sent[0].contains("asks: go?"));
        assert!(sent[1].contains("hit an error: boom"));
    }

    /// The bridge is the PHONE consumer: it delivers a row iff the notify rules
    /// routed it to the phone channel (tcp T5). A row whose resolved set omits
    /// Phone (board-only, or routed only to web/os) is silently skipped, so the
    /// suppressed channel never fires while the allowed one does.
    #[tokio::test]
    async fn dispatch_filters_to_phone_routed_rows() {
        let notifier = RecordingNotifier(Mutex::new(Vec::new()));
        let rows = [
            // Phone-routed → delivered.
            row_on(
                "phone",
                "escalation",
                "/w/esc",
                r#"{"question":"stuck"}"#,
                ChannelSet::from_channels([Channel::Phone, Channel::Web, Channel::Os]),
            ),
            // Web+os only (phone dropped, e.g. a workspace override) → suppressed.
            row_on(
                "webonly",
                "ask_user_question",
                "/w/ask",
                r#"{"question":"go?"}"#,
                ChannelSet::from_channels([Channel::Web, Channel::Os]),
            ),
            // Board-only (the waiting default) → suppressed on the phone.
            row_on("board", "waiting", "/w/wait", "{}", ChannelSet::NONE),
        ];
        dispatch(&rows, &notifier).await;
        let sent = notifier.0.lock().unwrap().clone();
        assert_eq!(
            sent.len(),
            1,
            "only the phone-routed row is pushed: {sent:?}"
        );
        assert!(
            sent[0].contains("escalated"),
            "the delivered row is the escalation: {sent:?}"
        );
    }
}
