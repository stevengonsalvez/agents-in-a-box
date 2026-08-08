//! The attention ingest producer (spec P2, D10) — the daemon's own tail of the
//! durable hook event log (`events.jsonl`) into the `attention` table.
//!
//! The lifecycle hook (`ainb fleet atc hook`) appends one JSON line per Claude
//! event to `~/.agents-in-a-box/events.jsonl`. notifyd already ingests that file
//! into ITS sqlite for OS notifications; this is the SECOND, independent consumer
//! the converged control centre needs — the one that folds every input-request
//! into the answerable attention inbox. It owns its OWN byte-offset cursor and
//! reads the shared file directly, rather than cross-reading notifyd's rusqlite,
//! so the T2 store boundary holds (the daemon is sqlx-only; it never touches
//! another crate's DB).
//!
//! ## Pipeline
//!
//! For each qualifying hook line (`Notification` / `Stop` / `SubagentStop` — the
//! events that can mean "this session needs a human"), the producer runs the
//! SAME needs classifier the fleet panel uses ([`classify`], ASK > ERR > IDLE >
//! WAIT), and — when it fires — inserts a durable `attention` row and emits an
//! `AttentionRaised` nudge on the fleet-wide stream.
//!
//! The one exception is a Claude AskUserQuestion, which is read from the hook
//! payload instead ([`ask_context_from_hook_payload`]): Claude withholds the
//! picker's `tool_use` row from the transcript until the tool RESOLVES, so a
//! transcript read while the question is open finds nothing and no interview
//! could ever raise a card.
//!
//! ## Idempotency
//!
//! The attention id uses the hook's durable event ID:
//! `att:<session>:<event_id>`. Legacy lines fall back to their absolute byte
//! offset. The event ID is stable across replay, so:
//!
//!   - A re-read of the SAME line (a crash between insert and the cursor write, a
//!     best-effort cursor-write failure, or a corrupt/missing cursor that resets
//!     the read to 0) has the SAME id → the insert is skipped → no
//!     duplicate card. Re-reading from 0 raises each request exactly once, so the
//!     cursor is a pure efficiency optimisation, not a correctness dependency.
//!   - A genuinely NEW occurrence (a fresh hook line — the same question asked
//!     again after the first was answered, or a recurring error) is appended at a
//!     NEW offset → a NEW id → a fresh row, even when its request context is
//!     byte-for-byte identical to a prior one.
//!
//! Event IDs avoid context hashes, whose time-derived fields can change when a
//! line is re-classified. Legacy offsets remain invariant to read time.
//!
//! The event ID alone is NOT enough for an ASK, because Claude re-fires
//! `Notification` while a session stays blocked and each firing is a genuinely
//! new event: one live question was measured raising three cards. ASK rows
//! therefore also carry a `request_key` ([`request_key_of`], migration 0081):
//! every firing that observes the SAME still-open question derives the same key
//! and collapses onto the first row. Once that row is closed the key is free
//! again, so a repeat of the same question is still raisable.
//!
//! ## Convergence
//!
//! [`AttentionIngest::sweep_once`] closes open rows no live Fleet session
//! claims. The `attention` table and `fleet_session.attention_state` are two
//! independent records of "needs input" with no cross-writes; without a
//! reconcile they drift apart indefinitely (732 open rows against 7 waiting
//! sessions, measured live).
//!
//! ## Delivery semantics
//!
//! Best-effort: a missing file is a no-op and a corrupt line is skipped. A store
//! fault leaves the cursor before the failed line so a later pass replays it.
//! A failed ingest never downs the daemon.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use ainb_fleet_core::read::jsonl_tail::ask_data_from_tool_input;
use ainb_fleet_core::read::needs::{ClassifyInput, NeedsContext, classify};
use ainb_fleet_core::types::{Session, SessionSource};
use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use ainb_hangar_store::repo::fleet::FleetRepo;
use ainb_hangar_store::repo::fleet_provider_event::{
    FleetProviderEventRepo, NewFleetProviderEvent,
};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::events::EventSink;

/// Maximum bytes read from the un-ingested suffix in one pass. Bounds peak memory
/// regardless of how far behind the cursor is; the remainder is picked up next
/// tick. Mirrors notifyd's ingest bound.
const MAX_INGEST_BYTES: u64 = 4 * 1024 * 1024;

/// How often the producer tails the event log.
const TICK: std::time::Duration = std::time::Duration::from_secs(3);

/// How often the producer reconciles the open set against Fleet's own view.
/// Slow on purpose: the sweep is a convergence backstop, not a hot path.
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// How long a row is protected from the sweep. A card raised seconds ago may
/// legitimately lead its `fleet_session` projection, and closing it would delete
/// a live question.
const SWEEP_GRACE_MS: i64 = 15 * 60 * 1000;

/// Rows one sweep pass may close. Bounds the pass so a pathological backlog
/// cannot stall the tail loop; the remainder goes in the next pass.
const SWEEP_LIMIT: i64 = 2000;

/// The hook events worth classifying for attention. `Notification` is Claude
/// asking for input / permission (the ASK path); `Stop` / `SubagentStop` mark a
/// finished turn (the possible IDLE path). Other hook events (tool use, prompt
/// submit) never indicate a session is blocked, so they are skipped without a
/// transcript read.
fn is_qualifying(event_type: &str) -> bool {
    matches!(
        event_type,
        "AskUserQuestion" | "PermissionRequest" | "Notification" | "Stop" | "SubagentStop"
    )
}

/// The subset of the canonical hook line the producer needs. Lenient: unknown
/// fields are ignored and missing ones default, so a format the hook grows never
/// breaks the tail.
#[derive(Debug, Deserialize)]
struct HookEventLine {
    #[serde(default, deserialize_with = "null_as_default")]
    event_id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    raw_payload_ref: String,
    #[serde(default, deserialize_with = "null_as_default")]
    ts: i64,
    #[serde(default, deserialize_with = "null_as_default")]
    session_id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    cwd: String,
    #[serde(default, deserialize_with = "null_as_default")]
    transcript_path: String,
    #[serde(default, deserialize_with = "null_as_default")]
    event_type: String,
    #[serde(default, deserialize_with = "null_as_default")]
    matcher: String,
    #[serde(default, deserialize_with = "null_as_default")]
    agent: String,
}

/// Deserialize a field that may be `null` into its `Default`.
///
/// `#[serde(default)]` alone only covers an ABSENT key — an EXPLICIT `null` on a
/// non-`Option` field is still a hard type error that fails the whole line. The
/// hook writes its optional fields as explicit nulls (`"matcher":null`,
/// `"transcript_path":null`), so without this every such line failed to parse and
/// was silently dropped as "corrupt" — an `AskUserQuestion` from a session with
/// no matcher never reached the attention inbox at all. notifyd's reader models
/// the same fields as `Option<String>` and was unaffected, which is why the
/// divergence went unnoticed.
fn null_as_default<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(de)?.unwrap_or_default())
}

/// The attention ingest producer — owns the paths + the write handles.
pub struct AttentionIngest {
    pool: SqlitePool,
    events: EventSink,
    /// The shared hook event log (`~/.agents-in-a-box/events.jsonl`).
    events_jsonl: PathBuf,
    /// This producer's OWN durable byte-offset cursor (a plain u64 text file).
    cursor_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineOutcome {
    Processed,
    Raised,
    Retry,
}

impl AttentionIngest {
    /// Construct the producer over the shared event log + this producer's cursor.
    #[must_use]
    pub fn new(
        pool: SqlitePool,
        events: EventSink,
        events_jsonl: PathBuf,
        cursor_path: PathBuf,
    ) -> Self {
        Self {
            pool,
            events,
            events_jsonl,
            cursor_path,
        }
    }

