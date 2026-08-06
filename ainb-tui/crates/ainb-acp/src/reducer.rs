//! `session/update` stream -> normalized transcript chunks.
//!
//! Two jobs, both from the plan's R4:
//!
//! * **Normalize**: every `SessionUpdate` variant collapses to one of seven
//!   [`ChunkKind`]s, so the transcript read model never has to know the
//!   adapter's variant zoo. Agent text and user text are SEPARATE kinds: the
//!   plan's kind list says `message`, but one shared kind loses the speaker
//!   (the transcript row cannot say who spoke) and lets a user chunk coalesce
//!   into the agent's reply, which matters most on the `session/load` path
//!   where the adapter replays the whole conversation as both roles.
//! * **Coalesce** (graft 5): contiguous same-kind TEXT deltas are merged and
//!   flushed at [`COALESCE_FLUSH_BYTES`] or at a kind boundary. An adapter
//!   emits one notification per token; without this the transcript would carry
//!   one row per token.
//!
//! The reducer also accumulates the turn's FINAL MESSAGE (the agent text, and
//! only the agent text), which is the single row the chat timeline gets while
//! the full stream lands in the transcript.

use agent_client_protocol::schema::v1::{ContentBlock, SessionUpdate};

/// Flush threshold for coalesced text, in bytes.
///
/// A chunk may exceed this by at most one delta: the accumulator is flushed
/// AFTER the delta that crosses the line, never by splitting it (splitting a
/// delta could cut a UTF-8 sequence and would make the row boundary depend on
/// the adapter's tokenizer).
pub const COALESCE_FLUSH_BYTES: usize = 4 * 1024;

/// The seven normalized transcript kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    /// Agent message text. The ONLY kind the timeline's final message is
    /// built from (R4/I4).
    Message,
    /// User message text. A separate kind on purpose: it keeps the speaker on
    /// the row, keeps user text out of the final message, and makes a role
    /// switch a coalescing boundary.
    UserMessage,
    /// Agent reasoning text.
    Thought,
    /// A tool call or tool-call update.
    ToolCall,
    /// An execution plan. Plan update/removal are unstable protocol
    /// extensions the pinned schema does not compile in; when they land they
    /// classify here too.
    Plan,
    /// A permission ask. ACP models permissions as an agent-to-client REQUEST,
    /// not a `session/update`, so these are fed in by the pool via
    /// [`TranscriptReducer::permission_chunk`] rather than produced by
    /// [`TranscriptReducer::push`].
    Permission,
    /// Token/cost accounting.
    Usage,
}

impl ChunkKind {
    /// The `fleet_provider_event.event_type` token for this kind.
    pub const fn event_type(self) -> &'static str {
        match self {
            Self::Message => "acp.message",
            Self::UserMessage => "acp.user_message",
            Self::Thought => "acp.thought",
            Self::ToolCall => "acp.tool_call",
            Self::Plan => "acp.plan",
            Self::Permission => "acp.permission",
            Self::Usage => "acp.usage",
        }
    }
}

/// One normalized transcript row, before it reaches the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptChunk {
    /// Normalized kind.
    pub kind: ChunkKind,
    /// The adapter's session id this chunk belongs to (multiplex demux key).
    pub session_id: String,
    /// The adapter's own event identity when it supplies a per-event unique
    /// one. ACP v1 supplies none (`messageId` is per message and `toolCallId`
    /// repeats across updates), so this is `None` today and the store writer
    /// mints a ULID; both ids stay visible inside [`Self::payload`].
    pub event_id: Option<String>,
    /// Coalesced text, empty for the structural kinds.
    pub text: String,
    /// What lands in `fleet_provider_event.raw_payload`: the verbatim update
    /// envelope for structural kinds, and for coalesced text the merged text
    /// plus the delta count it was built from (the individual deltas are not
    /// recoverable by construction, that is what coalescing means).
    pub payload: serde_json::Value,
}

