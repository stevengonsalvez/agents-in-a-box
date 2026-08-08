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
// formatting + de-dupe are unit-tested without a socket or a live channel, and
// `poll_once` is tested end-to-end against a fake daemon socket.
//
// EVERY tick reports to the shared `BridgeHeartbeat`: a success stamps
// `last_attention_poll_at`, a failure logs at WARN with the socket path and
// stamps `last_attention_error`. That is the only signal by which the health
// probe can tell "the chat gateway is connected" (which says nothing about the
// phone getting anything) from "this bridge can actually reach the fleet".
//
// Reaching the daemon is only HALF the contract, though, and the weaker half.
// The tick also reports the DELIVERY outcome: `Notifier::notify` returns a
// `Result`, a row is recorded in the already-pushed set ONLY once its message
// actually reached a channel, and a failure stamps `last_delivery_error` so the
// probe degrades the row. Without that, a poll that reached the daemon while
// every Discord/Slack/Telegram send 429'd would mark the ask delivered (never
// retried, because the row stays open and the seen-set keeps it forever),
// stamp LAST ACTIVITY for work that did not happen, and render as
// "running + connected (outbound push live)" while the human got nothing.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use ainb_hangar_proto::Channel;
use ainb_hangar_proto::events::AttentionRow;
use serde_json::Value;

use super::daemon::{DaemonClient, DaemonError};
use super::heartbeat::BridgeHeartbeat;

/// Circled digits ①..⑨ for option markers; options beyond nine fall back to
/// `N)` so the reply-by-number contract still reads.
const CIRCLED: [char; 9] = ['①', '②', '③', '④', '⑤', '⑥', '⑦', '⑧', '⑨'];

/// Why a proactive push did not reach the human.
///
/// Carries the operator-facing detail that ends up on the heartbeat and in
/// `ainb fleet daemons`, so a channel MUST scrub secrets out of it before
/// constructing one (the heartbeat scrubs again, defense in depth).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyError(String);

impl NotifyError {
    /// Build a failure from an already-scrubbed, operator-facing detail.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self(detail.into())
    }

    /// The operator-facing detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotifyError {}

/// A channel that can deliver a proactive text message to the human.
///
/// Abstracted so the outbound worker is unit-tested with a recording fake, and
/// each live channel (Telegram / Slack / Discord) provides its own send.
pub trait Notifier: Send + Sync {
    /// Deliver `text` to the human.
    ///
    /// The `Result` is the whole contract: `Ok(())` is a PROMISE that the
    /// message reached the channel, and the worker acts on it (it records the
    /// row as delivered, stamps last-activity, and never pushes it again). A
    /// channel that swallowed its own send failure would make the worker retire
    /// a row the human never saw and let the health surface claim the push is
    /// live, so an `Err` here is mandatory whenever the send did not land.
    ///
    /// Delivery is at-least-once: an `Err` after a multi-chunk send partially
    /// landed causes the whole message to be re-pushed on the next poll. A
    /// duplicate is strictly better than a silently dropped ask.
    fn notify(
        &self,
        text: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), NotifyError>> + Send + '_>>;
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

/// The open rows still owed a push: those not yet settled.
///
/// This is the worker's de-dupe: a row stays in the open inbox until answered,
/// so without it every poll would re-push the same ASK.
///
/// Deliberately PURE, recording nothing. A row joins `seen` only once
/// [`dispatch`] reports it settled (delivered, or never phone-routed), because
/// recording it up front is what turned a failed push into permanent silence:
/// the row stays open, so the prune keeps it in `seen` forever and it is never
/// retried.
// `seen` is the outbound worker's own de-dupe set, never a caller-supplied map,
// so generalising over the hasher would buy nothing.
#[allow(clippy::implicit_hasher)]
#[must_use]
pub fn undelivered_rows(seen: &HashSet<String>, rows: &[AttentionRow]) -> Vec<AttentionRow> {
    rows.iter().filter(|row| !seen.contains(&row.id)).cloned().collect()
}

/// What one [`dispatch`] pass actually achieved. The worker needs all three
/// facts: which rows may be retired, whether any real work happened, and
/// whether the human is missing something.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Dispatched {
    /// Ids that need no further attempt: delivered, or not phone-routed so
    /// there was never anything to deliver. An id absent from this list is
    /// deliberately left out of the seen-set so the next poll retries it.
    pub settled: Vec<String>,
    /// How many messages actually reached a channel. Only this counts as work.
    pub delivered: usize,
    /// The first delivery failure, already scrubbed. `Some` means at least one
    /// phone-routed row did not reach the human on this pass.
    pub failure: Option<NotifyError>,
}

