//! Transcript chunks -> `fleet_provider_event` rows, on a commit cadence.
//!
//! Graft 1: there is no transcript table. ACP rows ride the existing ledger
//! with `source = 'acp'`, `event_type = 'acp.<kind>'` and `ingest_order` as the
//! cursor, which buys the idempotent `event_id` insert and the `raw_blake3`
//! integrity check for free (both are computed by the Phase 1 repo function,
//! not here: repo functions are the ONE door to the store).
//!
//! ## Cadence, not per-chunk commits
//!
//! Flush on whichever comes first: a [`WriterConfig::flush_bytes`] batch or
//! [`WriterConfig::flush_interval`]. Chunk coalescing bounds the ROW count;
//! only a cadence bounds the COMMIT count, and every commit takes the single
//! `SQLite` write lock the whole control plane shares.
//!
//! The interval is CALLER-DRIVEN: this crate owns no task and no timer (that
//! is what makes it hostable by a standalone process), so a writer that is
//! never touched never commits on its own. The pump MUST call
//! [`StoreWriter::tick`] on the interval, or a slow turn (one thought chunk,
//! then a five-minute tool call) streams nothing until the next chunk lands
//! and I12's "chunks arrive DURING the turn" fails for that window.
//!
//! ## Nothing RETRYABLE is lost on a failed commit
//!
//! [`StoreWriter::flush`] puts the batch BACK in the buffer when the store
//! rejects it for a reason that can clear. The store's own comment
//! (`store.rs:84`) warns that a contended writer can exhaust its 10 s
//! `busy_timeout`, and this crate is the new heavy writer, so a failed flush
//! has to be retryable rather than a hole in the transcript.
//!
//! Two things bound that generosity, because "restore and retry forever" is how
//! a writer turns a store fault into a daemon-wide memory incident:
//!
//! * A batch that can NEVER commit
//!   ([`FleetProviderEventError::is_permanent`]) is dropped rather than
//!   restored. Restoring it pins it at the head of the buffer: every later
//!   flush retries the same doomed rows and nothing behind them ever commits.
//! * The buffer is capped at [`MAX_BUFFERED_BYTES`]. Past it the OLDEST rows
//!   go, so the live tail of the conversation survives.
//!
//! Neither path is silent. Both leave one [`TRANSCRIPT_TRUNCATED`] marker row
//! in the buffer, so the transcript ADMITS the hole instead of skipping it.
//!
//! ## No broker
//!
//! Every flush RETURNS its [`HighWater`] mark instead of emitting anything. The
//! daemon's pool owns `transcript_tx`, so this crate stays a library that a
//! standalone process could host unchanged.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::fleet_provider_event::{
    FleetProviderEventError, FleetProviderEventRepo, NewFleetProviderEvent,
};

use crate::reducer::TranscriptChunk;

/// The `fleet_provider_event.source` token every ACP row carries.
///
/// It is also the retention discriminator: `source = 'acp'` rows are the only
/// rows the operator export-then-delete may touch, because they carry no
/// `projection_revision` recovery contract.
pub const ACP_SOURCE: &str = "acp";

/// The `event_type` of the daemon-minted row that stands in for transcript
/// rows this writer had to throw away.
///
/// A reader that hits one knows the stream is not contiguous THERE, which is
/// the whole point: a silently short transcript reads as a complete one.
pub const TRANSCRIPT_TRUNCATED: &str = "acp.transcript_truncated";

/// Hard ceiling on buffered-but-uncommitted transcript payload, per session.
///
/// Without it a store that keeps refusing writes (a wedged disk, a lock nobody
/// releases, a permanent error restored forever) grows this buffer until the
/// daemon is OOM-killed, which loses the whole transcript of every session,
/// rather than the oldest slice of one. 1 MiB is ~256 full coalesced chunks:
/// far past any real flush hiccup, far short of a memory incident.
pub const MAX_BUFFERED_BYTES: usize = 1024 * 1024;