/// Accumulator over one session's `session/update` stream.
#[derive(Debug, Default)]
pub struct TranscriptReducer {
    session_id: String,
    pending: Option<Pending>,
    final_message: String,
    replaying: bool,
}

#[derive(Debug)]
struct Pending {
    kind: ChunkKind,
    text: String,
    deltas: usize,
}

impl TranscriptReducer {
    /// Start a reducer for one adapter session id.
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            pending: None,
            final_message: String::new(),
            replaying: false,
        }
    }

    /// Suppress transcript output while the adapter replays history.
    ///
    /// `session/load` replays the WHOLE prior conversation as `session/update`
    /// notifications, ahead of its own reply. Those chunks are already in the
    /// transcript from the turns that produced them, so classifying them
    /// normally would write every row a second time and rebuild
    /// [`Self::final_message`] out of an old turn's text: the chat timeline
    /// would show a stale reply as if it had just arrived. The plan's resume
    /// contract is "No client-side transcript replay for `session/load`
    /// resume", and this is the seam that keeps it: while the flag is set,
    /// every update classifies as [`Classified::Replayed`] and produces
    /// nothing at all, so the store writer is never handed a row to persist.
    ///
    /// The caller flushes any pending text BEFORE turning this on, and clears
    /// it once the `session/load` reply has landed.
    pub fn set_replaying(&mut self, replaying: bool) {
        self.replaying = replaying;
    }

    /// Feed one update. Returns the chunks it completed, in order.
    ///
    /// Returns 0, 1 or 2 chunks: a kind boundary flushes the pending text
    /// chunk BEFORE emitting the structural chunk that caused the boundary.
    /// Always 0 while [`Self::set_replaying`] is on.
    pub fn push(&mut self, update: &SessionUpdate) -> Vec<TranscriptChunk> {
        let mut out = Vec::new();
        let classified = if self.replaying {
            Classified::Replayed
        } else {
            classify(update)
        };
        match classified {
            Classified::Text { kind, text } => {
                if self.pending.as_ref().is_some_and(|p| p.kind != kind) {
                    out.extend(self.flush());
                }
                if kind == ChunkKind::Message {
                    self.final_message.push_str(&text);
                }
                let pending = self.pending.get_or_insert(Pending {
                    kind,
                    text: String::new(),
                    deltas: 0,
                });
                pending.text.push_str(&text);
                pending.deltas += 1;
                if pending.text.len() >= COALESCE_FLUSH_BYTES {
                    out.extend(self.flush());
                }
            }
            Classified::Structural { kind } => {
                out.extend(self.flush());
                out.push(TranscriptChunk {
                    kind,
                    session_id: self.session_id.clone(),
                    event_id: None,
                    text: String::new(),
                    payload: serde_json::to_value(update).unwrap_or(serde_json::Value::Null),
                });
            }
            Classified::NonText { kind } => {
                out.extend(self.flush());
                out.push(TranscriptChunk {
                    kind,
                    session_id: self.session_id.clone(),
                    event_id: None,
                    text: String::new(),
                    payload: serde_json::json!({
                        "kind": kind.event_type(),
                        "text": "",
                        "coalescedDeltas": 0,
                        "block": serde_json::to_value(update).unwrap_or(serde_json::Value::Null),
                    }),
                });
            }
            Classified::Replayed | Classified::Ignored => {}
        }
        out
    }

    /// Emit whatever text is still accumulating. Idempotent.
    ///
    /// The pool calls this at turn end and before any commit that must be
    /// durable (a turn marker), so no text is stranded in memory.
    pub fn flush(&mut self) -> Option<TranscriptChunk> {
        let pending = self.pending.take()?;
        Some(TranscriptChunk {
            kind: pending.kind,
            session_id: self.session_id.clone(),
            event_id: None,
            text: pending.text.clone(),
            payload: serde_json::json!({
                "kind": pending.kind.event_type(),
                "text": pending.text,
                "coalescedDeltas": pending.deltas,
            }),
        })
    }

    /// The turn's final agent message: exactly what the chat timeline gets
    /// (R4/I4), while every chunk above goes to the transcript.
    pub fn final_message(&self) -> &str {
        &self.final_message
    }

    /// Reset the final-message accumulator for the next turn. The pending text
    /// buffer is flushed by the caller first.
    pub fn begin_turn(&mut self) {
        self.final_message.clear();
    }

    /// Build the transcript row for a permission ask.
    ///
    /// Permissions arrive as an agent-to-client REQUEST, not an update, so the
    /// pool (which owns the responder) hands the envelope here rather than the
    /// reducer discovering it in the stream.
    pub fn permission_chunk(&self, payload: serde_json::Value) -> TranscriptChunk {
        TranscriptChunk {
            kind: ChunkKind::Permission,
            session_id: self.session_id.clone(),
            event_id: None,
            text: String::new(),
            payload,
        }
    }
}