/// Push a proactive message for each row the notify rules routed to the PHONE
/// channel (tcp T5).
///
/// The routing decision was resolved once at raise time and
/// stamped onto `row.channels`; the bridge is the phone consumer, so it delivers
/// a row iff its set contains [`Channel::Phone`] and silently skips the rest
/// (a `waiting` board-only row, or an `error` the rules kept off the phone). The
/// dispatch the live worker calls once it has diffed out the fresh rows.
///
/// A failing channel does NOT stop the pass: every remaining row is still
/// attempted, and the outcome reports what landed and what did not.
pub async fn dispatch(rows: &[AttentionRow], notifier: &dyn Notifier) -> Dispatched {
    let mut out = Dispatched::default();
    for row in rows {
        // Not phone-routed: nothing was ever owed, so it is settled without a
        // send and must not be retried.
        if !row.channels.contains(Channel::Phone) {
            out.settled.push(row.id.clone());
            continue;
        }
        match notifier.notify(format_attention_notification(row)).await {
            Ok(()) => {
                out.settled.push(row.id.clone());
                out.delivered += 1;
            }
            Err(e) => {
                tracing::warn!(
                    attention_id = %row.id,
                    error = %e,
                    "outbound: proactive push did not reach the channel; it will be retried on the next poll"
                );
                out.failure.get_or_insert(e);
            }
        }
    }
    out
}

/// Run ONE outbound tick.
///
/// Poll the daemon's open attention inbox, push a proactive message for each
/// NEWLY-open phone-routed row, and record the outcome on the heartbeat.
/// Returns the number of rows actually DELIVERED, or the daemon error.
///
/// Split out of [`run`] so the whole outbound contract is testable against a
/// fake daemon socket and a recording notifier without spawning a forever-loop:
/// "did it push?" and "did it tell the health surface?" are both assertions here.
///
/// The heartbeat writes are the point. Before this, a failed poll logged at
/// DEBUG and vanished, so a bridge that could not reach the daemon at all still
/// reported "running + connected" indefinitely.
///
/// Both halves of the tick are reported, and only what really happened:
/// reaching the daemon stamps the poll, a DELIVERED message stamps last
/// activity, and an UNDELIVERED one stamps the sticky delivery error the probe
/// degrades on while leaving the row out of `seen` so the next tick retries it.
// `seen` is this worker's own de-dupe set, never a caller-supplied map, so
// generalising over the hasher would buy nothing.
#[allow(clippy::implicit_hasher)]
pub async fn poll_once(
    client: &DaemonClient,
    notifier: &dyn Notifier,
    seen: &mut HashSet<String>,
    heartbeat: &BridgeHeartbeat,
) -> Result<usize, DaemonError> {
    match client.attention_list_fleet().await {
        Ok(rows) => {
            // Stamp the successful poll BEFORE dispatching: reaching the
            // attention source is what this signal means, and a slow channel
            // send must not delay the proof that the daemon is reachable. It
            // says NOTHING about delivery, which is recorded separately below.
            heartbeat.record_attention_poll();
            let fresh = undelivered_rows(seen, &rows);
            let outcome = dispatch(&fresh, notifier).await;
            // Only settled ids join the de-dupe set. A row whose push failed is
            // deliberately left out, so the next tick tries again.
            seen.extend(outcome.settled);
            if outcome.delivered > 0 {
                // A DELIVERED proactive push is real work. The LAST ACTIVITY
                // column has meant "an inbound reply was relayed" only, which is
                // why an outbound-only bridge rendered a bare "-". An attempted
                // push does not count: that is how activity ended up claiming
                // work the human never received.
                heartbeat.record_relay();
            }
            // Prune answered/closed ids so `seen` tracks only the currently
            // open set, bounding the set and re-arming a re-raised row.
            let current: HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
            seen.retain(|id| current.contains(id.as_str()));
            match outcome.failure {
                // A push that never reached the human must never render as
                // healthy: this is the sticky field the probe degrades on.
                Some(e) => heartbeat.record_delivery_error(format!("outbound push: {e}")),
                // Nothing outstanding this tick (the failing row was delivered
                // on retry, or it was answered and closed), so the sticky
                // verdict clears and the bridge goes green again by itself.
                None => heartbeat.clear_delivery_error(),
            }
            Ok(outcome.delivered)
        }
        Err(e) => {
            // WARN, not DEBUG: a persistent dial failure means the phone gets
            // nothing, and at the default log level a DEBUG line does not exist.
            // Name the socket so the operator knows which daemon is unreachable.
            let socket = client.socket().display().to_string();
            tracing::warn!(
                error = %e,
                socket = %socket,
                "outbound: attention/list poll failed; nothing will be pushed to the phone until it recovers"
            );
            heartbeat.record_attention_error(format!("attention/list via {socket}: {e}"));
            Err(e)
        }
    }
}