/// Daemon-minted turn lifecycle markers (B's markers).
///
/// These are not adapter output. The daemon writes them so a transcript reader
/// (and the boot/runtime convergence scan) can tell a finished turn from an
/// interrupted one without consulting live process state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// A prompt was accepted by the adapter and a turn is now open.
    TurnStarted,
    /// The turn ended normally.
    TurnCompleted,
    /// The turn ended with an adapter-reported failure.
    TurnFailed,
    /// The turn was cut short (daemon restart, adapter exit, deadline).
    TurnInterrupted,
    /// The session's context was rebuilt (`loaded` or `reprimed`).
    ContextRebuilt,
}

impl Lifecycle {
    /// The `event_type` token for this marker.
    pub const fn event_type(self) -> &'static str {
        match self {
            Self::TurnStarted => "acp.turn_started",
            Self::TurnCompleted => "acp.turn_completed",
            Self::TurnFailed => "acp.turn_failed",
            Self::TurnInterrupted => "acp.turn_interrupted",
            Self::ContextRebuilt => "acp.context_rebuilt",
        }
    }
}

/// Commit cadence knobs.
#[derive(Debug, Clone, Copy)]
pub struct WriterConfig {
    /// Flush once the buffered payload reaches this many bytes.
    pub flush_bytes: usize,
    /// Flush once this long has passed since the last commit, even if the
    /// batch is small (a slow turn must still stream).
    ///
    /// Evaluated on [`StoreWriter::push`] and on [`StoreWriter::tick`] only.
    /// The writer holds no timer; the caller drives the clock.
    pub flush_interval: Duration,
}

impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            flush_bytes: 4 * 1024,
            flush_interval: Duration::from_millis(250),
        }
    }
}

/// The cursor position a committed batch reached.
///
/// Phase 5's pump turns this into a `transcript_tx` wakeup; this crate never
/// touches the broker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighWater {
    /// The stable daemon-minted session key the rows belong to.
    pub session_key: String,
    /// The highest `fleet_provider_event.ingest_order` the batch committed.
    pub ingest_order: i64,
}

/// Buffers one session's chunks and commits them on the cadence.
pub struct StoreWriter {
    store: Store,
    config: WriterConfig,
    provider: String,
    session_key: String,
    acp_session_id: Option<String>,
    id_gen: Box<dyn IdGen>,
    buffered: Vec<NewFleetProviderEvent>,
    buffered_bytes: usize,
    last_flush: Instant,
    commits: u64,
    rows_written: u64,
    bytes_written: u64,
    rows_dropped: u64,
}

impl StoreWriter {
    /// Build a writer for one ACP session.
    ///
    /// `provider` is the adapter token (`claude-agent-acp`), `session_key` the
    /// daemon-minted STABLE key: it survives `acp_session_id` churn, which is
    /// exactly why the transcript keys on it (I5).
    pub fn new(
        store: Store,
        provider: impl Into<String>,
        session_key: impl Into<String>,
        id_gen: Box<dyn IdGen>,
        config: WriterConfig,
    ) -> Self {
        Self {
            store,
            config,
            provider: provider.into(),
            session_key: session_key.into(),
            acp_session_id: None,
            id_gen,
            buffered: Vec::new(),
            buffered_bytes: 0,
            last_flush: Instant::now(),
            commits: 0,
            rows_written: 0,
            bytes_written: 0,
            rows_dropped: 0,
        }
    }

    /// Record the adapter's CURRENT session id, stamped onto later rows.
    ///
    /// Mutable by design: a rebuild swaps the adapter id under the same
    /// `session_key`.
    pub fn set_acp_session_id(&mut self, acp_session_id: Option<String>) {
        self.acp_session_id = acp_session_id;
    }

    /// Buffer one chunk, committing if the cadence says so.
    pub async fn push(
        &mut self,
        chunk: &TranscriptChunk,
    ) -> Result<Option<HighWater>, FleetProviderEventError> {
        let payload = chunk.payload.to_string();
        self.buffer(chunk.kind.event_type(), chunk.event_id.clone(), payload);
        if self.flush_due() {
            self.flush().await
        } else {
            Ok(None)
        }
    }

