//! One ACP session's transcript, live and durable, behind one door.
//!
//! This module exists for its PRIVACY, not for its size. `StoreWriter` is a
//! field of [`TranscriptSink`] and nothing outside this file can name it, so
//! `acp_pool` cannot write through the STORE WRITER without publishing live
//! first. A doc comment on the field would not do that: Rust privacy is
//! per-MODULE, so a sign on the door inside `acp_pool.rs` is a sign, and the
//! same omission had already walked past it twice.
//!
//! **That is the exact scope of the guarantee, and it is narrower than "every
//! durable transcript row".** `FleetProviderEventRepo::append` is public and a
//! caller can reach the ledger without touching a `StoreWriter` at all —
//! `converge_dirty_session` does, and no wrap here can stop it. Such a caller
//! must publish through [`publish_repo_row`], and that one IS a convention.

use std::time::Duration;

use ainb_acp::reducer::TranscriptChunk;
use ainb_acp::store_writer::{HighWater, Lifecycle, StoreWriter};
use ainb_hangar_proto::events::MessageKind;
use ainb_hangar_proto::transcript::AcpClassifier;
use ainb_hangar_store::repo::fleet_provider_event::FleetProviderEventError;

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

/// Publish ONE row that was written straight to the repo, past the sink.
///
/// `converge_dirty_session` mints its `turn_interrupted` marker with
/// `FleetProviderEventRepo::append` because it is a free function with no actor
/// and no writer, so the sink's privacy cannot reach it — a durable row that no
/// wrap can catch. It still has to stream, and it is the one that matters most:
/// convergence is the operator-stop and adapter-death path, so without this the
/// live pane ends without the interruption and re-opening the ticket shows a
/// line that was never streamed.
///
/// A fresh classifier is correct here and not a shortcut: a lifecycle marker
/// carries everything it renders, unlike a `tool_call_update` that needs the
/// call before it.
pub(crate) fn publish_repo_row(stream: &RunStream, event_type: &str, raw_payload: &str) {
    for (kind, body) in AcpClassifier::default().classify_row(event_type, raw_payload) {
        stream.line(kind, body);
    }
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