/// The live outbound worker loop.
///
/// Every `interval`, run one [`poll_once`]. De-dupes across polls (an open ASK is pushed once, not every tick) and prunes
/// the seen-set to the currently-open ids so a row that is later
/// answered-then-reraised pushes again. Runs until the task is dropped; a
/// transient daemon error backs off to the next tick (the inbox is durable, so
/// nothing is lost) after being recorded on the heartbeat. This is what the
/// bridge spawns to become proactive.
pub async fn run(
    client: DaemonClient,
    notifier: impl Notifier,
    interval: Duration,
    heartbeat: BridgeHeartbeat,
) {
    let mut seen: HashSet<String> = HashSet::new();
    let mut ticker = tokio::time::interval(interval);
    tracing::info!(
        socket = %client.socket().display(),
        interval_secs = interval.as_secs(),
        "ainb phone bridge: outbound attention push worker started"
    );
    loop {
        ticker.tick().await;
        let _ = poll_once(&client, &notifier, &mut seen, &heartbeat).await;
    }
}

/// Deliver to every configured phone channel at once.
///
/// The bridge polls the attention source ONCE and fans the message out, rather
/// than running one worker (and one poll loop, and one de-dupe set) per
/// channel.
pub struct Fanout(Vec<Box<dyn Notifier>>);

impl Fanout {
    /// Build a fan-out over the configured channels.
    #[must_use]
    pub fn new(notifiers: Vec<Box<dyn Notifier>>) -> Self {
        Self(notifiers)
    }

    /// Whether there is no channel to deliver to (so no worker should run).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Notifier for Fanout {
    /// Deliver to every channel, and report failure only when NONE of them took
    /// the message.
    ///
    /// A partial failure (Discord delivered, Slack 500'd) is `Ok`, because the
    /// human DID receive the ask, and reporting it as undelivered would leave
    /// the row out of the seen-set and re-push it to the working channel on
    /// every single poll for as long as the ask stays open. The failing channel
    /// is still logged at WARN. When every channel fails, nobody was told, so
    /// that is a real `Err` and the row is retried.
    fn notify(
        &self,
        text: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), NotifyError>> + Send + '_>> {
        Box::pin(async move {
            let mut delivered = 0usize;
            let mut errors: Vec<String> = Vec::new();
            for n in &self.0 {
                match n.notify(text.clone()).await {
                    Ok(()) => delivered += 1,
                    Err(e) => errors.push(e.detail().to_string()),
                }
            }
            if errors.is_empty() {
                return Ok(());
            }
            let detail = errors.join("; ");
            if delivered > 0 {
                tracing::warn!(
                    delivered,
                    error = %detail,
                    "outbound fan-out: a channel failed but the message reached another, so it is not re-pushed"
                );
                return Ok(());
            }
            Err(NotifyError::new(detail))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::ChannelSet;
    use std::sync::{Arc, Mutex};

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
    fn undelivered_rows_dedupes_across_polls() {
        let mut seen = HashSet::new();
        let a = row("a", "ask_user_question", "/w/a", "{}");
        let b = row("b", "ask_user_question", "/w/b", "{}");

        // First poll: both are owed an attempt.
        let fresh = undelivered_rows(&seen, &[a.clone(), b.clone()]);
        assert_eq!(fresh.len(), 2);

        // Reading is PURE: nothing is retired until a delivery says so, so the
        // same call twice returns the same rows.
        assert_eq!(
            undelivered_rows(&seen, &[a.clone(), b.clone()]).len(),
            2,
            "the diff must not retire a row it merely looked at"
        );

        // Once they are recorded as delivered, they stop being owed.
        seen.insert("a".to_string());
        seen.insert("b".to_string());
        assert!(
            undelivered_rows(&seen, &[a.clone(), b.clone()]).is_empty(),
            "an already-delivered open row must not re-push"
        );

        // A brand-new open row is the only one still owed.
        let c = row("c", "escalation", "/w/c", "{}");
        let fresh = undelivered_rows(&seen, &[a, b, c]);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].id, "c");
    }