enum Classified {
    Text {
        kind: ChunkKind,
        text: String,
    },
    Structural {
        kind: ChunkKind,
    },
    /// A message or thought chunk whose `ContentBlock` is not text: an image,
    /// an audio blob, an embedded resource.
    ///
    /// It gets the SAME payload shape a coalesced text chunk gets (empty
    /// `text`, zero deltas, the verbatim update under `block`), so every
    /// `acp.message` / `acp.thought` row decodes one way. Routing it through
    /// [`Classified::Structural`] instead would give the same `event_type` two
    /// incompatible payload shapes, and a reader that assumed the coalesced one
    /// would silently read `text` as absent rather than as empty.
    NonText {
        kind: ChunkKind,
    },
    /// An update replayed by `session/load`; see
    /// [`TranscriptReducer::set_replaying`].
    Replayed,
    Ignored,
}

fn classify(update: &SessionUpdate) -> Classified {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => block_text(&chunk.content).map_or(
            Classified::NonText {
                kind: ChunkKind::Message,
            },
            |text| Classified::Text {
                kind: ChunkKind::Message,
                text,
            },
        ),
        SessionUpdate::UserMessageChunk(chunk) => block_text(&chunk.content).map_or(
            Classified::NonText {
                kind: ChunkKind::UserMessage,
            },
            |text| Classified::Text {
                kind: ChunkKind::UserMessage,
                text,
            },
        ),
        SessionUpdate::AgentThoughtChunk(chunk) => block_text(&chunk.content).map_or(
            Classified::NonText {
                kind: ChunkKind::Thought,
            },
            |text| Classified::Text {
                kind: ChunkKind::Thought,
                text,
            },
        ),
        SessionUpdate::ToolCall(_) | SessionUpdate::ToolCallUpdate(_) => Classified::Structural {
            kind: ChunkKind::ToolCall,
        },
        SessionUpdate::Plan(_) => Classified::Structural {
            kind: ChunkKind::Plan,
        },
        SessionUpdate::UsageUpdate(_) => Classified::Structural {
            kind: ChunkKind::Usage,
        },
        // Mode, config, available-commands and session-info updates are
        // control-plane state, not transcript. The client watches
        // CurrentModeUpdate for the I13 mode assertion; none of them belongs
        // in a conversation record.
        _ => Classified::Ignored,
    }
}