    /// Tail the event log from the durable cursor, classify each qualifying line,
    /// raise attention rows, and advance the cursor. Returns how many rows were
    /// raised this pass. Never errors — every fault degrades to a skip + a log.
    pub async fn ingest_once(&self, now_ms: i64) -> usize {
        let mut start = read_cursor(&self.cursor_path);

        let mut file = match std::fs::File::open(&self.events_jsonl) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return 0,
            Err(e) => {
                tracing::warn!(error = %e, "attention ingest: cannot open events.jsonl");
                return 0;
            }
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        // Rotation / truncation: the file shrank below our cursor, so the cursor
        // points into stale bytes. Reset to 0 and re-read the new file (the
        // content-addressed id keeps re-reads idempotent).
        if len < start {
            start = 0;
        }
        if len <= start {
            return 0;
        }

        if file.seek(SeekFrom::Start(start)).is_err() {
            return 0;
        }
        let to_read = (len - start).min(MAX_INGEST_BYTES) as usize;
        let mut buf = vec![0u8; to_read];
        if let Err(e) = file.read_exact(&mut buf) {
            tracing::warn!(error = %e, "attention ingest: short read");
            return 0;
        }

        // Only consume up to the last complete line; a partial trailing line (no
        // terminating '\n') is left for the next pass. Byte-level so the cursor
        // advance is exact even across invalid UTF-8.
        let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else {
            return 0; // no complete line yet
        };
        let end = last_nl + 1;

        let mut raised = 0;
        let mut committed_end = 0usize;
        // Track each line's absolute byte offset for legacy event identity.
        // `split_inclusive` keeps the delimiter, avoiding a synthetic extra byte
        // after the final newline.
        let mut line_start = 0usize;
        for segment in buf[..end].split_inclusive(|&b| b == b'\n') {
            let offset = start + line_start as u64;
            line_start += segment.len();
            let line_bytes = segment.strip_suffix(b"\n").unwrap_or(segment);
            if line_bytes.is_empty() {
                committed_end = line_start;
                continue;
            }
            if let Ok(line) = std::str::from_utf8(line_bytes) {
                match self.process_line(line, offset, now_ms).await {
                    LineOutcome::Raised => {
                        raised += 1;
                        committed_end = line_start;
                    }
                    LineOutcome::Processed => committed_end = line_start,
                    LineOutcome::Retry => break,
                }
            } else {
                committed_end = line_start;
            }
        }
        write_cursor(&self.cursor_path, start + committed_end as u64);
        raised
    }

    /// Process one hook line. Store faults return `Retry`, leaving the cursor
    /// before this line so the next pass replays it through idempotent event IDs.
    async fn process_line(&self, raw: &str, offset: u64, now_ms: i64) -> LineOutcome {
        let Ok(line) = serde_json::from_str::<HookEventLine>(raw) else {
            return LineOutcome::Processed;
        };
        let event_id = (!line.event_id.is_empty())
            .then_some(line.event_id.clone())
            .unwrap_or_else(|| format!("legacy-hook:{}:{offset}", line.session_id));
        let mut payload =
            serde_json::from_str::<serde_json::Value>(raw).unwrap_or(serde_json::Value::Null);
        let raw_payload = match self.raw_payload(&line, &event_id, &payload) {
            Ok(payload) => payload,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(error = %error, event_id, "fleet provider event sidecar missing; using envelope payload");
                payload
                    .get("payload")
                    .map(serde_json::Value::to_string)
                    .unwrap_or_else(|| payload.to_string())
            }
            Err(error) => {
                tracing::warn!(error = %error, "fleet provider event payload unavailable");
                return LineOutcome::Retry;
            }
        };
        if let Ok(raw_value) = serde_json::from_str::<serde_json::Value>(&raw_payload) {
            if let Some(envelope) = payload.as_object_mut() {
                envelope.insert("payload".to_string(), raw_value);
            }
        }
        let provider = if line.agent.is_empty() {
            "unknown"
        } else {
            line.agent.as_str()
        };
        let session_key =
            (!line.session_id.is_empty()).then(|| format!("{provider}:{}", line.session_id));
        if FleetProviderEventRepo::append(
            &self.pool,
            &NewFleetProviderEvent {
                event_id: event_id.clone(),
                provider: provider.to_string(),
                source: match provider {
                    "claude" => "claude_hook",
                    "codex" => "codex_hook",
                    _ => "hook",
                }
                .to_string(),
                session_key,
                provider_session_id: (!line.session_id.is_empty()).then(|| line.session_id.clone()),
                observed_at: if line.ts > 0 { line.ts } else { now_ms },
                received_at: now_ms,
                event_type: line.event_type.clone(),
                raw_payload,
            },
        )
        .await
        .is_err()
        {
            tracing::warn!("fleet provider event persistence failed");
            return LineOutcome::Retry;
        }
        if line.cwd.is_empty() && line.session_id.is_empty() {
            return LineOutcome::Processed;
        }

        // Every hook line feeds the canonical Fleet reducer, including events
        // that do not raise an attention card. The source event ID is replay-safe;
        // legacy lines use their durable byte offset.
        let semantic_event = if line.event_type == "PreToolUse" && line.matcher == "AskUserQuestion"
        {
            "AskUserQuestion"
        } else {
            line.event_type.as_str()
        };
        if !line.session_id.is_empty() {
            let observation = crate::fleet::HookObservation {
                event_id: event_id.clone(),
                provider: &line.agent,
                provider_session_id: &line.session_id,
                cwd: &line.cwd,
                event_type: semantic_event,
                payload: &payload,
                observed_at: if line.ts > 0 { line.ts } else { now_ms },
            };
            let reduced =
                match crate::fleet::apply_hook(&self.pool, &self.events, observation).await {
                    Ok(result) => result,
                    Err(error) => {
                        tracing::warn!(error = %error, "fleet hook reduce failed");
                        return LineOutcome::Retry;
                    }
                };
            if let Err(error) =
                FleetProviderEventRepo::mark_projected(&self.pool, &event_id, reduced.revision)
                    .await
            {
                tracing::warn!(error = %error, "fleet provider event projection link failed");
                return LineOutcome::Retry;
            }
            // Transcript writes commonly accompany hook events. The usage service
            // coalesces this into one background scan, so Fleet converges without
            // needing the TUI or a client-side filesystem read.
            crate::fleet_usage::request_refresh().await;
            crate::fleet_quota::request_refresh().await;
        }

        if !is_qualifying(semantic_event) {
            return LineOutcome::Processed;
        }

        // An AskUserQuestion is classified from the hook line that ANNOUNCED it,
        // not from the transcript. Claude does not append the `tool_use` row for
        // the picker until the tool RESOLVES, so while the question is genuinely
        // open the transcript cannot describe it: probed live at every lookback
        // window (20/40/80/160/320) against a blocked session: `None` at all
        // five. The transcript classifier therefore never fired for a live
        // interview and the `attention` table held zero ASK rows, ever. The hook
        // payload carries the full `tool_input` at open time, so it is the only
        // producer that can see the question while it still needs an answer.
        let context = match ask_context_from_hook_payload(&line.agent, semantic_event, &payload) {
            Some(ask) => ask,
            // Every other signal (Stop/Notification/an ask observed after the
            // fact, and every non-Claude producer) still classifies from the
            // transcript exactly as before.
            None => {
                // Classify off the async runtime: `classify` reads the JSONL
                // transcript + does blocking fs I/O, so it must not run inline on
                // a tokio worker.
                let session = session_from(&line);
                let Some(row) = tokio::task::spawn_blocking(move || {
                    classify(ClassifyInput::from_env(session, None, now_ms))
                })
                .await
                .ok()
                .flatten() else {
                    return LineOutcome::Processed;
                };
                row.context
            }
        };

        let kind = kind_of(&context);