    /// Write a daemon-minted lifecycle marker and commit IMMEDIATELY.
    ///
    /// Turn boundaries are the rows convergence reads, so they are never left
    /// sitting in a buffer waiting for a cadence tick.
    pub async fn lifecycle(
        &mut self,
        marker: Lifecycle,
        detail: serde_json::Value,
    ) -> Result<Option<HighWater>, FleetProviderEventError> {
        self.buffer(marker.event_type(), None, detail.to_string());
        self.flush().await
    }

    /// Commit if [`WriterConfig::flush_interval`] has elapsed since the last
    /// flush ATTEMPT (a failed one counts, so a store that is refusing writes
    /// is retried on the cadence rather than in a hot loop). The pump's timer
    /// leg: a buffered chunk becomes visible without waiting for a second
    /// chunk.
    pub async fn tick(&mut self) -> Result<Option<HighWater>, FleetProviderEventError> {
        if self.buffered.is_empty() || self.last_flush.elapsed() < self.config.flush_interval {
            return Ok(None);
        }
        self.flush().await
    }

    /// Commit whatever is buffered. Idempotent; `Ok(None)` when empty.
    ///
    /// On a RETRYABLE store error the batch is restored to the buffer (ahead
    /// of anything buffered since) and the error is returned, so the caller can
    /// retry the same rows instead of discovering a hole in the transcript. On
    /// a permanent one ([`FleetProviderEventError::is_permanent`]) the batch is
    /// dropped and replaced by a
    /// [`TRANSCRIPT_TRUNCATED`] marker: those rows cannot commit however
    /// often they are retried, and restoring them would wedge every row behind
    /// them for the life of the session.
    ///
    /// Either way the error is returned, so a caller that treats a flush
    /// failure as a session-level fault still sees it.
    pub async fn flush(&mut self) -> Result<Option<HighWater>, FleetProviderEventError> {
        self.last_flush = Instant::now();
        if self.buffered.is_empty() {
            return Ok(None);
        }
        let batch = std::mem::take(&mut self.buffered);
        let batch_bytes = std::mem::take(&mut self.buffered_bytes);
        let high_water = match FleetProviderEventRepo::append_batch(self.store.pool(), &batch).await
        {
            Ok(high_water) => high_water,
            Err(error) if error.is_permanent() => {
                tracing::error!(
                    session_key = %self.session_key,
                    rows = batch.len(),
                    error = %error,
                    "acp transcript batch can never commit; dropping it"
                );
                self.drop_rows(batch.len() as u64);
                return Err(error);
            }
            Err(error) => {
                self.buffered.splice(0..0, batch);
                self.buffered_bytes += batch_bytes;
                self.enforce_buffer_cap();
                return Err(error);
            }
        };
        self.commits += 1;
        self.rows_written += batch.len() as u64;
        self.bytes_written += batch.iter().map(|row| row.raw_payload.len() as u64).sum::<u64>();
        Ok(high_water.map(|ingest_order| HighWater {
            session_key: self.session_key.clone(),
            ingest_order,
        }))
    }

    /// Transactions issued so far. This is the write-amplification early
    /// warning the Observability section asks for; a test asserts it stays
    /// bounded across a 500-chunk turn.
    pub const fn commits(&self) -> u64 {
        self.commits
    }

    /// Transcript rows committed so far.
    pub const fn rows_written(&self) -> u64 {
        self.rows_written
    }

    /// Transcript payload bytes committed so far.
    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Transcript rows thrown away rather than committed. Non-zero means the
    /// transcript for this session has a hole in it (with a marker row at the
    /// hole), so it belongs on the same health surface as the counters above.
    pub const fn rows_dropped(&self) -> u64 {
        self.rows_dropped
    }

    /// Rows currently buffered but not yet committed.
    pub const fn buffered_rows(&self) -> usize {
        self.buffered.len()
    }

    /// Payload bytes currently buffered but not yet committed, bounded by
    /// [`MAX_BUFFERED_BYTES`].
    pub const fn buffered_bytes(&self) -> usize {
        self.buffered_bytes
    }