fn block_text(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::SessionNotification;

    /// Deserialize a recorded ndjson `session/update` stream, exactly as the
    /// adapter emits it. Fixture, not a real adapter.
    fn recorded(ndjson: &str) -> Vec<SessionUpdate> {
        ndjson
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let notification: SessionNotification =
                    serde_json::from_str(line).expect("recorded session/update params");
                notification.update
            })
            .collect()
    }

    const RECORDED_TURN: &str = include_str!("../tests/fixtures/recorded_turn.ndjson");

    #[test]
    fn recorded_turn_normalizes_to_the_expected_kind_sequence() {
        let mut reducer = TranscriptReducer::new("sess-1");
        let mut chunks = Vec::new();
        for update in recorded(RECORDED_TURN) {
            chunks.extend(reducer.push(&update));
        }
        chunks.extend(reducer.flush());

        let kinds: Vec<ChunkKind> = chunks.iter().map(|chunk| chunk.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ChunkKind::Thought,
                ChunkKind::ToolCall,
                ChunkKind::ToolCall,
                ChunkKind::Plan,
                ChunkKind::Usage,
                ChunkKind::Message,
            ]
        );
    }

    #[test]
    fn contiguous_same_kind_text_is_merged_into_one_chunk() {
        let mut reducer = TranscriptReducer::new("sess-1");
        let mut chunks = Vec::new();
        for update in recorded(RECORDED_TURN) {
            chunks.extend(reducer.push(&update));
        }
        chunks.extend(reducer.flush());

        let message = chunks
            .iter()
            .find(|chunk| chunk.kind == ChunkKind::Message)
            .expect("one message chunk");
        assert_eq!(message.text, "Hello, world!");
        assert_eq!(message.payload["coalescedDeltas"], 3);
        assert_eq!(reducer.final_message(), "Hello, world!");
    }

    #[test]
    fn a_kind_boundary_flushes_before_the_structural_chunk() {
        let updates = recorded(RECORDED_TURN);
        let mut reducer = TranscriptReducer::new("sess-1");
        // First update is a thought delta, second is the tool call.
        assert!(reducer.push(&updates[0]).is_empty());
        let emitted = reducer.push(&updates[1]);
        assert_eq!(emitted.len(), 2);
        assert_eq!(emitted[0].kind, ChunkKind::Thought);
        assert_eq!(emitted[1].kind, ChunkKind::ToolCall);
    }

    #[test]
    fn text_flushes_at_the_byte_threshold_not_at_turn_end() {
        let delta = "x".repeat(1024);
        let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": delta},
        }))
        .expect("agent_message_chunk");

        let mut reducer = TranscriptReducer::new("sess-1");
        let mut emitted = Vec::new();
        for _ in 0..4 {
            emitted.extend(reducer.push(&update));
        }
        assert_eq!(emitted.len(), 1, "flushed once the 4 KiB threshold was hit");
        assert_eq!(emitted[0].text.len(), COALESCE_FLUSH_BYTES);
        assert!(reducer.flush().is_none(), "nothing left pending");
    }

    #[test]
    fn five_hundred_token_deltas_coalesce_into_a_handful_of_chunks() {
        let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "token "},
        }))
        .expect("agent_message_chunk");

        let mut reducer = TranscriptReducer::new("sess-1");
        let mut emitted = Vec::new();
        for _ in 0..500 {
            emitted.extend(reducer.push(&update));
        }
        emitted.extend(reducer.flush());
        assert!(
            emitted.len() <= 1 + (500 * 6) / COALESCE_FLUSH_BYTES,
            "500 deltas produced {} rows",
            emitted.len()
        );
        assert_eq!(reducer.final_message().len(), 500 * 6);
    }

    /// The `session/load` replay carries BOTH roles. User text must never
    /// reach the timeline's final message, and must never be merged into the
    /// agent's reply row (R4/I4).
    #[test]
    fn user_text_is_neither_final_message_nor_merged_into_the_agent_reply() {
        let user: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "user_message_chunk",
            "content": {"type": "text", "text": "SECRET=kumquat"},
        }))
        .expect("user_message_chunk");
        let agent: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "ok"},
        }))
        .expect("agent_message_chunk");

        let mut reducer = TranscriptReducer::new("sess-1");
        let mut chunks = reducer.push(&user);
        chunks.extend(reducer.push(&agent));
        chunks.extend(reducer.flush());

        assert_eq!(
            chunks.iter().map(|chunk| chunk.kind).collect::<Vec<_>>(),
            vec![ChunkKind::UserMessage, ChunkKind::Message],
            "the role switch is a coalescing boundary"
        );
        assert_eq!(chunks[0].text, "SECRET=kumquat");
        assert_eq!(chunks[0].kind.event_type(), "acp.user_message");
        assert_eq!(chunks[1].text, "ok");
        assert_eq!(
            reducer.final_message(),
            "ok",
            "only agent text reaches the timeline"
        );
    }

    #[test]
    fn control_plane_updates_never_reach_the_transcript() {
        let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "current_mode_update",
            "currentModeId": "default",
        }))
        .expect("current_mode_update");
        let mut reducer = TranscriptReducer::new("sess-1");
        assert!(reducer.push(&update).is_empty());
    }

    fn agent_text(text: &str) -> SessionUpdate {
        serde_json::from_value(serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": text},
        }))
        .expect("agent_message_chunk")
    }

    /// The `session/load` history replay must not re-persist: those rows are
    /// already in the transcript from the turns that wrote them, and rebuilding
    /// `final_message` from them would show an old reply as the current one.
    #[test]
    fn a_replayed_history_produces_no_chunks_and_no_final_message() {
        let mut reducer = TranscriptReducer::new("sess-1");
        reducer.set_replaying(true);
        for text in ["replayed ", "history"] {
            assert!(
                reducer.push(&agent_text(text)).is_empty(),
                "a replayed chunk must produce nothing at all"
            );
        }
        assert!(reducer.flush().is_none(), "nothing accumulated to flush");
        assert_eq!(
            reducer.final_message(),
            "",
            "replayed agent text must never reach the timeline"
        );
    }

    /// Replay suppression is a WINDOW, not a one-way switch: the live turn that
    /// follows a resume behaves exactly as it would have without the replay.
    #[test]
    fn live_chunks_after_a_replay_behave_normally() {
        let mut reducer = TranscriptReducer::new("sess-1");
        reducer.set_replaying(true);
        reducer.push(&agent_text("old reply"));
        reducer.set_replaying(false);

        assert!(reducer.push(&agent_text("fresh ")).is_empty());
        assert!(reducer.push(&agent_text("reply")).is_empty());
        let chunk = reducer.flush().expect("the live text flushes");
        assert_eq!(chunk.kind, ChunkKind::Message);
        assert_eq!(chunk.text, "fresh reply");
        assert_eq!(
            reducer.final_message(),
            "fresh reply",
            "the replayed text left nothing behind"
        );
    }

    /// A non-text `ContentBlock` still lands under `acp.message`, so it must
    /// carry the SAME payload shape a coalesced text chunk carries. One
    /// `event_type` with two incompatible payload shapes is a reader bug
    /// waiting to happen.
    #[test]
    fn a_non_text_agent_block_keeps_the_coalesced_envelope_shape() {
        let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "image", "data": "aGk=", "mimeType": "image/png"},
        }))
        .expect("agent_message_chunk with an image block");

        let mut reducer = TranscriptReducer::new("sess-1");
        let chunks = reducer.push(&update);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, ChunkKind::Message);
        assert_eq!(chunks[0].payload["kind"], "acp.message");
        assert_eq!(chunks[0].payload["text"], "");
        assert_eq!(chunks[0].payload["coalescedDeltas"], 0);
        assert_eq!(
            chunks[0].payload["block"]["content"]["type"], "image",
            "the verbatim block is preserved alongside the common shape"
        );
        assert_eq!(
            reducer.final_message(),
            "",
            "a non-text block contributes no timeline text"
        );
    }

    #[test]
    fn permission_chunks_carry_their_envelope_verbatim() {
        let reducer = TranscriptReducer::new("sess-1");
        let envelope = serde_json::json!({"options": [{"optionId": "allow"}]});
        let chunk = reducer.permission_chunk(envelope.clone());
        assert_eq!(chunk.kind, ChunkKind::Permission);
        assert_eq!(chunk.payload, envelope);
        assert_eq!(chunk.kind.event_type(), "acp.permission");
    }
}