        // Stale-ASK reconcile (spec P2 open/close ordering). A session that is no
        // longer asking (the classifier returned anything but Ask) may still carry
        // an OPEN ASK row from an earlier Notification: the question was answered /
        // timed out / interrupted IN the live session, which never routes through
        // the hangar answer router, so nothing ever closed that row. Close it
        // BEFORE raising this follow-on card, or the stale ASK sits open + answerable
        // beside the new Waiting/Idle/Err card, duplicating the session's attention
        // signal (and a hangar surface could try to answer an already-answered ask).
        //
        // But a non-Ask classification is only trustworthy once the session has
        // actually stopped asking. It comes from the transcript, which cannot see
        // an AskUserQuestion until the tool RESOLVES, so a session still blocked on
        // a live question reads IDLE off its last FINISHED turn as soon as that
        // turn is 5 minutes old. Acting on that reading would close the card the
        // payload producer raised seconds earlier AND stand a Waiting card up in
        // its place, while the hook is still blocked waiting for the answer. Fleet's
        // own projection (written from this same hook line a few lines above)
        // is the authority on whether the request is still live, so while it says
        // one is, this line yields no attention decision at all.
        if kind != AttentionKind::AskUserQuestion {
            match FleetRepo::provider_session_holds_open_request(&self.pool, &line.session_id).await
            {
                Ok(true) => return LineOutcome::Processed,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "attention ingest: live-request check failed");
                    return LineOutcome::Retry;
                }
            }
            self.reconcile_stale_asks(&line.session_id, now_ms).await;
        }

        // Event-keyed id: a replay keeps the same id and is skipped. New source
        // events receive a new id even when request context is identical. Legacy
        // records retain offset identity rather than a time-derived context hash.
        let id = format!("att:{}:{event_id}", line.session_id);

        // Resolve the routing channels ONCE, here at raise time (tcp T5). Hook
        // sessions are host-wide (workspace None), so this reads the GLOBAL rule
        // for the kind. Stamped onto the row + the event so every consumer filters
        // on the same decision.
        let channels = crate::notify::resolve_channels(&self.pool, kind, None).await;

        let context = serde_json::to_value(&context)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
        let request_key = request_key_of(kind, &context);
        let payload = context.to_string();
        let new = NewAttention {
            id: id.clone(),
            session_id: line.session_id.clone(),
            cwd: line.cwd.clone(),
            // Hook sessions are host-wide; the fleet-wide control centre owns them.
            // (Resolving cwd→ainb workspace is a later enrichment.)
            workspace_id: None,
            kind,
            payload,
            // Hook-sourced = full fidelity (the degraded flag is for the
            // unhooked pane-classifier fallback, a separate producer).
            degraded: false,
            created_at: now_ms,
            // The exact transcript this session was writing — the session-stable
            // token the answer router's C1 guard binds cwd-fallback delivery to.
            raise_transcript: (!line.transcript_path.is_empty())
                .then(|| line.transcript_path.clone()),
            channels,
        };
        match AttentionRepo::insert_if_absent(&self.pool, &new, request_key.as_deref()).await {
            // Already raised: a replay of this durable line, or an earlier firing
            // of the SAME still-open question. Either way there is nothing new to
            // announce, and re-emitting would resurrect a dismissed card.
            Ok(false) => return LineOutcome::Processed,
            Ok(true) => {}
            Err(e) => {
                tracing::warn!(error = %e, "attention ingest: insert failed");
                return LineOutcome::Retry;
            }
        }
        self.events.emit_attention(HangarEvent::AttentionRaised {
            attention_id: id,
            session_id: line.session_id,
            workspace_id: None,
            kind: kind.as_str().to_string(),
            degraded: false,
            created_at: now_ms,
            channels,
        });
        LineOutcome::Raised
    }

    /// Return the exact payload sidecar when the hook stored one. Legacy lines
    /// have no sidecar, so their bounded inline payload remains replayable.
    fn raw_payload(
        &self,
        line: &HookEventLine,
        event_id: &str,
        envelope: &serde_json::Value,
    ) -> Result<String, std::io::Error> {
        if line.raw_payload_ref.is_empty() {
            return Ok(envelope
                .get("payload")
                .map(serde_json::Value::to_string)
                .unwrap_or_else(|| envelope.to_string()));
        }
        if line.raw_payload_ref != event_id
            || !line
                .raw_payload_ref
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid raw payload reference",
            ));
        }
        let Some(home) = self.events_jsonl.parent() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "hook event log has no parent",
            ));
        };
        let sidecar = std::fs::read_to_string(
            home.join("hangar")
                .join("provider-events")
                .join(format!("{}.json", line.raw_payload_ref)),
        )?;
        // An empty sidecar is a torn / never-filled write, not a payload. Report
        // it as absent so the caller falls back to the durable inline envelope —
        // storing "" would record it as the EXACT source payload and lose the
        // real one silently.
        if sidecar.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "empty raw payload sidecar",
            ));
        }
        Ok(sidecar)
    }

    /// Close any OPEN ASK rows a session still carries once a later hook shows it
    /// is no longer asking. The live AskUserQuestion was answered / timed out /
    /// interrupted in the session (never through the hangar answer router), so no
    /// [`AttentionRepo::mark_answered_if_open`] ever fired for it. Each close goes
    /// through that SAME first-answer-wins flip — so a concurrent human answer
    /// racing on the row is never clobbered — and emits an `AttentionAnswered`
    /// nudge so live surfaces drop the stale card without waiting for a re-pull.
    /// Best-effort: a lookup / close fault is logged and skipped, never fatal.
    async fn reconcile_stale_asks(&self, session_id: &str, now_ms: i64) {
        if session_id.is_empty() {
            return;
        }
        let ids = match AttentionRepo::open_ask_ids_for_session(&self.pool, session_id).await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "attention ingest: stale-ASK lookup failed");
                return;
            }
        };
        for id in ids {
            match AttentionRepo::mark_answered_if_open(
                &self.pool,
                &id,
                "resolved:session",
                "answered in session",
                now_ms,
            )
            .await
            {
                // We flipped it: the ASK was still open and is now closed.
                Ok(1) => {
                    self.events.emit_attention(HangarEvent::AttentionAnswered {
                        attention_id: id,
                        by: "resolved:session".to_string(),
                    });
                }
                // A surface won the answer race first — already closed, nothing to do.
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "attention ingest: stale-ASK close failed");
                }
            }
        }
    }

    /// Close open rows that no live Fleet session still claims.
    ///
    /// The `attention` table and `fleet_session.attention_state` are two
    /// independent records of "needs input" that never cross-write, so they
    /// drift: measured live at 732 open rows against 7 sessions Fleet believed
    /// were waiting, the oldest 25 days stale. Nothing else closes a `waiting`
    /// row at all, so this runs on a slow tick rather than once at boot: a
    /// one-shot pass would clear today's backlog and let it re-accumulate.
    ///
    /// Best-effort: a store fault is logged and retried next interval.
    async fn sweep_once(&self, now_ms: i64) {
        match AttentionRepo::close_unclaimed_open(
            &self.pool,
            now_ms - SWEEP_GRACE_MS,
            now_ms,
            SWEEP_LIMIT,
        )
        .await
        {
            Ok(0) => {}
            Ok(closed) => {
                tracing::info!(closed, "attention sweep: closed rows no session claims");
            }
            Err(e) => tracing::warn!(error = %e, "attention sweep failed"),
        }
    }

    /// Spawn the tail loop: tick every [`TICK`], ingesting each pass and
    /// reconciling the open set every [`SWEEP_INTERVAL`]. Mirrors the inbox
    /// aggregator: the returned handle is dropped by `boot()` (process exit
    /// tears the task down); a future supervisor can keep it to stop cleanly.
    #[must_use]
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        use ainb_hangar_core::clock::{HangarClock as _, SystemClock};
        tokio::spawn(async move {
            let clock = SystemClock;
            let mut ticker = tokio::time::interval(TICK);
            let mut next_sweep = tokio::time::Instant::now();
            loop {
                ticker.tick().await;
                let now_ms = clock.now_ms();
                if tokio::time::Instant::now() >= next_sweep {
                    self.sweep_once(now_ms).await;
                    next_sweep = tokio::time::Instant::now() + SWEEP_INTERVAL;
                }
                let _ = self.ingest_once(now_ms).await;
            }
        })
    }
}

