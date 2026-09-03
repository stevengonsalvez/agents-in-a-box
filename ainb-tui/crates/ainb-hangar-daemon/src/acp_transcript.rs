//! One ACP session's transcript, live and durable, behind one door.
//!
//! This module exists for its PRIVACY, not for its size. `StoreWriter` is a
//! field of [`TranscriptSink`] and nothing outside this file can name it, so
//! `acp_pool` cannot write through the STORE WRITER without publishing live
//! first. A doc comment on the field would not do that: Rust privacy is
//! per-MODULE, so a sign on the door inside `acp_pool.rs` is a sign, and the
//! same omission had already walked past it twice.
//!
//! # The guarantee, in terms you can check in one grep
//!
//! `AcpClassifier::classify_value` renders exactly NINE `event_type` tokens and
//! drops everything else, so a row can only diverge live-from-durable if
//! something minted one of those nine. **Every minter of those nine is behind
//! this module**: the reducer's seven reach the ledger only through
//! [`TranscriptSink::chunk`], the lifecycle markers only through
//! [`TranscriptSink::lifecycle`], and the one caller that appends straight to
//! the ledger goes through [`append_and_publish`].
//!
//! Counting WRITE SITES is what made seven successive counts wrong: that set is
//! unbounded and grows with the code. Counting TOKEN MINTERS bounds it, because
//! the token set is nine strings in one `match`.
//!
//! One exception, and it is unfixable by design rather than an oversight:
//! `acp.transcript_truncated` is minted INSIDE `StoreWriter::enforce_buffer_cap`,
//! below the layer that knows a stream exists, so it reaches durable without
//! streaming. That is the same buffer-overflow window the live-before-durable
//! ordering already qualifies the equality with, from the other side.

use std::time::Duration;

use ainb_acp::reducer::TranscriptChunk;
use ainb_acp::store_writer::{HighWater, Lifecycle, StoreWriter};
use ainb_hangar_proto::events::MessageKind;
use ainb_hangar_proto::transcript::AcpClassifier;
use ainb_hangar_store::repo::fleet_provider_event::{
    FleetProviderEventError, FleetProviderEventRepo, FleetProviderEventRow, NewFleetProviderEvent,
};
use sqlx::SqlitePool;

use crate::events::EventSink;
use crate::runner::RunStream;

/// One session's transcript, live and durable, behind one door.
///
/// The [`StoreWriter`] is PRIVATE to this type on purpose. Every durable
/// transcript row must also be published live, or the durable re-read carries
/// lines the operator never saw stream — and that omission has now been written
/// twice, both times by someone adding a perfectly legitimate new flush (the
/// turn-end one, then the adapter-death drain). A doc comment did not stop the
/// second. Here the only ways in are [`Self::chunk`] and [`Self::lifecycle`],
/// both of which publish before they write, so a third flush cannot compile
/// without going through one of them.
///
/// Live is published BEFORE the durable commit, deliberately: the writer
/// buffers and commits on a cadence, so publishing after would hold the
/// operator's view back by up to a flush interval. See the module docs for the
/// one guarantee that costs.
pub(crate) struct TranscriptSink {
    writer: StoreWriter,
    /// Where this session's rows go live, for a TASK session only. `None` for
    /// every chat session: no task to name, and its own stream elsewhere.
    pub(crate) stream: Option<RunStream>,
    /// The SAME classifier the durable `board_card_timeline` read uses, so a
    /// line published live and its re-read twin are byte-identical.
    classifier: AcpClassifier,
    /// Tool calls published this turn, for the run banner's tally.
    pub(crate) tool_calls: u32,
}

