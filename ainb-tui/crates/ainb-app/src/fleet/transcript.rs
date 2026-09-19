// ABOUTME: An ACP session's transcript, paged from the daemon and projected as
// a frame carries it: the host half lives in `HostOnlyState`, the bounded,
// scrubbed window rides the Fleet section.
//
// An ACP session has no tmux pane, so no terminal tab can show it. Its
// transcript is the daemon's `fleet_provider_event` rows (`acp.message`,
// `acp.thought`, `acp.tool_call`, `acp.plan`, `acp.permission`, ...), paged
// through `fleet/transcript_list` by an ingest-order cursor. Each chunk's kind
// comes straight from its event type, so the seven kinds are exact rather than
// guessed, and its text from the daemon's own `AcpClassifier`, so this window
// and the daemon's task timeline cannot describe one chunk two ways.
//
//   BOUNDED   the host keeps a tail, the frame carries a smaller tail, and
//             every body is cut, so no transcript can reach MAX_FRAME_BYTES.
//   SCRUBBED  bodies are scrubbed BEFORE the cut (a credential straddling the
//             cut would otherwise escape its pattern) and again on the frame.

use std::sync::{Arc, Mutex};

use ainb_hangar_proto::fleet::{FleetTranscriptListParams, FleetTranscriptListResult};
use ainb_hangar_proto::transcript::{AcpClassifier, acp_card_text};

use crate::fleet::bridge::redact::scrub;

// Bounds beside the conversation's (`fleet::conversation`); the spec takes
// ownership of both in #1179.

/// How many chunks the frame carries: the tail a surface draws.
pub const MAX_CHUNKS: usize = 80;

/// How many characters of one chunk's text survive, as with a conversation row.
pub const MAX_CHUNK_CHARS: usize = 512;

/// How many chunks the host keeps. More than the frame carries, so a surface
/// that later widens its window has the rows, and bounded, so a long run does
/// not grow the process without end.
const KEEP_CHUNKS: usize = 400;

/// How many chunks one page asks for. With one page in flight at a time and
/// one folded per tick, this is the most any tick can take in, however large
/// or hostile the transcript: it cannot pin the host.
const PAGE: u32 = 200;

/// How often an open transcript asks for the next page.
const POLL_MS: i64 = 1_000;

/// What one chunk is, from its `acp.<kind>` event type.
#[derive(serde::Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub enum ChunkKind {
    /// The agent's message text.
    Message,
    /// The user's message text.
    UserMessage,
    /// The agent's reasoning.
    Thought,
    /// A tool call or its update.
    ToolCall,
    /// An execution plan.
    Plan,
    /// A permission the agent asked for.
    Permission,
    /// Token and cost accounting.
    Usage,
    /// Anything else the daemon records about the run: a turn ending, a
    /// truncation notice, a kind this build does not know.
    Lifecycle,
}

impl ChunkKind {
    /// The kind an `acp.<kind>` event type names.
    #[must_use]
    pub fn of(event_type: &str) -> Self {
        match event_type {
            "acp.message" => Self::Message,
            "acp.user_message" => Self::UserMessage,
            "acp.thought" => Self::Thought,
            "acp.tool_call" => Self::ToolCall,
            "acp.plan" => Self::Plan,
            "acp.permission" => Self::Permission,
            "acp.usage" => Self::Usage,
            _ => Self::Lifecycle,
        }
    }
}

/// One chunk as the host holds it: classified, scrubbed, then cut, so what
/// the host keeps is bounded by `KEEP_CHUNKS` bodies of `MAX_CHUNK_CHARS`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeldChunk {
    order: i64,
    kind: ChunkKind,
    body: String,
    truncated: bool,
}

/// What a page worker reported.
#[derive(Debug)]
pub enum TranscriptOutcome {
    /// A page arrived.
    Page(FleetTranscriptListResult),
    /// The read failed, in the daemon client's words.
    Failed(String),
}

/// Where the transcript read stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReadStatus {
    Loading,
    Live,
    Unavailable(String),
}