/// Build the classifier's [`Session`] from a hook line. `tmux_session` is left
/// `None` — the hook does not carry it; the answer router re-discovers the live
/// tmux target at answer time.
fn session_from(line: &HookEventLine) -> Session {
    Session {
        id: line.session_id.clone(),
        cwd: line.cwd.clone(),
        pid: None,
        git_root: None,
        tmux_session: None,
        workspace_name: None,
        worktree_path: None,
        peer_id: None,
        bg_job_id: None,
        transcript_path: (!line.transcript_path.is_empty()).then(|| line.transcript_path.clone()),
        sources: vec![SessionSource::Ainb],
        summary: None,
        last_seen_ms: None,
    }
}

/// The open `AskUserQuestion` a Claude hook line OPENS, read straight off the
/// payload the hook already carries, or `None` when the line does not open one.
///
/// `payload.tool_input` is the SAME `{"questions":[…]}` object the transcript
/// later stores as the `tool_use` block's `input`, and both are parsed by
/// [`ask_data_from_tool_input`], so a hook-derived ask and a transcript-derived
/// ask for one question produce byte-identical context, and therefore the same
/// [`request_key_of`].
///
/// Only the picker-OPEN event qualifies (`PreToolUse` for the AskUserQuestion
/// tool, which the caller has already canonicalised to `AskUserQuestion`).
/// Claude also re-announces a live picker as `PermissionRequest` +
/// `Notification`: measured live, 15s after the open, when Fleet releases the
/// question back to Claude's own picker. Those describe the SAME picker, not a
/// new one; `crate::fleet::apply_hook` makes exactly this call for the session
/// projection (`duplicate_claude_structured_permission`) and the attention
/// inbox must agree. Raising off them re-opened a card that the release had
/// just closed as `native_claude`, advertising a Fleet answer route that no
/// longer existed: observed live in the sandbox before this gate.
///
/// One case genuinely loses its only signal: when the hook cannot register with
/// the broker at all it returns early WITHOUT appending the `PreToolUse` line,
/// so a permission re-announcement is the only trace of the question. That is
/// the right trade: with no broker there is no answer route for a card to
/// offer, and the session's `fleet_session` row still shows it needs a human.
///
/// Claude-only, matching the producing hook and the reducer: the answer route a
/// card advertises is the Claude structured broker, so another provider that
/// happened to name a tool `AskUserQuestion` must not mint one.
fn ask_context_from_hook_payload(
    agent: &str,
    semantic_event: &str,
    envelope: &serde_json::Value,
) -> Option<NeedsContext> {
    if agent != "claude" || semantic_event != "AskUserQuestion" {
        return None;
    }
    let payload = envelope.get("payload")?;
    if payload.get("tool_name").and_then(serde_json::Value::as_str) != Some("AskUserQuestion") {
        return None;
    }
    ask_data_from_tool_input(payload.get("tool_input")?).map(NeedsContext::Ask)
}

/// The stable identity of the request an ASK row is about, or `None` for kinds
/// that have no request identity to collapse on.
///
/// Claude re-fires `Notification` while a session stays blocked, and every
/// firing carries a fresh hook `event_id`, so an event-keyed row id mints a new
/// card per firing: one live question was measured producing three. The
/// classifier reads the SAME still-open `AskUserQuestion` out of the transcript
/// on every one of those firings, so a hash of the classified request collapses
/// them onto one row (see migration 0080).
///
/// Derived from the CLASSIFIED context, not the hook payload, because the
/// `Notification` line that raises most ASK rows carries neither `tool_use_id`
/// nor `tool_input`, the fields `fleet_session.current_request_fingerprint` is
/// built from. The two values are therefore NOT comparable, which is why the
/// column is `request_key` and not `request_fingerprint`.
///
/// Only `Ask` qualifies: an `Idle` context carries a time-derived
/// `idle_minutes`, so its hash would change with the read clock and dedupe
/// nothing, and `Wait`/`Err` describe a session state rather than a request.
fn request_key_of(kind: AttentionKind, context: &serde_json::Value) -> Option<String> {
    (kind == AttentionKind::AskUserQuestion)
        .then(|| ainb_plugin_notifyd::broker::request_fingerprint(context))
}

/// Map a classified need to its attention kind.
fn kind_of(ctx: &NeedsContext) -> AttentionKind {
    match ctx {
        NeedsContext::Ask(_) => AttentionKind::AskUserQuestion,
        NeedsContext::Err(_) => AttentionKind::Error,
        // An idle-at-prompt or an explicit WAITING marker are both "waiting on a
        // human" from the inbox's point of view.
        NeedsContext::Idle(_) | NeedsContext::Wait(_) => AttentionKind::Waiting,
    }
}

