// ABOUTME: Read-side fleet primitives — tmux capture, errors regex, state merge, JSONL tail.

pub mod errors;
pub mod jsonl_tail;
pub mod needs;
pub mod state;
pub mod tmux_pane;

pub use errors::{API_ERROR_PATTERNS, detect_error_signals};
/// Canonical turn-end stop_reason helper — crate-internal (the bridge transport
/// and the needs classifier both import it from here).
pub(crate) use jsonl_tail::is_turn_end_stop_reason;
pub use jsonl_tail::{
    AskUserQuestionData, LastAssistantInfo, cwd_to_project_slug, last_api_error_from_jsonl,
    last_ask_user_question, last_assistant_info, last_narrative_snapshot,
    latest_transcript_for_cwd, wait_for_turn_end,
};
pub use needs::{
    ClassifyInput, ErrContext, IdleContext, NeedsContext, NeedsRow, RouteHint, WaitContext,
    classify,
};
pub use state::derive_state;
pub use tmux_pane::{capture_pane, detect_signals_from_pane};
