// ABOUTME: The open conversation as a frame carries it: a bounded, scrubbed
// window the reducer writes on its tick, so a renderer in another process can
// draw the thread without holding the chat host.
//
// The host itself (`ChatHost`, and the `ChatState` it drives) stays in
// `HostOnlyState`: it owns a poll loop, an inbox shared with workers and an
// unsent draft, none of which crosses a process boundary. What crosses is this
// projection, built the way #1131 built the attention merge: the reducer
// writes it, every host ticks it, and nothing is re-derived on a render path.
//
// Two rules hold the shape together:
//
//   BOUNDED   every list is capped and every text is cut, here, before serde
//             sees it, so `MAX_FRAME_BYTES` cannot be reached by a long
//             conversation or one enormous tool argument.
//   SCRUBBED  every field that can carry what a person or an agent wrote goes
//             through a scrubber from `wire::fields`, because a conversation
//             is exactly where a pasted credential ends up.

use ainb_hangar_proto::fleet::{FleetConfirmState, FleetMessageKind};
use ainb_plugin_hangar::screen::fleet_chat::{
    ChatActor, ChatConfirmCard, ChatMessageRow, ChatState, ChatStatus, ChatThreadRole, ChatTopic,
};

use crate::fleet::chat_host::ChatHost;

/// How many timeline rows a frame carries: the tail of the conversation, which
/// is the part a surface draws without scrolling. The host keeps more.
pub const MAX_ROWS: usize = 50;

/// How many characters of one row's body survive. Longer bodies are cut with an
/// ellipsis rather than dropped: a truncated reply still says what it was
/// about, and a surface that needs the whole of one asks the daemon for it.
pub const MAX_BODY_CHARS: usize = 512;

/// How many confirm cards a frame carries. The pane itself windows to four
/// (`CARDS_VISIBLE`), so this is already generous.
pub const MAX_CARDS: usize = 8;

/// How much of one card's arguments survive, as the compact JSON's length. A
/// tool call can carry a whole file; past this the card says so and carries the
/// size instead, because an operator deciding on a call needs the tool and the
/// shape, not the payload.
pub const MAX_ARGUMENT_BYTES: usize = 4 * 1024;

/// Which conversation the window is showing.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub enum ConversationTopic {
    /// Nothing is open.
    #[default]
    None,
    /// Pal, the fleet's own assistant.
    Pal,
    /// One session's own thread.
    Session,
}

/// Who wrote one row. The session key rides with the actor rather than in the
/// body, so a renderer attributes a row without parsing text.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub enum ConversationActor {
    /// A person at a surface.
    Operator,
    /// Pal writing through its own tools.
    Pal,
    /// An agent session replying in its own name, by session key.
    Session(String),
    /// A row whose sender was blank. Never rendered as a person: the daemon
    /// refuses a blank actor, and a row that cannot say who wrote it must not
    /// claim a human did.
    Unattributed,
}

/// What one row is: a prompt, a reply, or a lifecycle marker the daemon minted.
#[derive(serde::Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub enum ConversationKind {
    User,
    Agent,
    Marker,
}

/// One timeline row, attributed and cut to [`MAX_BODY_CHARS`].
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct ConversationRow {
    /// The daemon's own message identity, so a surface can key and thread rows.
    pub id: String,
    pub actor: ConversationActor,
    pub kind: ConversationKind,
    /// Whether this row answers another one.
    pub reply: bool,
    /// What was written, scrubbed. A conversation is where a pasted credential
    /// ends up, so this never reaches a frame verbatim.
    #[serde(serialize_with = "crate::wire::fields::scrub_str")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = String))]
    pub body: String,
    /// Whether the body was cut, so a surface can say so rather than implying
    /// the agent stopped mid-sentence.
    pub truncated: bool,
}

/// What a guardrail confirm card is waiting for.
#[derive(serde::Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub enum ConversationCardState {
    Open,
    Approved,
    Denied,
    Expired,
    /// The frame carried a card this build could not decode. Rendered, never
    /// answerable: an unknown state that offers an approve key is the failure
    /// the tolerant decode exists to avoid.
    Unrecognised,
}