/// Read the durable byte cursor; `0` when the file is missing or unparseable
/// (a fresh producer, or a corrupt cursor — re-reading from 0 is idempotent).
fn read_cursor(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Persist the byte cursor atomically (temp + rename). Best-effort — a write
/// fault only means the next pass re-reads a little (the id dedup absorbs it).
fn write_cursor(path: &Path, offset: u64) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if std::fs::write(&tmp, offset.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_store::Store;
    use std::io::Write;

    /// Plant a transcript under `~/.claude/projects/<slug>` for a UNIQUE cwd so
    /// `classify` resolves it, returning the fabricated cwd + a cleanup guard.
    /// Mirrors the fleet needs-classifier test fixture.
    struct TranscriptFixture {
        cwd: String,
        dir: std::path::PathBuf,
    }
    impl Drop for TranscriptFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    fn plant_ask_transcript(tag: &str) -> TranscriptFixture {
        let cwd = format!(
            "/ainb-test-att-ingest/{tag}/{}/{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut dir = dirs::home_dir().expect("home dir");
        dir.push(".claude");
        dir.push("projects");
        let slug = ainb_fleet_core::read::jsonl_tail::cwd_to_project_slug(&cwd);
        dir.push(&slug);
        std::fs::create_dir_all(&dir).expect("create project dir");
        let mut f = std::fs::File::create(dir.join("session.jsonl")).expect("create transcript");
        // An AskUserQuestion tool_use block — the strongest classifier signal.
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"AskUserQuestion","input":{{"questions":[{{"question":"Ship it?","options":[{{"label":"yes"}},{{"label":"no"}}]}}]}}}}]}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        TranscriptFixture { cwd, dir }
    }

    /// Plant an IDLE transcript (a finished `end_turn` assistant text row stamped
    /// at `ts_ms`) under a UNIQUE cwd, so `classify` returns IDLE with a
    /// TIME-DERIVED `idle_minutes` that depends on the `now_ms` it is read at.
    fn plant_idle_transcript(tag: &str, ts_ms: i64) -> TranscriptFixture {
        let cwd = format!(
            "/ainb-test-att-ingest/{tag}/{}/{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut dir = dirs::home_dir().expect("home dir");
        dir.push(".claude");
        dir.push("projects");
        let slug = ainb_fleet_core::read::jsonl_tail::cwd_to_project_slug(&cwd);
        dir.push(&slug);
        std::fs::create_dir_all(&dir).expect("create project dir");
        let iso = chrono::DateTime::from_timestamp_millis(ts_ms).unwrap().to_rfc3339();
        let mut f = std::fs::File::create(dir.join("session.jsonl")).expect("create transcript");
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"stop_reason":"end_turn","content":[{{"type":"text","text":"All done."}}]}},"timestamp":"{iso}"}}"#
        )
        .unwrap();
        TranscriptFixture { cwd, dir }
    }

    /// Plant a transcript where an AskUserQuestion was RAISED then ANSWERED (a
    /// paired `tool_result`), followed by a finished `end_turn` assistant turn
    /// at `ts_ms`. After the sticky-ASK fix this classifies IDLE (not ASK), so a
    /// `Stop` hook over it raises a Waiting row — never a stale AskUserQuestion.
    fn plant_answered_ask_then_idle_transcript(tag: &str, ts_ms: i64) -> TranscriptFixture {
        let cwd = format!(
            "/ainb-test-att-ingest/{tag}/{}/{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut dir = dirs::home_dir().expect("home dir");
        dir.push(".claude");
        dir.push("projects");
        let slug = ainb_fleet_core::read::jsonl_tail::cwd_to_project_slug(&cwd);
        dir.push(&slug);
        std::fs::create_dir_all(&dir).expect("create project dir");
        let iso = chrono::DateTime::from_timestamp_millis(ts_ms).unwrap().to_rfc3339();
        let mut f = std::fs::File::create(dir.join("session.jsonl")).expect("create transcript");
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"stop_reason":"tool_use","content":[{{"type":"tool_use","id":"toolu_ans","name":"AskUserQuestion","input":{{"questions":[{{"question":"Scope?","options":[{{"label":"a"}}]}}]}}}}]}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"toolu_ans","content":"Your questions have been answered."}}]}},"timestamp":"2026-01-01T00:00:01Z"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"stop_reason":"end_turn","content":[{{"type":"text","text":"All done."}}]}},"timestamp":"{iso}"}}"#
        )
        .unwrap();
        TranscriptFixture { cwd, dir }
    }

    /// Plant a transcript with a single OPEN AskUserQuestion that carries a
    /// tool_use `id` (so a later paired `tool_result` can close it). The stale-ASK
    /// reconcile regression appends the answer + a finished turn to this file
    /// between ingest passes to drive the classifier from ASK to IDLE.
    fn plant_open_ask_with_id_transcript(tag: &str) -> TranscriptFixture {
        let cwd = format!(
            "/ainb-test-att-ingest/{tag}/{}/{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut dir = dirs::home_dir().expect("home dir");
        dir.push(".claude");
        dir.push("projects");
        let slug = ainb_fleet_core::read::jsonl_tail::cwd_to_project_slug(&cwd);
        dir.push(&slug);
        std::fs::create_dir_all(&dir).expect("create project dir");
        let mut f = std::fs::File::create(dir.join("session.jsonl")).expect("create transcript");
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"stop_reason":"tool_use","content":[{{"type":"tool_use","id":"toolu_rec","name":"AskUserQuestion","input":{{"questions":[{{"question":"Ship it?","options":[{{"label":"yes"}}]}}]}}}}]}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        TranscriptFixture { cwd, dir }
    }

    /// One hook line in the EXACT shape `ainb fleet atc hook` appends —
    /// including the explicit `null`s it writes for the optional fields. The old
    /// helper omitted those keys entirely, so the unit suite was green against a
    /// shape the hook never emits while every real line failed to parse.
    fn hook_line(session: &str, cwd: &str, event_type: &str) -> String {
        format!(
            r#"{{"agent":"claude","cwd":"{cwd}","event_type":"{event_type}","matcher":null,"parent":"hangar-daemon","process_start_fingerprint":null,"session_id":"{session}","tmux_target":null,"transcript_path":"","ts":1700000000000}}"#
        )
    }

    /// One hook line carrying an EXPLICIT durable `event_id`, the shape the
    /// hook writes for every real firing (a fresh `Uuid::new_v4()` each time).
    fn hook_line_with_event_id(
        session: &str,
        cwd: &str,
        event_type: &str,
        event_id: &str,
    ) -> String {
        format!(
            r#"{{"agent":"claude","cwd":"{cwd}","event_id":"{event_id}","event_type":"{event_type}","matcher":null,"parent":"hangar-daemon","process_start_fingerprint":null,"session_id":"{session}","tmux_target":null,"transcript_path":"","ts":1700000000000}}"#
        )
    }

    /// A verbatim real hook line — nulls and all — parses, rather than being
    /// discarded as corrupt.
    #[test]
    fn real_hook_line_with_explicit_nulls_parses() {
        let raw = r#"{"agent":"claude","cwd":"/w","event_type":"Notification","matcher":null,"parent":"hangar-daemon","process_start_fingerprint":null,"session_id":"sid-1","tmux_target":null,"transcript_path":null,"ts":1784921161073}"#;
        let line = serde_json::from_str::<HookEventLine>(raw)
            .expect("a real hook line must parse (explicit nulls included)");
        assert_eq!(line.session_id, "sid-1");
        assert_eq!(line.event_type, "Notification");
        assert!(line.matcher.is_empty());
        assert!(line.transcript_path.is_empty());
    }

    fn ingest_for(store: &Store, events_jsonl: &Path, cursor: &Path) -> AttentionIngest {
        let broker = crate::events::EventBroker::new();
        AttentionIngest::new(
            store.pool().clone(),
            broker.sink(),
            events_jsonl.to_path_buf(),
            cursor.to_path_buf(),
        )
    }

    #[tokio::test]
    async fn qualifying_ask_line_raises_one_attention_row_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let fx = plant_ask_transcript("ask");

        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        std::fs::write(
            &events_jsonl,
            format!("{}\n", hook_line("sid-1", &fx.cwd, "Notification")),
        )
        .unwrap();

        let ingest = ingest_for(&store, &events_jsonl, &cursor);
        let raised = ingest.ingest_once(5000).await;
        assert_eq!(raised, 1, "the Notification line classifies to one ASK row");

        let open = AttentionRepo::list_fleet(store.pool()).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].kind, AttentionKind::AskUserQuestion);
        assert_eq!(open[0].session_id, "sid-1");
        assert_eq!(open[0].cwd, fx.cwd);
        assert!(!open[0].degraded, "hook-sourced rows are full fidelity");

        // A second pass over the same (already-consumed) log raises nothing — the
        // cursor advanced AND the offset-keyed id dedups.
        let again = ingest.ingest_once(6000).await;
        assert_eq!(again, 0, "no duplicate row on a re-tail");
        assert_eq!(
            AttentionRepo::list_fleet(store.pool()).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn reread_from_zero_at_a_later_clock_does_not_duplicate_a_time_derived_row() {
        // A corrupt/missing cursor (or a crash before the cursor write) re-reads
        // the whole file. An IDLE row's context carries a TIME-DERIVED
        // `idle_minutes`, so a context-hash id would change between the two reads
        // and mint a spurious duplicate. The offset-keyed id is invariant to the
        // read clock, so the re-read dedups.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let base_ms = 1_700_000_000_000_i64;
        let fx = plant_idle_transcript("idle", base_ms);

        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        std::fs::write(
            &events_jsonl,
            format!("{}\n", hook_line("sid-idle", &fx.cwd, "Stop")),
        )
        .unwrap();

        let ingest = ingest_for(&store, &events_jsonl, &cursor);
        // First read, 10 min after the turn ended → IDLE(idle_minutes=10).
        let raised = ingest.ingest_once(base_ms + 10 * 60_000).await;
        assert_eq!(raised, 1, "the Stop line classifies to one IDLE row");
        assert_eq!(
            AttentionRepo::list_fleet(store.pool()).await.unwrap().len(),
            1
        );

        // Simulate a lost cursor: re-read from 0, now 20 min after the turn end →
        // IDLE(idle_minutes=20). A time-varying context hash would differ; the
        // offset key does not, so no duplicate row is raised.
        std::fs::remove_file(&cursor).ok();
        let again = ingest.ingest_once(base_ms + 20 * 60_000).await;
        assert_eq!(
            again, 0,
            "a later-clock re-read of the same line raises no duplicate"
        );
        assert_eq!(
            AttentionRepo::list_fleet(store.pool()).await.unwrap().len(),
            1,
            "still exactly one IDLE row after the re-read"
        );
    }

    #[tokio::test]
    async fn repeat_firings_for_one_open_question_raise_one_card() {
        // The observed triplicate: ONE live question, three cards, because Claude
        // re-fires `Notification` while a session stays blocked and the hook mints
        // a fresh `event_id` per firing. The classifier reads the SAME still-open
        // AskUserQuestion out of the transcript every time, so the request key
        // collapses the later firings onto the first card.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let fx = plant_ask_transcript("refire");

        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        std::fs::write(
            &events_jsonl,
            format!(
                "{}\n{}\n{}\n",
                hook_line_with_event_id(
                    "sid-refire",
                    &fx.cwd,
                    "Notification",
                    "f489417c-7995-41d4-af9c-5a5b12c9df48"
                ),
                hook_line_with_event_id(
                    "sid-refire",
                    &fx.cwd,
                    "Notification",
                    "6f238c73-a952-49f3-8595-a060040d29c6"
                ),
                hook_line_with_event_id(
                    "sid-refire",
                    &fx.cwd,
                    "Notification",
                    "0d64ec43-f2ef-4f90-9d9f-a111397945b0"
                ),
            ),
        )
        .unwrap();

        let ingest = ingest_for(&store, &events_jsonl, &cursor);
        assert_eq!(
            ingest.ingest_once(5000).await,
            1,
            "three firings of one question raise ONE card"
        );
        let open = AttentionRepo::list_fleet(store.pool()).await.unwrap();
        assert_eq!(open.len(), 1, "one question is one card");
        assert_eq!(open[0].kind, AttentionKind::AskUserQuestion);
        assert_eq!(
            open[0].id, "att:sid-refire:f489417c-7995-41d4-af9c-5a5b12c9df48",
            "the FIRST firing owns the card, so its wait time is the real one"
        );
    }

    /// A VERBATIM hook line captured from a live Claude interview, retyped only
    /// in the fields the assertion needs. `event_id`, the question text and the
    /// `tool_use_id` vary per firing; everything else is exactly what
    /// `ainb-hooks` wrote to `events.jsonl` on 2026-08-08.
    fn live_ask_hook_line(
        event_id: &str,
        hook_event: &str,
        question: &str,
        tool_use_id: Option<&str>,
    ) -> String {
        live_ask_hook_line_in(
            "d39ec648",
            "/private/tmp/sbxwork",
            event_id,
            hook_event,
            question,
            tool_use_id,
        )
    }

    /// The same captured line, re-homed onto a caller-chosen session + cwd so a
    /// planted transcript can be reached through the normal cwd→project lookup.
    fn live_ask_hook_line_in(
        session: &str,
        cwd: &str,
        event_id: &str,
        hook_event: &str,
        question: &str,
        tool_use_id: Option<&str>,
    ) -> String {
        let id = tool_use_id.map(|id| format!(r#","tool_use_id":"{id}""#)).unwrap_or_default();
        format!(
            r#"{{"event_id":"{event_id}","ts":1786222389057,"session_id":"{session}","cwd":"{cwd}","transcript_path":"","agent":"claude","event_type":"{hook_event}","matcher":"AskUserQuestion","parent":null,"tmux_target":"sbx-claude:1.1","process_start_fingerprint":"pane=%5853;pid=39567","payload":{{"session_id":"{session}","cwd":"{cwd}","permission_mode":"bypassPermissions","hook_event_name":"{hook_event}","tool_name":"AskUserQuestion","tool_input":{{"questions":[{{"question":"{question}","header":"XRAY","options":[{{"label":"Alpha","description":"Route Alpha."}},{{"label":"Bravo","description":"Route Bravo."}}],"multiSelect":false}}]}}{id}}}}}"#
        )
    }

    #[tokio::test]
    async fn a_live_open_question_survives_a_follow_on_hook_that_reads_idle() {
        // The stale-ASK reconcile closes every open ASK row for a session as soon
        // as the transcript classifier returns anything but `Ask`, on the premise
        // that "not asking any more" is observable from the transcript. This
        // producer's whole reason to exist is that the premise is FALSE while a
        // question is open: Claude withholds the tool_use row until the tool
        // resolves, so a still-blocked session reads IDLE off its last finished
        // turn. Left ungated, the reconcile would delete the very card that was
        // raised seconds earlier and replace it with a Waiting card, while the
        // hook is still blocked waiting for the answer.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let base_ms = 1_700_000_000_000_i64;
        // The last FINISHED turn, well past the 5-minute idle threshold. The open
        // question that followed it is invisible to the transcript.
        let fx = plant_idle_transcript("still-open", base_ms);

        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        let ingest = ingest_for(&store, &events_jsonl, &cursor);

        std::fs::write(
            &events_jsonl,
            format!(
                "{}\n",
                live_ask_hook_line_in(
                    "sid-open",
                    &fx.cwd,
                    "evt-ask",
                    "PreToolUse",
                    "still waiting on you",
                    Some("toolu-open"),
                )
            ),
        )
        .unwrap();
        assert_eq!(ingest.ingest_once(base_ms + 10 * 60_000).await, 1);
        let ask_id = "att:sid-open:evt-ask";

        // A later hook line for the SAME session while the question is still open.
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&events_jsonl).unwrap();
            writeln!(f, "{}", hook_line("sid-open", &fx.cwd, "Notification")).unwrap();
        }
        ingest.ingest_once(base_ms + 11 * 60_000).await;

        let row = AttentionRepo::get(store.pool(), ask_id).await.unwrap().unwrap();
        assert_eq!(
            row.state, "open",
            "the live question must survive: the session is still blocked on it"
        );
        let open = AttentionRepo::list_fleet(store.pool()).await.unwrap();
        assert_eq!(open.len(), 1, "and it must not be joined by a Waiting card");
        assert_eq!(open[0].kind, AttentionKind::AskUserQuestion);
    }

    #[tokio::test]
    async fn live_interview_hook_raises_an_ask_row_without_any_transcript() {
        // THE defect: `attention` held zero rows across every real interview.
        // Claude withholds the AskUserQuestion `tool_use` row from the transcript
        // until the tool resolves, so the transcript classifier returned None for
        // every live question and the ingest minted nothing. This test plants NO
        // transcript at all (`transcript_path` points at a file that does not
        // exist), so it passes ONLY if the hook payload is what raises the card.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        std::fs::write(
            &events_jsonl,
            format!(
                "{}\n",
                live_ask_hook_line(
                    "66a17c06-4952-422e-ba13-e63b1562ed23",
                    "PreToolUse",
                    "pick a route XRAY",
                    Some("toolu_01L1eKD2ietPMh89c34SgbeK"),
                )
            ),
        )
        .unwrap();

        let ingest = ingest_for(&store, &events_jsonl, &cursor);
        assert_eq!(
            ingest.ingest_once(5000).await,
            1,
            "the PreToolUse that OPENS the picker must raise the card"
        );

        let open = AttentionRepo::list_fleet(store.pool()).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].kind, AttentionKind::AskUserQuestion);
        assert_eq!(open[0].session_id, "d39ec648");
        assert!(
            open[0].payload.contains("pick a route XRAY"),
            "the card carries the real question: {}",
            open[0].payload
        );
        assert!(
            open[0].payload.contains("Alpha") && open[0].payload.contains("Bravo"),
            "the card carries the answerable options: {}",
            open[0].payload
        );
    }

    #[tokio::test]
    async fn repeat_openings_of_one_still_open_question_collapse_onto_one_card() {
        // The migration-0082 `request_key` collapse, on the payload producer: two
        // picker-open firings carrying the SAME question but a fresh `event_id`
        // (a re-announced tool call, or a replay after a log rotation) are one
        // need, so they must share one card rather than mint two.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        std::fs::write(
            &events_jsonl,
            format!(
                "{}\n{}\n",
                live_ask_hook_line(
                    "66a17c06-4952-422e-ba13-e63b1562ed23",
                    "PreToolUse",
                    "pick a route XRAY",
                    Some("toolu_01L1eKD2ietPMh89c34SgbeK"),
                ),
                // No `tool_use_id`: the key must come from the QUESTION, not the
                // tool call, or a re-announcement would not collapse.
                live_ask_hook_line(
                    "d614a4ba-5588-45ce-95c4-39d6e031baa1",
                    "PreToolUse",
                    "pick a route XRAY",
                    None,
                ),
            ),
        )
        .unwrap();

        let ingest = ingest_for(&store, &events_jsonl, &cursor);
        assert_eq!(
            ingest.ingest_once(5000).await,
            1,
            "two firings of one open question raise ONE card"
        );
        let open = AttentionRepo::list_fleet(store.pool()).await.unwrap();
        assert_eq!(open.len(), 1, "one question is one card");
        assert_eq!(
            open[0].id, "att:d39ec648:66a17c06-4952-422e-ba13-e63b1562ed23",
            "the FIRST firing owns the card, so its wait time is the real one"
        );
    }

    #[tokio::test]
    async fn permission_reannouncement_never_resurrects_a_released_question() {
        // Observed live in the sandbox: Fleet released an open interview to
        // Claude's own picker (card closed `native_claude`), and 15s later Claude
        // re-announced the SAME question as PermissionRequest:AskUserQuestion.
        // The close had freed the request key, so the re-announcement minted a
        // fresh OPEN card: re-advertising a Fleet answer route that the release
        // had just retired. A PermissionRequest describes the picker that is
        // already open, never a new one, so it must raise nothing.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        let ingest = ingest_for(&store, &events_jsonl, &cursor);

        std::fs::write(
            &events_jsonl,
            format!(
                "{}\n",
                live_ask_hook_line(
                    "evt-open",
                    "PreToolUse",
                    "route check ECHO2",
                    Some("t-echo")
                )
            ),
        )
        .unwrap();
        assert_eq!(ingest.ingest_once(5000).await, 1);

        // Fleet hands the question back to Claude's picker.
        AttentionRepo::mark_answered_if_open(
            store.pool(),
            "att:d39ec648:evt-open",
            "native_claude",
            "released to Claude native picker",
            6000,
        )
        .await
        .unwrap();

        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&events_jsonl).unwrap();
            writeln!(
                f,
                "{}",
                live_ask_hook_line("evt-perm", "PermissionRequest", "route check ECHO2", None)
            )
            .unwrap();
        }
        assert_eq!(
            ingest.ingest_once(7000).await,
            0,
            "the permission re-announcement must not raise a card"
        );
        assert!(
            AttentionRepo::list_fleet(store.pool()).await.unwrap().is_empty(),
            "the released question stays closed: Fleet can no longer answer it"
        );
        let row = AttentionRepo::get(store.pool(), "att:d39ec648:evt-open")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.answered_by.as_deref(), Some("native_claude"));
    }

    #[tokio::test]
    async fn a_second_distinct_question_raises_its_own_card() {
        // The dedupe must collapse RE-FIRINGS, not distinct interviews. Two
        // different questions in one session are two separate needs.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        std::fs::write(
            &events_jsonl,
            format!(
                "{}\n{}\n",
                live_ask_hook_line("evt-xray", "PreToolUse", "pick a route XRAY", Some("t-1")),
                live_ask_hook_line(
                    "evt-yankee",
                    "PreToolUse",
                    "pick a route YANKEE",
                    Some("t-2")
                ),
            ),
        )
        .unwrap();

        let ingest = ingest_for(&store, &events_jsonl, &cursor);
        assert_eq!(ingest.ingest_once(5000).await, 2);
        assert_eq!(
            AttentionRepo::list_fleet(store.pool()).await.unwrap().len(),
            2,
            "two different questions are two cards"
        );
    }

    #[tokio::test]
    async fn sweep_closes_open_rows_no_fleet_session_claims() {
        // The drift the two representations never reconciled: 732 open rows
        // against 7 sessions Fleet believed were waiting.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let base_ms = 1_700_000_000_000_i64;
        sqlx::query(
            "INSERT INTO fleet_session \
             (session_key, provider, provider_session_id, attention_state, discovered_at, \
              last_observed_at) \
             VALUES ('claude:live', 'claude', 'live', 'ASK', 0, 0)",
        )
        .execute(store.pool())
        .await
        .unwrap();
        for (id, session, created) in [
            ("keep-live", "live", base_ms - SWEEP_GRACE_MS - 1),
            ("close-gone", "gone", base_ms - SWEEP_GRACE_MS - 1),
            ("keep-fresh", "gone", base_ms - 1000),
        ] {
            AttentionRepo::insert(
                store.pool(),
                &NewAttention {
                    id: id.to_string(),
                    session_id: session.to_string(),
                    cwd: "/w".to_string(),
                    workspace_id: None,
                    kind: AttentionKind::Waiting,
                    payload: "{}".to_string(),
                    degraded: false,
                    created_at: created,
                    raise_transcript: None,
                    channels: ainb_hangar_core::channel::ChannelSet::NONE,
                },
            )
            .await
            .unwrap();
        }

        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        ingest_for(&store, &events_jsonl, &cursor).sweep_once(base_ms).await;

        let open: Vec<_> = AttentionRepo::list_fleet(store.pool())
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(
            open,
            ["keep-live", "keep-fresh"],
            "only the stale row whose session is gone is closed"
        );
    }

    #[tokio::test]
    async fn non_qualifying_event_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let fx = plant_ask_transcript("skip");

        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        // A PostToolUse event is not a qualifying attention signal — even though
        // the transcript WOULD classify as ASK, the gate skips it (no wasteful
        // transcript read, no row).
        std::fs::write(
            &events_jsonl,
            format!("{}\n", hook_line("sid-2", &fx.cwd, "PostToolUse")),
        )
        .unwrap();

        let ingest = ingest_for(&store, &events_jsonl, &cursor);
        assert_eq!(ingest.ingest_once(5000).await, 0);
        assert!(AttentionRepo::list_fleet(store.pool()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn raw_sidecar_reaches_source_ledger_without_jsonl_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        let event_id = "source-sidecar-1";
        let raw_payload = format!(r#"{{"tool_input":{{"body":"{}"}}}}"#, "x".repeat(8 * 1024));
        let payload_dir = dir.path().join("hangar").join("provider-events");
        std::fs::create_dir_all(&payload_dir).unwrap();
        std::fs::write(payload_dir.join(format!("{event_id}.json")), &raw_payload).unwrap();
        std::fs::write(
            &events_jsonl,
            format!(
                r#"{{"agent":"claude","cwd":"/w","event_id":"{event_id}","event_type":"SessionStart","matcher":null,"raw_payload_ref":"{event_id}","session_id":"sid-sidecar","transcript_path":null,"ts":1700000000000,"payload":{{"_truncated":true}}}}"#,
            ) + "\n",
        )
        .unwrap();
        let ingest = ingest_for(&store, &events_jsonl, &cursor);

        assert_eq!(ingest.ingest_once(5_000).await, 0);
        let source = FleetProviderEventRepo::get(store.pool(), event_id)
            .await
            .unwrap()
            .expect("source envelope must persist");
        assert_eq!(source.raw_payload, raw_payload);
        assert!(source.projection_revision.is_some());
    }

    #[tokio::test]
    async fn empty_sidecar_falls_back_to_inline_envelope_payload() {
        // A 0-byte sidecar is a torn / never-filled write. It must NOT be stored
        // as the exact source payload — the durable inline envelope wins, exactly
        // as it does when the sidecar file is missing outright.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        let event_id = "source-sidecar-empty";
        let payload_dir = dir.path().join("hangar").join("provider-events");
        std::fs::create_dir_all(&payload_dir).unwrap();
        std::fs::write(payload_dir.join(format!("{event_id}.json")), "").unwrap();
        std::fs::write(
            &events_jsonl,
            format!(
                r#"{{"agent":"claude","cwd":"/w","event_id":"{event_id}","event_type":"SessionStart","matcher":null,"raw_payload_ref":"{event_id}","session_id":"sid-empty","transcript_path":null,"ts":1700000000000,"payload":{{"inline":"kept"}}}}"#,
            ) + "\n",
        )
        .unwrap();
        let ingest = ingest_for(&store, &events_jsonl, &cursor);

        assert_eq!(ingest.ingest_once(5_000).await, 0);
        let source = FleetProviderEventRepo::get(store.pool(), event_id)
            .await
            .unwrap()
            .expect("source envelope must persist");
        assert_eq!(
            source.raw_payload, r#"{"inline":"kept"}"#,
            "an empty sidecar must not overwrite the inline envelope payload"
        );
        assert!(source.projection_revision.is_some());
    }

    #[tokio::test]
    async fn fleet_store_failure_leaves_cursor_before_line_for_replay() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        std::fs::write(
            &events_jsonl,
            format!("{}\n", hook_line("sid-retry", "/tmp/retry", "SessionStart")),
        )
        .unwrap();

        let ingest = ingest_for(&store, &events_jsonl, &cursor);
        store.pool().close().await;

        assert_eq!(ingest.ingest_once(5000).await, 0);
        assert_eq!(
            read_cursor(&cursor),
            0,
            "failed Fleet persistence must not consume the durable hook line"
        );
    }

    #[tokio::test]
    async fn partial_trailing_line_is_left_for_the_next_pass() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let fx = plant_ask_transcript("partial");

        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        // Write a complete line + a partial (no trailing newline) line.
        let complete = hook_line("sid-3", &fx.cwd, "Notification");
        std::fs::write(&events_jsonl, format!("{complete}\n{{\"partial\":")).unwrap();

        let ingest = ingest_for(&store, &events_jsonl, &cursor);
        assert_eq!(
            ingest.ingest_once(5000).await,
            1,
            "only the complete line ingests"
        );
        // The cursor stopped at the newline; it did not consume the partial tail.
        assert_eq!(read_cursor(&cursor), (complete.len() + 1) as u64);
    }

    #[tokio::test]
    async fn answered_ask_then_stop_raises_waiting_not_sticky_ask() {
        // Regression for the sticky-ASK-forever bug at the daemon-ingest seam: a
        // session that ANSWERED its interview and then finished a turn must NOT be
        // re-raised as an AskUserQuestion. classify() now sees the paired
        // tool_result → falls through to IDLE → the ingest raises a Waiting row.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let base_ms = 1_700_000_000_000_i64;
        let fx = plant_answered_ask_then_idle_transcript("answered", base_ms);

        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        std::fs::write(
            &events_jsonl,
            format!("{}\n", hook_line("sid-answered", &fx.cwd, "Stop")),
        )
        .unwrap();

        let ingest = ingest_for(&store, &events_jsonl, &cursor);
        // 10 min after the finished turn → IDLE (> the 5-min default threshold).
        let raised = ingest.ingest_once(base_ms + 10 * 60_000).await;
        assert_eq!(
            raised, 1,
            "the Stop line classifies to one IDLE→Waiting row"
        );
        let open = AttentionRepo::list_fleet(store.pool()).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(
            open[0].kind,
            AttentionKind::Waiting,
            "an answered ask must not re-raise as AskUserQuestion (sticky-ASK fix)"
        );
    }

    #[tokio::test]
    async fn notification_then_answer_then_stop_closes_stale_ask_no_duplicate_card() {
        // W1 open/close-ordering regression. The sticky-ASK classifier fix stops a
        // NEW ASK being re-raised, but the ASK row raised while the question was
        // genuinely open is never closed when the human answers IN the session (no
        // hangar answer router runs). Sequence: Notification raises an open ASK →
        // the human answers in-session (a tool_result + a finished turn land in the
        // transcript) → a later Stop classifies IDLE. The ingest must close the
        // stale ASK as it raises the Waiting card, so the session shows exactly ONE
        // open card, never a stale-ASK-beside-Waiting duplicate.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let base_ms = 1_700_000_000_000_i64;
        let fx = plant_open_ask_with_id_transcript("reconcile");
        let transcript = fx.dir.join("session.jsonl");

        let events_jsonl = dir.path().join("events.jsonl");
        let cursor = dir.path().join("attention_ingest.offset");
        let ingest = ingest_for(&store, &events_jsonl, &cursor);

        // Pass 1 — a Notification over the OPEN-ask transcript raises the ASK row.
        std::fs::write(
            &events_jsonl,
            format!("{}\n", hook_line("sid-rec", &fx.cwd, "Notification")),
        )
        .unwrap();
        assert_eq!(
            ingest.ingest_once(base_ms).await,
            1,
            "Notification raises the ASK"
        );
        let raised = AttentionRepo::list_fleet(store.pool()).await.unwrap();
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].kind, AttentionKind::AskUserQuestion);
        let ask_id = raised[0].id.clone();

        // The human answers IN the session: a paired tool_result closes the ask in
        // the transcript and a finished end_turn follows. No hangar answer router
        // ran, so the ASK attention row is STILL open in the store.
        let iso = chrono::DateTime::from_timestamp_millis(base_ms).unwrap().to_rfc3339();
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&transcript).unwrap();
            writeln!(
                f,
                r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"toolu_rec","content":"Your questions have been answered."}}]}},"timestamp":"2026-01-01T00:00:01Z"}}"#
            )
            .unwrap();
            writeln!(
                f,
                r#"{{"type":"assistant","message":{{"stop_reason":"end_turn","content":[{{"type":"text","text":"All done."}}]}},"timestamp":"{iso}"}}"#
            )
            .unwrap();
        }

        // Pass 2 — a Stop over the now-idle transcript (10 min later, past the IDLE
        // threshold). The classifier reads IDLE (the ask has a paired tool_result),
        // so the ingest raises a Waiting card AND reconciles the stale ASK closed.
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&events_jsonl).unwrap();
            writeln!(f, "{}", hook_line("sid-rec", &fx.cwd, "Stop")).unwrap();
        }
        assert_eq!(
            ingest.ingest_once(base_ms + 10 * 60_000).await,
            1,
            "the Stop line raises exactly one Waiting card"
        );

        // Exactly ONE open card, and it is the Waiting card — the stale ASK is gone.
        let open = AttentionRepo::list_fleet(store.pool()).await.unwrap();
        assert_eq!(
            open.len(),
            1,
            "no duplicate open card — the stale ASK was closed"
        );
        assert_eq!(open[0].kind, AttentionKind::Waiting);

        // The original ASK row is answered (closed) with the reconcile marker.
        let closed = AttentionRepo::get(store.pool(), &ask_id).await.unwrap().unwrap();
        assert_eq!(
            closed.state, "answered",
            "the stale ASK is closed, not open"
        );
        assert_eq!(closed.answered_by.as_deref(), Some("resolved:session"));
    }
}