/// One open ACP transcript: the cursor, the kept tail and the worker's inbox.
#[derive(Debug)]
pub struct TranscriptHost {
    session_key: String,
    chunks: Vec<HeldChunk>,
    /// The ingest-order cursor the next page reads after.
    after: Option<i64>,
    /// Whether the daemon said older rows were left behind.
    truncated: bool,
    classifier: AcpClassifier,
    status: ReadStatus,
    in_flight: bool,
    last_poll_ms: Option<i64>,
    inbox: Arc<Mutex<Vec<TranscriptOutcome>>>,
}

impl TranscriptHost {
    /// Open `session_key`'s transcript. Nothing is read until the first tick.
    #[must_use]
    pub fn new(session_key: String) -> Self {
        Self {
            session_key,
            chunks: Vec::new(),
            after: None,
            truncated: false,
            classifier: AcpClassifier::default(),
            status: ReadStatus::Loading,
            in_flight: false,
            last_poll_ms: None,
            inbox: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The session this transcript is for.
    #[must_use]
    pub fn session_key(&self) -> &str {
        &self.session_key
    }

    /// The channel a page worker reports through. A host test holds it to
    /// stand in for a worker, as `AskState::reports` is held.
    #[must_use]
    pub fn reports(&self) -> Arc<Mutex<Vec<TranscriptOutcome>>> {
        Arc::clone(&self.inbox)
    }

    /// Fold what the worker reported, and ask for the next page when one is
    /// due. Reports whether anything a frame carries moved.
    pub fn tick(&mut self, now_ms: i64) -> bool {
        // ONE outcome per tick, whatever the inbox holds: with one page in
        // flight there is only ever one, and the bound holds even if not.
        let landed = {
            let mut inbox = self.inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (!inbox.is_empty()).then(|| inbox.remove(0))
        };
        let moved = landed.is_some_and(|outcome| {
            self.in_flight = false;
            self.fold(outcome)
        });
        let due = self.last_poll_ms.is_none_or(|last| now_ms - last >= POLL_MS);
        if due && !self.in_flight {
            self.last_poll_ms = Some(now_ms);
            self.in_flight = true;
            spawn_page(
                self.session_key.clone(),
                self.after,
                Arc::clone(&self.inbox),
            );
        }
        moved
    }

    /// Apply one outcome, reporting whether it changed anything.
    fn fold(&mut self, outcome: TranscriptOutcome) -> bool {
        match outcome {
            TranscriptOutcome::Failed(reason) => {
                let next = ReadStatus::Unavailable(reason);
                let moved = self.status != next;
                self.status = next;
                moved
            }
            TranscriptOutcome::Page(page) => {
                let mut moved = self.status != ReadStatus::Live;
                self.status = ReadStatus::Live;
                if self.after.is_none() && page.truncated && !self.truncated {
                    self.truncated = true;
                    moved = true;
                }
                for chunk in page.chunks {
                    // A page can overlap the last one by its cursor; the
                    // ingest order is the identity.
                    if self.after.is_some_and(|after| chunk.ingest_order <= after) {
                        continue;
                    }
                    let classified = self
                        .classifier
                        .classify_value(&chunk.event_type, &chunk.payload)
                        .into_iter()
                        .map(|(_, body)| body)
                        .collect::<Vec<_>>()
                        .join("\n");
                    // The classifier is silent on the prompt echo and the
                    // usage report, which the card still labels, so it asks
                    // for their card text rather than drawing an empty row.
                    let text = if classified.is_empty() {
                        acp_card_text(&chunk.event_type, &chunk.payload).unwrap_or_default()
                    } else {
                        classified
                    };
                    self.after = Some(chunk.ingest_order);
                    // Scrubbed before the cut, so a credential straddling the
                    // bound is redacted whole rather than cut in half.
                    let (body, truncated) =
                        crate::fleet::conversation::cut(&scrub(&text), MAX_CHUNK_CHARS);
                    self.chunks.push(HeldChunk {
                        order: chunk.ingest_order,
                        kind: ChunkKind::of(&chunk.event_type),
                        body,
                        truncated,
                    });
                    moved = true;
                }
                if let Some(next) = page.next_after_order {
                    self.after = Some(self.after.map_or(next, |after| after.max(next)));
                }
                if self.chunks.len() > KEEP_CHUNKS {
                    let drop = self.chunks.len() - KEEP_CHUNKS;
                    self.chunks.drain(..drop);
                    self.truncated = true;
                }
                moved
            }
        }
    }
}

/// Read one page on a worker thread and report it into `inbox`.
fn spawn_page(session_key: String, after: Option<i64>, inbox: Arc<Mutex<Vec<TranscriptOutcome>>>) {
    let worker_inbox = Arc::clone(&inbox);
    let spawned =
        std::thread::Builder::new().name("ainb-transcript-page".into()).spawn(move || {
            let outcome = read_page(session_key, after);
            report(&worker_inbox, outcome);
        });
    // A page that never starts must still land, or `in_flight` never clears
    // and the card waits on a read nobody is doing.
    if let Err(error) = spawned {
        tracing::warn!(%error, "transcript page thread spawn failed");
        report(
            &inbox,
            TranscriptOutcome::Failed(format!("the transcript read could not start: {error}")),
        );
    }
}

/// Put `outcome` in `inbox`, through a poisoned lock as `tick` reads it: a
/// dropped report would leave the page in flight for good.
fn report(inbox: &Mutex<Vec<TranscriptOutcome>>, outcome: TranscriptOutcome) {
    inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(outcome);
}

fn read_page(session_key: String, after: Option<i64>) -> TranscriptOutcome {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return TranscriptOutcome::Failed("no runtime for the transcript read".to_string());
    };
    runtime.block_on(async move {
        let client = match crate::fleet::bridge::daemon::tui_client() {
            Ok(client) => client,
            Err(error) => {
                return TranscriptOutcome::Failed(format!(
                    "fleet/transcript_list unavailable: {error}"
                ));
            }
        };
        match client
            .transcript_list(FleetTranscriptListParams {
                session_key,
                after_order: after,
                limit: PAGE,
            })
            .await
        {
            Ok(page) => TranscriptOutcome::Page(page),
            Err(error) => TranscriptOutcome::Failed(format!("fleet/transcript_list: {error}")),
        }
    })
}

/// One chunk as a frame carries it.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct TranscriptChunk {
    /// The daemon's ingest order, which keys and orders the chunks.
    pub order: i64,
    pub kind: ChunkKind,
    /// What the chunk says, as the daemon's classifier renders it: scrubbed,
    /// then cut to [`MAX_CHUNK_CHARS`] as the host folds it, and scrubbed
    /// again on the frame.
    #[serde(serialize_with = "crate::wire::fields::scrub_str")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = String))]
    pub body: String,
    /// Whether the body was cut.
    pub truncated: bool,
}

