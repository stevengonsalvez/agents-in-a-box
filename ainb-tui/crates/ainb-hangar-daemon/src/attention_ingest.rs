//! The attention ingest producer (spec P2, D10) — the daemon's own tail of the
//! durable hook event log (`events.jsonl`) into the `attention` table.
//!
//! The lifecycle hook (`ainb fleet atc hook`) appends one JSON line per managed
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
//! ## Idempotency
//!
//! The attention id is keyed on the hook line's ABSOLUTE byte offset in the
//! append-only `events.jsonl`: `att:<session>:<offset>`. The offset is a stable
//! per-occurrence identity — a given hook line lives at one offset for the life
//! of the file, so:
//!
//!   - A re-read of the SAME line (a crash between insert and the cursor write, a
//!     best-effort cursor-write failure, or a corrupt/missing cursor that resets
//!     the read to 0) hashes to the SAME id → the insert is skipped → no
//!     duplicate card. Re-reading from 0 raises each request exactly once, so the
//!     cursor is a pure efficiency optimisation, not a correctness dependency.
//!   - A genuinely NEW occurrence (a fresh hook line — the same question asked
//!     again after the first was answered, or a recurring error) is appended at a
//!     NEW offset → a NEW id → a fresh row, even when its request context is
//!     byte-for-byte identical to a prior one.
//!
//! Keying on the offset — rather than a blake3 of the request context — is
//! deliberate: the context can carry TIME-DERIVED fields (e.g. `idle_minutes`),
//! so a context hash changes when the same line is re-classified at a later
//! wall-clock, which would mint spurious duplicate rows on any delayed re-read.
//! The offset is invariant to when the line is read.
//!
//! ## Delivery semantics
//!
//! Best-effort: a missing file is a no-op and a corrupt line is skipped. A store
//! fault leaves the cursor before the failed line so a later pass replays it.
//! A failed ingest never downs the daemon.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use ainb_fleet_core::read::needs::{ClassifyInput, NeedsContext, classify};
use ainb_fleet_core::types::{Session, SessionSource};
use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::events::EventSink;

/// Maximum bytes read from the un-ingested suffix in one pass. Bounds peak memory
/// regardless of how far behind the cursor is; the remainder is picked up next
/// tick. Mirrors notifyd's ingest bound.
const MAX_INGEST_BYTES: u64 = 4 * 1024 * 1024;

/// How often the producer tails the event log.
const TICK: std::time::Duration = std::time::Duration::from_secs(3);

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
        // Track each line's absolute byte offset in the file (the stable
        // per-occurrence identity the attention id is keyed on). `split_inclusive`
        // keeps the delimiter, avoiding a synthetic extra byte after the final
        // newline.
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
        if line.cwd.is_empty() && line.session_id.is_empty() {
            return LineOutcome::Processed;
        }

        // Every hook line feeds the canonical Fleet reducer, including events
        // that do not raise an attention card. The byte offset is a replay-safe
        // occurrence id, shared with this consumer's durable cursor semantics.
        let payload =
            serde_json::from_str::<serde_json::Value>(raw).unwrap_or(serde_json::Value::Null);
        let semantic_event = if line.event_type == "PreToolUse" && line.matcher == "AskUserQuestion"
        {
            "AskUserQuestion"
        } else {
            line.event_type.as_str()
        };
        if !line.session_id.is_empty() {
            let observation = crate::fleet::HookObservation {
                event_id: format!("hook:{}:{offset}", line.session_id),
                provider: &line.agent,
                provider_session_id: &line.session_id,
                cwd: &line.cwd,
                event_type: semantic_event,
                payload: &payload,
                observed_at: if line.ts > 0 { line.ts } else { now_ms },
            };
            if let Err(error) =
                crate::fleet::apply_hook(&self.pool, &self.events, observation).await
            {
                tracing::warn!(error = %error, "fleet hook reduce failed");
                return LineOutcome::Retry;
            }
        }

        if !is_qualifying(semantic_event) {
            return LineOutcome::Processed;
        }

        // Classify off the async runtime: `classify` reads the JSONL transcript +
        // does blocking fs I/O, so it must not run inline on a tokio worker.
        let session = session_from(&line);
        let Some(row) = tokio::task::spawn_blocking(move || {
            classify(ClassifyInput::from_env(session, None, now_ms))
        })
        .await
        .ok()
        .flatten() else {
            return LineOutcome::Processed;
        };

        let kind = kind_of(&row.context);

        // Stale-ASK reconcile (spec P2 open/close ordering). A session that is no
        // longer asking (the classifier returned anything but Ask) may still carry
        // an OPEN ASK row from an earlier Notification: the question was answered /
        // timed out / interrupted IN the live session, which never routes through
        // the hangar answer router, so nothing ever closed that row. Close it
        // BEFORE raising this follow-on card, or the stale ASK sits open + answerable
        // beside the new Waiting/Idle/Err card, duplicating the session's attention
        // signal (and a hangar surface could try to answer an already-answered ask).
        if kind != AttentionKind::AskUserQuestion {
            self.reconcile_stale_asks(&line.session_id, now_ms).await;
        }

        // Offset-keyed id: the SAME hook line (a replay / re-read) → same offset →
        // same id → the insert is skipped (idempotent). A NEW hook line → a new
        // offset → a fresh row, even if its request context is identical. Keying
        // on the offset (not a hash of the possibly time-derived context) keeps a
        // delayed re-read from minting spurious duplicates.
        let id = format!("att:{}:{}", line.session_id, offset);

        match AttentionRepo::get(&self.pool, &id).await {
            Ok(Some(_)) => return LineOutcome::Processed, // already raised
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, "attention ingest: existence check failed");
                return LineOutcome::Retry;
            }
        }

        // Resolve the routing channels ONCE, here at raise time (tcp T5). Hook
        // sessions are host-wide (workspace None), so this reads the GLOBAL rule
        // for the kind. Stamped onto the row + the event so every consumer filters
        // on the same decision.
        let channels = crate::notify::resolve_channels(&self.pool, kind, None).await;

        let payload = serde_json::to_string(&row.context).unwrap_or_else(|_| "{}".to_string());
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
        if let Err(e) = AttentionRepo::insert(&self.pool, &new).await {
            tracing::warn!(error = %e, "attention ingest: insert failed");
            return LineOutcome::Retry;
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

    /// Spawn the tail loop: tick every [`TICK`], ingesting each pass. Mirrors the
    /// inbox aggregator — the returned handle is dropped by `boot()` (process exit
    /// tears the task down); a future supervisor can keep it to stop cleanly.
    #[must_use]
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        use ainb_hangar_core::clock::{HangarClock as _, SystemClock};
        tokio::spawn(async move {
            let clock = SystemClock;
            let mut ticker = tokio::time::interval(TICK);
            loop {
                ticker.tick().await;
                let _ = self.ingest_once(clock.now_ms()).await;
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