    fn flush_due(&self) -> bool {
        self.buffered_bytes >= self.config.flush_bytes
            || self.last_flush.elapsed() >= self.config.flush_interval
    }

    /// Drop the oldest buffered rows until the buffer is back under
    /// [`MAX_BUFFERED_BYTES`], leaving one marker row where they were.
    ///
    /// OLDEST, not newest: a reader resuming after the incident needs the live
    /// tail of the conversation, and the tail is also the part no other copy
    /// of exists. The newest row is never dropped, so a session always makes
    /// forward progress even if one payload is on its own over the cap.
    fn enforce_buffer_cap(&mut self) {
        if self.buffered_bytes <= MAX_BUFFERED_BYTES {
            return;
        }
        let mut drop_count = 0;
        let mut freed = 0;
        let mut dropped_content = 0;
        for row in self.buffered.iter().take(self.buffered.len().saturating_sub(1)) {
            if self.buffered_bytes - freed <= MAX_BUFFERED_BYTES {
                break;
            }
            freed += row.raw_payload.len();
            drop_count += 1;
            // A marker absorbed by a LATER truncation was already counted when
            // it was minted; counting it again would inflate the hole.
            if row.event_type != TRANSCRIPT_TRUNCATED {
                dropped_content += 1;
            }
        }
        if drop_count == 0 {
            return;
        }
        self.buffered.drain(..drop_count);
        self.buffered_bytes -= freed;
        tracing::error!(
            session_key = %self.session_key,
            dropped = dropped_content,
            buffered_bytes = self.buffered_bytes,
            "acp transcript buffer hit its cap; oldest rows dropped"
        );
        self.drop_rows(dropped_content);
    }

    /// Account for `dropped` lost rows and put ONE marker in their place.
    ///
    /// The marker is buffered at the FRONT, which is where the rows it stands
    /// in for used to be, so the committed transcript carries the hole in
    /// commit order rather than at the end of the session.
    fn drop_rows(&mut self, dropped: u64) {
        self.rows_dropped += dropped;
        let payload = serde_json::json!({
            "kind": TRANSCRIPT_TRUNCATED,
            "droppedRows": dropped,
            "droppedRowsTotal": self.rows_dropped,
        })
        .to_string();
        let now = epoch_millis();
        self.buffered_bytes += payload.len();
        self.buffered.insert(
            0,
            NewFleetProviderEvent {
                event_id: self.id_gen.new_ulid(),
                provider: self.provider.clone(),
                source: ACP_SOURCE.to_string(),
                session_key: Some(self.session_key.clone()),
                provider_session_id: self.acp_session_id.clone(),
                observed_at: now,
                received_at: now,
                event_type: TRANSCRIPT_TRUNCATED.to_string(),
                raw_payload: payload,
            },
        );
    }

    fn buffer(&mut self, event_type: &str, event_id: Option<String>, payload: String) {
        let now = epoch_millis();
        self.buffered_bytes += payload.len();
        self.buffered.push(NewFleetProviderEvent {
            // Adapter-supplied id when there is one, else a minted ULID. ACP
            // v1 carries no per-update unique id, so this is a mint in
            // practice; the seam exists for the adapter that grows one. A
            // reused adapter id whose envelope differs is an
            // `EventIdCollision` from the store, never a silently discarded
            // payload; a re-flushed batch keeps its original stamps and so
            // replays as an exact no-op.
            event_id: event_id.unwrap_or_else(|| self.id_gen.new_ulid()),
            provider: self.provider.clone(),
            source: ACP_SOURCE.to_string(),
            session_key: Some(self.session_key.clone()),
            provider_session_id: self.acp_session_id.clone(),
            observed_at: now,
            received_at: now,
            event_type: event_type.to_string(),
            raw_payload: payload,
        });
        // Only reachable when flushes are FAILING: the cap is two orders of
        // magnitude above `flush_bytes`, so a healthy writer commits long
        // before it gets here.
        self.enforce_buffer_cap();
    }
}

fn epoch_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
        i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    })
}