/// Bind a session's live task stream, or `None` when the scope is not a task's.
///
/// The scope convention is the whole discriminator: `acp_task` mints
/// `task:<id>` and nothing else may (`fleet/acp_session_create` refuses the
/// prefix), so `strip_prefix` is an exact test rather than a heuristic.
///
/// Owned arguments so a caller holding a `&mut` actor can await it: the actor
/// is `Send` but not `Sync`, so a future borrowing it is not spawnable.
pub(crate) async fn bind_task_stream(
    pool: SqlitePool,
    events: EventSink,
    scope_key: String,
) -> Option<RunStream> {
    let task_id = scope_key.strip_prefix(crate::acp_task::TASK_SCOPE_PREFIX)?;
    let workspace_id = crate::acp_task::workspace_for_scope(&pool, &scope_key).await?;
    RunStream::bind(&events, &workspace_id, task_id)
}

/// Append one row STRAIGHT to the ledger and publish it live, in that order,
/// as one operation.
///
/// `converge_dirty_session` mints its `turn_interrupted` marker here because it
/// is a free function with no actor and no writer, so [`TranscriptSink`]'s
/// privacy cannot reach it: a renderable durable row that no wrap around
/// `StoreWriter` can catch. Convergence is the operator-stop and adapter-death
/// path, so without the publish the live pane ends without the interruption and
/// re-opening the ticket shows a line nobody saw arrive.
///
/// Both halves are ONE function, not two statements at the call site, for the
/// reason this defect has now appeared three times: two statements are
/// separable by a later edit, and every previous instance was written by
/// someone doing something legitimate next to one of them.
///
/// A fresh classifier is correct here and not a shortcut: a lifecycle marker
/// carries everything it renders, unlike a `tool_call_update` that needs the
/// call before it.
pub(crate) async fn append_and_publish(
    pool: &SqlitePool,
    events: &EventSink,
    scope_key: &str,
    row: &NewFleetProviderEvent,
) -> Result<FleetProviderEventRow, FleetProviderEventError> {
    let stored = FleetProviderEventRepo::append(pool, row).await?;
    if let Some(stream) =
        bind_task_stream(pool.clone(), events.clone(), scope_key.to_string()).await
    {
        for (kind, body) in AcpClassifier::default().classify_row(&row.event_type, &row.raw_payload)
        {
            stream.line(kind, body);
        }
    }
    Ok(stored)
}

impl TranscriptSink {
    pub(crate) fn new(writer: StoreWriter) -> Self {
        Self {
            writer,
            stream: None,
            classifier: AcpClassifier::default(),
            tool_calls: 0,
        }
    }

    /// Publish a chunk live, then commit it.
    pub(crate) async fn chunk(
        &mut self,
        chunk: &TranscriptChunk,
    ) -> Result<Option<HighWater>, FleetProviderEventError> {
        self.publish(chunk.kind.event_type(), &chunk.payload);
        self.writer.push(chunk).await
    }

    /// Publish a lifecycle marker live, then commit it.
    pub(crate) async fn lifecycle(
        &mut self,
        marker: Lifecycle,
        payload: serde_json::Value,
    ) -> Result<Option<HighWater>, FleetProviderEventError> {
        self.publish(marker.event_type(), &payload);
        self.writer.lifecycle(marker, payload).await
    }

    /// Classify one row and stream every line it yields.
    fn publish(&mut self, event_type: &str, payload: &serde_json::Value) {
        if self.stream.is_none() {
            return;
        }
        for (kind, body) in self.classifier.classify_value(event_type, payload) {
            if kind == MessageKind::ToolCall {
                self.tool_calls = self.tool_calls.saturating_add(1);
            }
            if let Some(stream) = &self.stream {
                stream.line(kind, body);
            }
        }
    }

    /// The run banner's tally + clock. No-op for a chat session.
    pub(crate) fn progress(&self, elapsed: Duration) {
        if let Some(stream) = &self.stream {
            stream.progress(self.tool_calls, elapsed);
        }
    }

    pub(crate) async fn tick(&mut self) -> Result<Option<HighWater>, FleetProviderEventError> {
        self.writer.tick().await
    }

    pub(crate) fn bytes_written(&self) -> u64 {
        self.writer.bytes_written()
    }

    pub(crate) fn set_acp_session_id(&mut self, id: Option<String>) {
        self.writer.set_acp_session_id(id);
    }
}