    /// A fake hangar daemon that accepts one connection, answers `auth/hello`,
    /// then answers `attention/list` with `rows`. Mirrors the framing in
    /// `daemon.rs`'s own test server.
    ///
    /// The listener is bound by the CALLER before this task is spawned, so the
    /// client can never dial a path that does not exist yet.
    async fn fake_daemon(listener: tokio::net::UnixListener, rows: serde_json::Value) {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let (stream, _) = listener.accept().await.expect("accept client");
        let (read_half, mut writer) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        // Read exactly two Content-Length frames (auth/hello, then the call) and
        // answer each.
        for (id, result) in [(1, serde_json::json!({})), (2, rows)] {
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.expect("read header line");
                let trimmed = line.trim_end_matches("\r\n");
                if trimmed.is_empty() {
                    break;
                }
                if let Some((name, v)) = trimmed.split_once(':') {
                    if name.trim().eq_ignore_ascii_case("Content-Length") {
                        len = v.trim().parse().expect("Content-Length parses");
                    }
                }
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).await.expect("read body");
            let payload = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
            let body = serde_json::to_vec(&payload).expect("serialize reply");
            let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
            frame.extend_from_slice(&body);
            writer.write_all(&frame).await.expect("write reply");
            writer.flush().await.expect("flush reply");
        }
    }

    fn attention_result(rows: &[AttentionRow]) -> serde_json::Value {
        serde_json::json!({ "attention": rows })
    }

    /// THE regression test for the shipped defect: the outbound worker existed,
    /// was fully unit-tested, and was never wired to anything, so a live,
    /// reachable daemon with a phone-routed ask delivered nothing, and the
    /// health surface had no way to know.
    ///
    /// This exercises the real wire path (unix socket, `auth/hello`,
    /// `attention/list`) and asserts BOTH halves of the contract: exactly one
    /// message reaches the channel, and the successful poll is stamped on the
    /// heartbeat so `ainb fleet daemons` can report it.
    #[tokio::test]
    async fn poll_once_pushes_the_phone_routed_row_and_records_the_poll() {
        let temp = tempfile::tempdir().expect("temp dir");
        let socket = temp.path().join("hangar.sock");
        let rows = vec![
            row(
                "ask-1",
                "ask_user_question",
                "/work/backend",
                r#"{"question":"Ship it?","options":["yes","no"]}"#,
            ),
            // Board-only: routed away from the phone, must not be delivered.
            row_on("board", "waiting", "/work/api", "{}", ChannelSet::NONE),
        ];
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind fake hangar socket");
        let server = tokio::spawn(fake_daemon(listener, attention_result(&rows)));

        let home = tempfile::tempdir().expect("temp home");
        let heartbeat = BridgeHeartbeat::start_in(home.path());
        let client = DaemonClient::with_parts(socket, "test-token".into());
        let notifier = RecordingNotifier(Mutex::new(Vec::new()));
        let mut seen = HashSet::new();

        let pushed = poll_once(&client, &notifier, &mut seen, &heartbeat)
            .await
            .expect("poll succeeds against the fake daemon");
        server.await.expect("fake daemon completes");

        assert_eq!(pushed, 1, "only the phone-routed row is pushed");
        let sent = notifier.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "exactly one message delivered: {sent:?}");
        assert!(sent[0].contains("asks: Ship it?"), "got: {}", sent[0]);

        let on_disk =
            crate::fleet::daemons::heartbeat::DaemonHeartbeat::read_in(home.path(), "bridge")
                .expect("heartbeat written");
        assert!(
            on_disk.last_attention_poll_at.is_some(),
            "a successful poll must be visible to the health probe"
        );
        assert!(on_disk.last_attention_error.is_none());
        assert!(
            on_disk.last_activity_at.is_some(),
            "an outbound push is real activity, not an idle bridge"
        );
    }

    /// A failed poll must be LOUD on the health surface. It used to be a
    /// `tracing::debug!` and nothing else, which at the default log level is
    /// indistinguishable from a working bridge.
    #[tokio::test]
    async fn poll_once_records_an_unreachable_daemon_on_the_heartbeat() {
        let temp = tempfile::tempdir().expect("temp dir");
        // No listener at this path: the exact "daemon is down" shape.
        let socket = temp.path().join("hangar.sock");

        let home = tempfile::tempdir().expect("temp home");
        let heartbeat = BridgeHeartbeat::start_in(home.path());
        let client = DaemonClient::with_parts(socket.clone(), "test-token".into());
        let notifier = RecordingNotifier(Mutex::new(Vec::new()));
        let mut seen = HashSet::new();

        let err = poll_once(&client, &notifier, &mut seen, &heartbeat)
            .await
            .expect_err("a missing socket must surface as an error");
        assert!(
            err.to_string().contains("hangar.sock"),
            "the error must name the socket: {err}"
        );
        assert!(notifier.0.lock().unwrap().is_empty());

        let on_disk =
            crate::fleet::daemons::heartbeat::DaemonHeartbeat::read_in(home.path(), "bridge")
                .expect("heartbeat written");
        assert!(on_disk.last_attention_poll_at.is_none());
        let recorded = on_disk.last_attention_error.clone().expect("attention error recorded");
        assert!(
            recorded.contains("hangar.sock"),
            "the recorded error must name the socket: {recorded}"
        );
        assert_eq!(
            on_disk.error_count, 1,
            "an unreachable attention source counts as an error"
        );

        // And the probe turns that record into a degraded row rather than the
        // green "running + connected" the operator used to see. Read it 60s into
        // the bridge's life: past one outbound poll window, still well inside the
        // 90s liveness window, so this isolates the outbound verdict.
        let read_at = on_disk.started_at + 60_000;
        let status = crate::fleet::daemons::probe::classify_heartbeat(
            crate::fleet::daemons::probe::DaemonKind::Bridge,
            Some(on_disk),
            crate::fleet::daemons::heartbeat::PidCheck::Matched,
            read_at,
        );
        assert_eq!(
            status.state,
            crate::fleet::daemons::probe::DaemonState::Degraded
        );
        assert!(
            status.reason.contains("hangar.sock"),
            "the degraded reason must name the socket: {}",
            status.reason
        );
    }

    /// The de-dupe must survive across polls when driven through `poll_once`
    /// (the loop's real entry point), not only through `take_new_rows`.
    #[tokio::test]
    async fn poll_once_does_not_re_push_a_row_that_is_still_open() {
        let temp = tempfile::tempdir().expect("temp dir");
        let socket = temp.path().join("hangar.sock");
        let rows = vec![row(
            "ask-1",
            "ask_user_question",
            "/work/backend",
            r#"{"question":"Ship it?"}"#,
        )];
        let home = tempfile::tempdir().expect("temp home");
        let heartbeat = BridgeHeartbeat::start_in(home.path());
        let client = DaemonClient::with_parts(socket.clone(), "test-token".into());
        let notifier = RecordingNotifier(Mutex::new(Vec::new()));
        let mut seen = HashSet::new();

        for expected in [1usize, 0usize] {
            let listener =
                tokio::net::UnixListener::bind(&socket).expect("bind fake hangar socket");
            let server = tokio::spawn(fake_daemon(listener, attention_result(&rows)));
            let pushed = poll_once(&client, &notifier, &mut seen, &heartbeat)
                .await
                .expect("poll succeeds");
            server.await.expect("fake daemon completes");
            assert_eq!(pushed, expected);
            std::fs::remove_file(&socket).expect("free the socket path for the next poll");
        }
        assert_eq!(
            notifier.0.lock().unwrap().len(),
            1,
            "an open row is pushed once, not once per tick"
        );
    }

    /// THE P1 regression test: a poll that reaches the daemon but whose CHANNEL
    /// SEND fails must not look like a delivered push.
    ///
    /// The exact reported shape: hangar raises a phone-routed ASK, the poll
    /// succeeds, and the Discord send then fails (429, or the DM cannot be
    /// opened because the user has server DMs disabled). Before this, the row
    /// went into `seen` before the send, `notify` returned `()` so the failure
    /// was swallowed into a counter the probe never reads, and `record_relay`
    /// fired on "pushed > 0". Net effect: the ask was never re-pushed (the row
    /// stays open, so the prune keeps it in `seen` forever), LAST ACTIVITY
    /// claimed work that did not happen, and the row rendered
    /// "running + connected (outbound push live)" while the human got nothing.
    ///
    /// All three halves of that are asserted here: retried, no false activity,
    /// degraded health.
    #[tokio::test]
    async fn poll_once_with_a_failing_channel_retries_the_row_and_degrades_health() {
        let temp = tempfile::tempdir().expect("temp dir");
        let socket = temp.path().join("hangar.sock");
        let rows = vec![row(
            "ask-1",
            "ask_user_question",
            "/work/backend",
            r#"{"question":"Ship it?","options":["yes","no"]}"#,
        )];
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind fake hangar socket");
        let server = tokio::spawn(fake_daemon(listener, attention_result(&rows)));

        let home = tempfile::tempdir().expect("temp home");
        let heartbeat = BridgeHeartbeat::start_in(home.path());
        // The inbound chat gateway is perfectly healthy: this is the green row
        // the operator used to see.
        heartbeat.set_connected(true, Some("Discord (gateway)".into()));
        let client = DaemonClient::with_parts(socket, "test-token".into());
        let notifier = FailingNotifier::new("Discord: HTTP 429 rate limited");
        let mut seen = HashSet::new();

        let delivered = poll_once(&client, &notifier, &mut seen, &heartbeat)
            .await
            .expect("the poll itself succeeds; only the send fails");
        server.await.expect("fake daemon completes");

        assert_eq!(notifier.attempts(), 1, "the push was attempted");
        assert_eq!(delivered, 0, "but nothing reached the human");
        assert!(
            !seen.contains("ask-1"),
            "an undelivered row must stay out of the de-dupe set or it is never \
             retried: {seen:?}"
        );

        let on_disk =
            crate::fleet::daemons::heartbeat::DaemonHeartbeat::read_in(home.path(), "bridge")
                .expect("heartbeat written");
        assert!(
            on_disk.last_activity_at.is_none(),
            "an attempted-but-undelivered push is not activity: LAST ACTIVITY \
             must not claim work the human never received"
        );
        let recorded = on_disk
            .last_delivery_error
            .clone()
            .expect("a failed delivery must reach the sticky field the probe reads");
        assert!(
            recorded.contains("HTTP 429"),
            "the recorded failure must name the cause: {recorded}"
        );
        assert_eq!(
            on_disk.error_count, 1,
            "an undelivered push counts as an error exactly once"
        );

        // And the probe turns that into a degraded row. Read it 10s in: the
        // attention poll DID succeed and is well inside its window, so without
        // the delivery verdict this classifies as "outbound push live".
        let read_at = on_disk.started_at + 10_000;
        let status = crate::fleet::daemons::probe::classify_heartbeat(
            crate::fleet::daemons::probe::DaemonKind::Bridge,
            Some(on_disk),
            crate::fleet::daemons::heartbeat::PidCheck::Matched,
            read_at,
        );
        assert_eq!(
            status.state,
            crate::fleet::daemons::probe::DaemonState::Degraded,
            "a push that never reached the human must never render as healthy: {}",
            status.reason
        );
        assert!(
            status.reason.contains("HTTP 429"),
            "the degraded reason must name the channel failure: {}",
            status.reason
        );
    }

    /// The other half of the retry contract: once the channel recovers, the row
    /// that failed IS pushed again, the human gets it, and the bridge clears
    /// its own degraded verdict without a restart.
    #[tokio::test]
    async fn poll_once_retries_an_undelivered_row_until_it_lands() {
        /// Fails the first send, takes every one after: a transient 429.
        struct RecoveringNotifier {
            sent: Mutex<Vec<String>>,
            attempts: Mutex<usize>,
        }
        impl Notifier for RecoveringNotifier {
            fn notify(
                &self,
                text: String,
            ) -> Pin<Box<dyn Future<Output = Result<(), NotifyError>> + Send + '_>> {
                let mut attempts = self.attempts.lock().unwrap();
                *attempts += 1;
                let first = *attempts == 1;
                drop(attempts);
                if !first {
                    self.sent.lock().unwrap().push(text);
                }
                Box::pin(async move {
                    if first {
                        Err(NotifyError::new("Discord: HTTP 429 rate limited"))
                    } else {
                        Ok(())
                    }
                })
            }
        }

        let temp = tempfile::tempdir().expect("temp dir");
        let socket = temp.path().join("hangar.sock");
        let rows = vec![row(
            "ask-1",
            "ask_user_question",
            "/work/backend",
            r#"{"question":"Ship it?"}"#,
        )];
        let home = tempfile::tempdir().expect("temp home");
        let heartbeat = BridgeHeartbeat::start_in(home.path());
        heartbeat.set_connected(true, Some("Discord (gateway)".into()));
        let client = DaemonClient::with_parts(socket.clone(), "test-token".into());
        let notifier = RecoveringNotifier {
            sent: Mutex::new(Vec::new()),
            attempts: Mutex::new(0),
        };
        let mut seen = HashSet::new();

        // Two polls over the same still-open row: the first send fails, the
        // second lands.
        let mut delivered_per_poll = Vec::new();
        for _ in 0..2 {
            let listener =
                tokio::net::UnixListener::bind(&socket).expect("bind fake hangar socket");
            let server = tokio::spawn(fake_daemon(listener, attention_result(&rows)));
            delivered_per_poll.push(
                poll_once(&client, &notifier, &mut seen, &heartbeat)
                    .await
                    .expect("poll succeeds"),
            );
            server.await.expect("fake daemon completes");
            std::fs::remove_file(&socket).expect("free the socket path for the next poll");
        }

        assert_eq!(
            delivered_per_poll,
            vec![0, 1],
            "the row the first poll failed to deliver is retried by the second"
        );
        assert_eq!(*notifier.attempts.lock().unwrap(), 2);
        assert_eq!(notifier.sent.lock().unwrap().len(), 1);
        assert!(seen.contains("ask-1"), "now delivered, so now de-duped");

        let on_disk =
            crate::fleet::daemons::heartbeat::DaemonHeartbeat::read_in(home.path(), "bridge")
                .expect("heartbeat written");
        assert!(
            on_disk.last_delivery_error.is_none(),
            "a landed retry clears the sticky verdict: {:?}",
            on_disk.last_delivery_error
        );
        assert!(
            on_disk.last_activity_at.is_some(),
            "the delivery that DID land is real activity"
        );
        let read_at = on_disk.started_at + 10_000;
        let status = crate::fleet::daemons::probe::classify_heartbeat(
            crate::fleet::daemons::probe::DaemonKind::Bridge,
            Some(on_disk),
            crate::fleet::daemons::heartbeat::PidCheck::Matched,
            read_at,
        );
        assert_eq!(
            status.state,
            crate::fleet::daemons::probe::DaemonState::Running,
            "the bridge recovers on its own once the push lands: {}",
            status.reason
        );
    }

    #[tokio::test]
    async fn fanout_delivers_to_every_configured_channel() {
        let a = Arc::new(RecordingNotifier(Mutex::new(Vec::new())));
        let b = Arc::new(RecordingNotifier(Mutex::new(Vec::new())));
        let fanout = Fanout::new(vec![Box::new(a.clone()), Box::new(b.clone())]);
        assert!(!fanout.is_empty());
        fanout
            .notify("session api asks: go?".to_string())
            .await
            .expect("both channels took the message");
        assert_eq!(a.0.lock().unwrap().len(), 1);
        assert_eq!(b.0.lock().unwrap().len(), 1);
        assert!(Fanout::new(Vec::new()).is_empty());
    }

    /// The fan-out's delivery verdict. Nobody reached = a real failure the
    /// worker must retry and the health surface must show; somebody reached =
    /// the human was told, so re-pushing to the working channel every poll
    /// would be worse than the partial miss (which is still logged).
    #[tokio::test]
    async fn fanout_fails_only_when_no_channel_took_the_message() {
        let ok = Arc::new(RecordingNotifier(Mutex::new(Vec::new())));
        let bad = Arc::new(FailingNotifier::new("Slack HTTP 500"));

        let partial = Fanout::new(vec![Box::new(ok.clone()), Box::new(bad.clone())]);
        assert!(
            partial.notify("session api asks: go?".into()).await.is_ok(),
            "one channel delivered, so the human was told"
        );
        assert_eq!(ok.0.lock().unwrap().len(), 1);

        let all_bad = Fanout::new(vec![
            Box::new(Arc::new(FailingNotifier::new("Slack HTTP 500"))),
            Box::new(Arc::new(FailingNotifier::new("Discord HTTP 429"))),
        ]);
        let err = all_bad
            .notify("session api asks: go?".into())
            .await
            .expect_err("no channel took it, so nobody was told");
        assert!(err.detail().contains("Slack HTTP 500"), "got: {err}");
        assert!(err.detail().contains("Discord HTTP 429"), "got: {err}");
    }

    struct RecordingNotifier(Mutex<Vec<String>>);

    impl Notifier for RecordingNotifier {
        fn notify(
            &self,
            text: String,
        ) -> Pin<Box<dyn Future<Output = Result<(), NotifyError>> + Send + '_>> {
            self.0.lock().unwrap().push(text);
            Box::pin(async { Ok(()) })
        }
    }

    /// The fan-out holds `Box<dyn Notifier>`, so the recorder needs a shared
    /// handle the test can still read after it is boxed.
    impl Notifier for Arc<RecordingNotifier> {
        fn notify(
            &self,
            text: String,
        ) -> Pin<Box<dyn Future<Output = Result<(), NotifyError>> + Send + '_>> {
            self.0.lock().unwrap().push(text);
            Box::pin(async { Ok(()) })
        }
    }

    /// A channel that is reachable enough to be ATTEMPTED and always fails the
    /// send: the Discord-429 / DMs-disabled / revoked-token shape. It records
    /// every attempt so a test can prove the row is retried rather than
    /// silently retired.
    struct FailingNotifier {
        attempts: Mutex<Vec<String>>,
        detail: &'static str,
    }

    impl FailingNotifier {
        fn new(detail: &'static str) -> Self {
            Self {
                attempts: Mutex::new(Vec::new()),
                detail,
            }
        }

        fn attempts(&self) -> usize {
            self.attempts.lock().unwrap().len()
        }
    }

    impl Notifier for FailingNotifier {
        fn notify(
            &self,
            text: String,
        ) -> Pin<Box<dyn Future<Output = Result<(), NotifyError>> + Send + '_>> {
            self.attempts.lock().unwrap().push(text);
            Box::pin(async { Err(NotifyError::new(self.detail)) })
        }
    }

    impl Notifier for Arc<FailingNotifier> {
        fn notify(
            &self,
            text: String,
        ) -> Pin<Box<dyn Future<Output = Result<(), NotifyError>> + Send + '_>> {
            self.attempts.lock().unwrap().push(text);
            Box::pin(async { Err(NotifyError::new(self.detail)) })
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
        let outcome = dispatch(&rows, &notifier).await;
        let sent = notifier.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        assert!(sent[0].contains("asks: go?"));
        assert!(sent[1].contains("hit an error: boom"));
        assert_eq!(outcome.delivered, 2);
        assert_eq!(outcome.settled, vec!["a1".to_string(), "e1".to_string()]);
        assert!(outcome.failure.is_none());
    }

    /// A channel that refuses the send must NOT settle the row: the ask is
    /// still owed to the human, so the worker has to be told to retry it.
    #[tokio::test]
    async fn dispatch_reports_a_failed_push_and_settles_nothing() {
        let notifier = FailingNotifier::new("HTTP 429 rate limited");
        let rows = [
            row("a1", "ask_user_question", "/w/a", r#"{"question":"go?"}"#),
            // Board-only: never attempted, so it settles without a send.
            row_on("board", "waiting", "/w/wait", "{}", ChannelSet::NONE),
        ];
        let outcome = dispatch(&rows, &notifier).await;
        assert_eq!(notifier.attempts(), 1, "the phone-routed row is attempted");
        assert_eq!(outcome.delivered, 0, "nothing reached the human");
        assert_eq!(
            outcome.settled,
            vec!["board".to_string()],
            "only the row that was never owed a push is settled: {:?}",
            outcome.settled
        );
        assert_eq!(
            outcome.failure.as_ref().map(NotifyError::detail),
            Some("HTTP 429 rate limited")
        );
    }

    /// One bad channel must not stop the pass: the rows behind it are still
    /// attempted, and the ones that land are still settled.
    #[tokio::test]
    async fn dispatch_keeps_going_past_a_failing_row() {
        struct FlakyNotifier(Mutex<Vec<String>>);
        impl Notifier for FlakyNotifier {
            fn notify(
                &self,
                text: String,
            ) -> Pin<Box<dyn Future<Output = Result<(), NotifyError>> + Send + '_>> {
                self.0.lock().unwrap().push(text.clone());
                Box::pin(async move {
                    if text.contains("boom") {
                        Err(NotifyError::new("send failed"))
                    } else {
                        Ok(())
                    }
                })
            }
        }
        let notifier = FlakyNotifier(Mutex::new(Vec::new()));
        let rows = [
            row("e1", "error", "/w/e", r#"{"message":"boom"}"#),
            row("a1", "ask_user_question", "/w/a", r#"{"question":"go?"}"#),
        ];
        let outcome = dispatch(&rows, &notifier).await;
        assert_eq!(
            notifier.0.lock().unwrap().len(),
            2,
            "a failed row must not abort the rest of the pass"
        );
        assert_eq!(outcome.delivered, 1);
        assert_eq!(outcome.settled, vec!["a1".to_string()]);
        assert!(outcome.failure.is_some());
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
        let outcome = dispatch(&rows, &notifier).await;
        let sent = notifier.0.lock().unwrap().clone();
        assert_eq!(
            sent.len(),
            1,
            "only the phone-routed row is pushed: {sent:?}"
        );
        assert_eq!(
            outcome.settled.len(),
            3,
            "a row that was never owed a push is settled too, or it would be \
             re-examined forever: {:?}",
            outcome.settled
        );
        assert_eq!(outcome.delivered, 1);
        assert!(
            sent[0].contains("escalated"),
            "the delivered row is the escalation: {sent:?}"
        );
    }
}