/// Where the transcript read stands, as a frame carries it.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub enum TranscriptStatus {
    /// No transcript is open.
    #[default]
    Closed,
    /// Opened; the first page has not arrived.
    Loading,
    /// The daemon answered.
    Live,
    /// The daemon could not answer, in its client's words, scrubbed.
    Unavailable {
        #[serde(serialize_with = "crate::wire::fields::scrub_str")]
        #[cfg_attr(feature = "typescript-bindings", specta(type = String))]
        detail: String,
    },
}

/// The open ACP transcript, as a frame carries it.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct Transcript {
    /// The Fleet session it belongs to (`acp:<id>`), an identity the host
    /// resolved against its own status read, scrubbed all the same.
    #[serde(serialize_with = "crate::wire::fields::scrub_opt")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = Option<String>))]
    pub session_key: Option<String>,
    pub status: TranscriptStatus,
    /// The newest chunks, oldest first, at most [`MAX_CHUNKS`].
    pub chunks: Vec<TranscriptChunk>,
    /// How many chunks the host holds.
    pub chunks_held: u32,
    /// Whether older chunks exist that neither the host nor the frame holds,
    /// so a surface says the transcript starts part-way rather than implying
    /// the run began here.
    pub starts_part_way: bool,
}

/// Project `host` into the bounded, scrubbed window a frame carries.
#[must_use]
pub fn project(host: &TranscriptHost) -> Transcript {
    let held = &host.chunks;
    Transcript {
        session_key: Some(host.session_key.clone()),
        status: match &host.status {
            ReadStatus::Loading => TranscriptStatus::Loading,
            ReadStatus::Live => TranscriptStatus::Live,
            ReadStatus::Unavailable(detail) => TranscriptStatus::Unavailable {
                detail: detail.clone(),
            },
        },
        chunks: held[held.len().saturating_sub(MAX_CHUNKS)..]
            .iter()
            .map(|chunk| TranscriptChunk {
                order: chunk.order,
                kind: chunk.kind,
                body: chunk.body.clone(),
                truncated: chunk.truncated,
            })
            .collect(),
        chunks_held: u32::try_from(held.len()).unwrap_or(u32::MAX),
        starts_part_way: host.truncated || held.len() > MAX_CHUNKS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::fleet::FleetTranscriptChunk;

    fn chunk(order: i64, event_type: &str, payload: serde_json::Value) -> FleetTranscriptChunk {
        FleetTranscriptChunk {
            ingest_order: order,
            event_id: format!("e-{order}"),
            session_key: "acp:s-1".to_string(),
            event_type: event_type.to_string(),
            payload,
            observed_at: order,
        }
    }

    fn page(chunks: Vec<FleetTranscriptChunk>) -> TranscriptOutcome {
        let next = chunks.last().map(|chunk| chunk.ingest_order);
        TranscriptOutcome::Page(FleetTranscriptListResult {
            chunks,
            next_after_order: next,
            truncated: false,
        })
    }

    #[test]
    fn each_chunk_takes_its_kind_from_its_event_type() {
        let mut host = TranscriptHost::new("acp:s-1".to_string());
        let kinds = [
            "acp.message",
            "acp.user_message",
            "acp.thought",
            "acp.tool_call",
            "acp.plan",
            "acp.permission",
            "acp.usage",
            "acp.turn_completed",
        ];
        let chunks = kinds
            .iter()
            .enumerate()
            .map(|(order, kind)| {
                chunk(
                    i64::try_from(order).expect("small"),
                    kind,
                    serde_json::json!({}),
                )
            })
            .collect();
        assert!(host.fold(page(chunks)));

        let projected = project(&host);
        assert_eq!(
            projected.chunks.iter().map(|c| c.kind).collect::<Vec<_>>(),
            vec![
                ChunkKind::Message,
                ChunkKind::UserMessage,
                ChunkKind::Thought,
                ChunkKind::ToolCall,
                ChunkKind::Plan,
                ChunkKind::Permission,
                ChunkKind::Usage,
                ChunkKind::Lifecycle,
            ]
        );
        assert_eq!(projected.status, TranscriptStatus::Live);
    }

    /// #1200: the card labels the prompt echo and the usage report, so they
    /// arrive with text rather than as labelled empty rows. The classifier is
    /// silent on both; the host asks `acp_card_text` for them.
    #[test]
    fn the_prompt_echo_and_the_usage_report_carry_text() {
        let token = format!("npm_{}", "a1B2".repeat(9));
        let mut host = TranscriptHost::new("acp:s-1".to_string());
        host.fold(page(vec![
            chunk(
                1,
                "acp.user_message",
                serde_json::json!({ "kind": "acp.user_message", "text": format!("use {token}") }),
            ),
            chunk(
                2,
                "acp.usage",
                serde_json::json!({
                    "sessionUpdate": "usage_update",
                    "used": 1200,
                    "size": 200_000,
                    "cost": { "amount": 0.4, "currency": "USD" },
                }),
            ),
            chunk(
                3,
                "acp.turn_started",
                serde_json::json!({ "turnId": "m-1" }),
            ),
        ]));

        let projected = project(&host);
        let bodies: Vec<_> =
            projected.chunks.iter().map(|chunk| (chunk.kind, chunk.body.as_str())).collect();
        assert_eq!(
            bodies,
            vec![
                (ChunkKind::UserMessage, "use <redacted>"),
                (
                    ChunkKind::Usage,
                    "1200 of 200000 tokens in context · 0.40 USD"
                ),
                (ChunkKind::Lifecycle, ""),
            ],
            "the echo is scrubbed, the cost has two decimals, bookkeeping stays empty"
        );
    }

    #[test]
    fn a_page_that_overlaps_the_last_adds_nothing_twice() {
        let mut host = TranscriptHost::new("acp:s-1".to_string());
        host.fold(page(vec![chunk(1, "acp.message", serde_json::json!({}))]));
        host.fold(page(vec![
            chunk(1, "acp.message", serde_json::json!({})),
            chunk(2, "acp.thought", serde_json::json!({})),
        ]));
        assert_eq!(
            project(&host).chunks.iter().map(|c| c.order).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn the_window_is_the_tail_and_says_it_starts_part_way() {
        let mut host = TranscriptHost::new("acp:s-1".to_string());
        let many = (0..i64::try_from(MAX_CHUNKS).expect("small") + 10)
            .map(|order| chunk(order, "acp.message", serde_json::json!({})))
            .collect();
        host.fold(page(many));

        let projected = project(&host);
        assert_eq!(projected.chunks.len(), MAX_CHUNKS);
        assert!(projected.starts_part_way);
        assert_eq!(
            projected.chunks.last().map(|c| c.order),
            i64::try_from(MAX_CHUNKS).ok().map(|n| n + 9)
        );
    }

    #[test]
    fn a_credential_straddling_the_cut_is_scrubbed_whole() {
        // Assembled at runtime, so no credential-shaped literal is committed.
        let token = format!("npm_{}", "a1B2".repeat(9));
        let text = format!("{} {token} and more", "x".repeat(MAX_CHUNK_CHARS - 20));
        let mut host = TranscriptHost::new("acp:s-1".to_string());
        host.fold(page(vec![chunk(
            1,
            "acp.message",
            serde_json::json!({ "text": text }),
        )]));

        let body = project(&host).chunks[0].body.clone();

        assert!(
            !body.contains("npm_a1B2"),
            "no part of the token survives: {body}"
        );
    }

    #[test]
    fn a_failed_read_says_so_and_keeps_what_it_had() {
        let mut host = TranscriptHost::new("acp:s-1".to_string());
        host.fold(page(vec![chunk(1, "acp.message", serde_json::json!({}))]));
        assert!(host.fold(TranscriptOutcome::Failed("socket gone".to_string())));
        let projected = project(&host);
        assert_eq!(
            projected.chunks.len(),
            1,
            "a failed poll does not blank the run"
        );
        assert!(matches!(
            projected.status,
            TranscriptStatus::Unavailable { .. }
        ));
    }

    #[test]
    fn a_report_through_a_poisoned_inbox_still_lands() {
        let host = TranscriptHost::new("acp:s-1".to_string());
        let inbox = host.reports();
        let poisoner = Arc::clone(&inbox);
        let _ = std::thread::spawn(move || {
            let _held = poisoner.lock().expect("inbox");
            panic!("a worker died holding the inbox");
        })
        .join();
        assert!(inbox.is_poisoned());
        report(
            &inbox,
            TranscriptOutcome::Failed("could not start".to_string()),
        );
        assert_eq!(
            inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(),
            1,
            "the page is reported, so it is no longer in flight"
        );
    }

    #[test]
    fn the_host_holds_bounded_bodies() {
        let mut host = TranscriptHost::new("acp:s-1".to_string());
        host.fold(page(vec![chunk(
            1,
            "acp.message",
            serde_json::json!({ "text": "x".repeat(MAX_CHUNK_CHARS * 40) }),
        )]));
        assert!(host.chunks[0].body.chars().count() <= MAX_CHUNK_CHARS + 1);
        assert!(host.chunks[0].truncated);
    }
}