/// One held tool call: what Pal wants to run, and what it would run it with.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct ConversationCard {
    pub confirm_id: String,
    /// The tool as the provider names it. An identifier, not prose.
    pub tool: String,
    /// The call's arguments, every string scrubbed and the structure kept: the
    /// shape is what an operator decides on. Replaced by `null` when the call
    /// is larger than [`MAX_ARGUMENT_BYTES`], with `arguments_bytes` saying how
    /// much was withheld.
    #[serde(serialize_with = "crate::wire::fields::scrub_json")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = specta_typescript::Unknown))]
    pub arguments: serde_json::Value,
    /// The compact size of the arguments as they were, whether or not they fit.
    pub arguments_bytes: u32,
    pub state: ConversationCardState,
    /// Why a card could not be decoded, when that is what happened.
    #[serde(serialize_with = "crate::wire::fields::scrub_str")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = String))]
    pub detail: String,
}

/// Where the conversation stands: still opening, live, or unavailable with the
/// daemon's own reason.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub enum ConversationStatus {
    /// Nothing has been opened.
    #[default]
    Closed,
    /// The open sequence is walking; `call` is the RPC it is on, as the daemon
    /// logs it, so a slow step is nameable rather than a spinner.
    Opening { call: String },
    /// The daemon answered and the timeline is live.
    Live,
    /// The daemon could not answer. The detail is its own words, scrubbed.
    Unavailable {
        #[serde(serialize_with = "crate::wire::fields::scrub_str")]
        #[cfg_attr(feature = "typescript-bindings", specta(type = String))]
        detail: String,
    },
}

/// The open conversation, as a frame carries it.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct Conversation {
    pub topic: ConversationTopic,
    /// The daemon's scope for this conversation (`session:<key>`,
    /// `channel:<id>`): an identifier a second surface reads the same thread by.
    pub scope_key: Option<String>,
    /// The session this conversation reaches, when it reaches one.
    pub target_session_key: Option<String>,
    pub status: ConversationStatus,
    /// The tail of the timeline, oldest first, at most [`MAX_ROWS`].
    pub rows: Vec<ConversationRow>,
    /// How many rows the host holds, so a surface can say the window is a tail
    /// rather than the whole conversation.
    pub rows_held: u32,
    /// The newest held tool calls, at most [`MAX_CARDS`].
    pub cards: Vec<ConversationCard>,
    /// How many cards the host holds, so a surface can say the window is the
    /// newest of them rather than all of them.
    pub cards_held: u32,
    /// Why a send would be refused right now, in the refusing surface's own
    /// words, or empty when it would not. A composer over a conversation that
    /// cannot send is the advertisement that makes a surface a lie.
    #[serde(serialize_with = "crate::wire::fields::scrub_str")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = String))]
    pub send_block: String,
    /// The operator's unsent draft, as its length. The text itself is theirs
    /// and has not been sent anywhere yet, so it never leaves the process that
    /// is typing it.
    #[serde(
        rename = "composer_len",
        serialize_with = "crate::wire::fields::char_count"
    )]
    #[cfg_attr(feature = "typescript-bindings", specta(type = u32))]
    pub composer: String,
}

/// Project `chat` into the bounded, scrubbed window a frame carries.
#[must_use]
pub fn project(chat: &ChatHost) -> Conversation {
    let state = chat.state();
    let rows = state.messages();
    let confirms = state.confirms();
    Conversation {
        topic: match chat.topic() {
            ChatTopic::Pal => ConversationTopic::Pal,
            ChatTopic::Session { .. } => ConversationTopic::Session,
            // A named broadcast channel never reaches this projection: the
            // broadcast composer is `fleet.broadcast`, which has its own field.
            ChatTopic::Channel { .. } => ConversationTopic::None,
        },
        scope_key: state.scope_key().map(ToString::to_string),
        target_session_key: state.target_session_key().map(ToString::to_string),
        status: status_of(state),
        // The TAIL: the newest rows are the ones a surface draws, and an
        // operator scrolling back is asking the daemon, not this window.
        rows: rows[rows.len().saturating_sub(MAX_ROWS)..].iter().map(row).collect(),
        rows_held: rows.len() as u32,
        // The newest cards, as with the rows: the one a surface is about to be
        // asked about is the last one the daemon held.
        cards: confirms[confirms.len().saturating_sub(MAX_CARDS)..].iter().map(card).collect(),
        cards_held: confirms.len() as u32,
        send_block: state.send_block().unwrap_or_default(),
        composer: state.composer().to_string(),
    }
}

fn status_of(state: &ChatState) -> ConversationStatus {
    match state.status() {
        ChatStatus::Opening(step) => ConversationStatus::Opening {
            call: step.call().to_string(),
        },
        ChatStatus::Live => ConversationStatus::Live,
        ChatStatus::Unavailable { detail, .. } => ConversationStatus::Unavailable {
            detail: detail.clone(),
        },
    }
}

