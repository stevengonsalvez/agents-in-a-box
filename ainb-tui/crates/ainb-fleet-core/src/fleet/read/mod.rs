// ABOUTME: Read-side fleet primitives — tmux capture, error regex, JSONL tail,
// and the needs classifier.
//
// This is the fleet-core slice of the read layer. The event-sourced
// `current_state` index and the `state` merge helper stay in `ainb-core`
// (they are TUI/notifyd-facing and pull no daemon-shared code); everything they
// need from here is re-exported below and by `ainb-core::fleet::read`.

pub mod errors;
pub mod jsonl_tail;
pub mod needs;
pub mod tmux_pane;

pub use errors::{API_ERROR_PATTERNS, detect_error_signals};
/// Canonical turn-end stop_reason helper. `pub` (was `pub(crate)` pre-extraction)
/// because the bridge transport in `ainb-core` reaches it across the crate
/// boundary via the `ainb-core::fleet::read` re-export.
pub use jsonl_tail::is_turn_end_stop_reason;
pub use jsonl_tail::{
    AskUserQuestionData, LastAssistantInfo, cwd_to_project_slug, last_api_error_from_jsonl,
    last_ask_user_question, last_assistant_info, last_narrative_snapshot,
    latest_transcript_for_cwd, wait_for_turn_end,
};
pub use needs::{
    ClassifyInput, ErrContext, IdleContext, NeedsContext, NeedsRow, RouteHint, WaitContext,
    classify,
};
pub use tmux_pane::{capture_pane, detect_signals_from_pane};
