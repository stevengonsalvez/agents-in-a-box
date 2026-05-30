// ABOUTME: Read-side fleet primitives — tmux capture, errors regex, state merge, JSONL tail.

pub mod errors;
pub mod jsonl_tail;
pub mod needs;
pub mod state;
pub mod tmux_pane;

pub use errors::{detect_error_signals, API_ERROR_PATTERNS};
pub use jsonl_tail::{
    cwd_to_project_slug, last_ask_user_question, last_assistant_info, last_narrative_snapshot,
    latest_transcript_for_cwd, wait_for_turn_end, AskUserQuestionData, LastAssistantInfo,
};
pub use needs::{
    classify, ClassifyInput, ErrContext, IdleContext, NeedsContext, NeedsRow, RouteHint,
    WaitContext,
};
pub use state::derive_state;
pub use tmux_pane::{capture_pane, detect_signals_from_pane};