fn row(row: &ChatMessageRow) -> ConversationRow {
    // Scrubbed BEFORE it is cut. A fixed-length credential straddling the cut
    // loses the tail its pattern needs, so a scrub after the cut would let the
    // rest of it ride the frame; the serializer scrubs again, harmlessly.
    let (body, truncated) = cut(
        &crate::fleet::bridge::redact::scrub(&row.body),
        MAX_BODY_CHARS,
    );
    ConversationRow {
        id: row.id.clone(),
        actor: match &row.actor {
            ChatActor::Operator => ConversationActor::Operator,
            ChatActor::Pal => ConversationActor::Pal,
            ChatActor::Session(key) => ConversationActor::Session(key.clone()),
            ChatActor::Unattributed => ConversationActor::Unattributed,
        },
        kind: match row.kind {
            FleetMessageKind::User => ConversationKind::User,
            FleetMessageKind::Agent => ConversationKind::Agent,
            FleetMessageKind::Marker => ConversationKind::Marker,
        },
        reply: row.role == ChatThreadRole::Reply,
        body,
        truncated,
    }
}

fn card(card: &ChatConfirmCard) -> ConversationCard {
    match card {
        ChatConfirmCard::Known(confirm) => {
            let size = compact_len(&confirm.arguments);
            let fits = size <= MAX_ARGUMENT_BYTES;
            ConversationCard {
                confirm_id: confirm.confirm_id.clone(),
                tool: confirm.tool.clone(),
                arguments: if fits {
                    confirm.arguments.clone()
                } else {
                    serde_json::Value::Null
                },
                arguments_bytes: size as u32,
                state: match confirm.state {
                    FleetConfirmState::Open => ConversationCardState::Open,
                    FleetConfirmState::Approved => ConversationCardState::Approved,
                    FleetConfirmState::Denied => ConversationCardState::Denied,
                    FleetConfirmState::Expired => ConversationCardState::Expired,
                },
                detail: String::new(),
            }
        }
        ChatConfirmCard::Unrecognised {
            confirm_id,
            tool,
            detail,
        } => ConversationCard {
            confirm_id: confirm_id.clone(),
            tool: tool.clone(),
            arguments: serde_json::Value::Null,
            arguments_bytes: 0,
            state: ConversationCardState::Unrecognised,
            detail: detail.clone(),
        },
    }
}

/// The length of `value` as compact JSON, counted without building the string:
/// a held tool call can carry a whole file, and measuring it must not copy it.
fn compact_len(value: &serde_json::Value) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    // Writing a `Value` to a sink that never fails cannot fail.
    let _ = serde_json::to_writer(&mut count, value);
    count.0
}

/// `text` cut to `limit` CHARACTERS, and whether anything was cut. Characters,
/// not bytes: a cut inside a multi-byte character is a panic, and a cut inside
/// an emoji is a mojibake a surface then paints.
fn cut(text: &str, limit: usize) -> (String, bool) {
    let mut kept: String = text.chars().take(limit).collect();
    if kept.chars().count() == text.chars().count() {
        return (kept, false);
    }
    kept.push('…');
    (kept, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_plugin_hangar::screen::fleet_chat::ChatSnapshot;

    /// A host holding `messages` rows and one confirm carrying `argument_bytes`
    /// of arguments, as the daemon's page lands them.
    fn chat_with(messages: usize, argument_bytes: usize) -> ChatHost {
        let mut chat = ChatHost::thread("claude:s-1".to_string());
        chat.state_mut().apply_snapshot(ChatSnapshot {
            scope_key: Some("session:claude:s-1".to_string()),
            messages: (0..messages)
                .map(|index| ainb_hangar_proto::fleet::FleetMessage {
                    id: format!("m-{index}"),
                    scope_key: "session:claude:s-1".to_string(),
                    origin_message_id: None,
                    sender: "copilot".to_string(),
                    kind: FleetMessageKind::Agent,
                    body: "b".repeat(MAX_BODY_CHARS + 20),
                    created_at: index as i64,
                })
                .collect(),
            confirms: vec![serde_json::json!({
                "confirm_id": "c-1",
                "scope_key": "session:claude:s-1",
                "tool": "shell",
                "arguments": { "command": "c".repeat(argument_bytes) },
                "state": "open",
                "created_at": 1,
                "expires_at": 2,
            })],
            ..ChatSnapshot::default()
        });
        chat
    }

    #[test]
    fn the_window_is_the_tail_of_the_conversation() {
        // The newest rows are the ones a surface draws; an operator scrolling
        // back asks the daemon, not this window.
        let projected = project(&chat_with(MAX_ROWS + 5, 16));

        assert_eq!(projected.rows.len(), MAX_ROWS);
        assert_eq!(projected.rows_held, MAX_ROWS as u32 + 5);
        assert_eq!(
            projected.rows.last().expect("a row").id,
            format!("m-{}", MAX_ROWS + 4),
            "ending at the newest row"
        );
        assert!(projected.rows[0].truncated, "and each body is cut");
    }

    #[test]
    fn a_tool_call_too_large_to_carry_says_so_instead() {
        // A tool call can carry a whole file. An operator deciding on one needs
        // the tool and the shape; the payload would put a megabyte on a frame.
        let projected = project(&chat_with(1, MAX_ARGUMENT_BYTES * 2));

        let card = &projected.cards[0];
        assert!(card.arguments.is_null(), "{card:?}");
        assert!(card.arguments_bytes > MAX_ARGUMENT_BYTES as u32);
        assert_eq!(card.state, ConversationCardState::Open, "still answerable");
    }

    #[test]
    fn a_full_window_is_far_inside_one_frame() {
        // The bound exists so a long conversation cannot reach MAX_FRAME_BYTES
        // (4 MiB). Worst case measured here, so a later change that widens a
        // row has to move this number deliberately.
        let mut projected = project(&chat_with(MAX_ROWS, MAX_ARGUMENT_BYTES - 64));
        projected.cards = std::iter::repeat_n(projected.cards[0].clone(), MAX_CARDS).collect();
        let bytes = serde_json::to_string(&projected).expect("serialises").len();

        println!("a full window is {bytes} bytes");
        assert!(
            bytes < crate::wire::frame::MAX_FRAME_BYTES / 8,
            "a full window is {bytes} bytes"
        );
    }

    #[test]
    fn a_long_body_is_cut_on_a_character_boundary() {
        let (kept, truncated) = cut(&"é".repeat(MAX_BODY_CHARS + 10), MAX_BODY_CHARS);
        assert!(truncated);
        assert_eq!(
            kept.chars().count(),
            MAX_BODY_CHARS + 1,
            "plus the ellipsis"
        );
    }

    #[test]
    fn a_credential_straddling_the_cut_is_scrubbed_whole() {
        // Assembled at runtime, so no credential-shaped literal is committed.
        // A 40-character npm token whose first half sits inside the cut would
        // lose the tail its pattern needs if the cut ran first.
        let token = format!("npm_{}", "a1B2".repeat(9));
        let body = format!("{} {token} and more", "x".repeat(MAX_BODY_CHARS - 20));
        let mut chat = ChatHost::thread("claude:s-1".to_string());
        chat.state_mut().apply_snapshot(ChatSnapshot {
            messages: vec![ainb_hangar_proto::fleet::FleetMessage {
                id: "m-1".to_string(),
                scope_key: "session:claude:s-1".to_string(),
                origin_message_id: None,
                sender: "copilot".to_string(),
                kind: FleetMessageKind::Agent,
                body,
                created_at: 1,
            }],
            ..ChatSnapshot::default()
        });

        let framed = project(&chat).rows[0].body.clone();

        assert!(
            !framed.contains("npm_a1B2"),
            "no part of the token survives: {framed}"
        );
    }

    #[test]
    fn the_newest_cards_are_the_window() {
        let mut chat = chat_with(1, 16);
        let confirms: Vec<serde_json::Value> = (0..MAX_CARDS + 3)
            .map(|index| {
                serde_json::json!({
                    "confirm_id": format!("c-{index}"),
                    "scope_key": "session:claude:s-1",
                    "tool": "shell",
                    "arguments": {},
                    "state": "open",
                    "created_at": 1,
                    "expires_at": 2,
                })
            })
            .collect();
        chat.state_mut().apply_snapshot(ChatSnapshot {
            confirms,
            ..ChatSnapshot::default()
        });

        let projected = project(&chat);

        assert_eq!(projected.cards.len(), MAX_CARDS);
        assert_eq!(projected.cards_held, MAX_CARDS as u32 + 3);
        assert_eq!(
            projected.cards.last().expect("a card").confirm_id,
            format!("c-{}", MAX_CARDS + 2)
        );
    }

    #[test]
    fn a_short_body_is_left_alone() {
        let (kept, truncated) = cut("nothing to cut", MAX_BODY_CHARS);
        assert_eq!(kept, "nothing to cut");
        assert!(!truncated);
    }
}
