//! Reusable render widgets shared across the Core 5 screens.
//!
//! These are **pure render helpers**: each takes a [`WireBuffer`], an area
//! width, and the data to paint, and emits cells. They hold no state and do no
//! IO. Screens own their reducer state and delegate the visual chrome of common
//! controls (filter chip bars, the working-agent avatar stack) to the widgets
//! here so the same control renders identically wherever it appears
//! (`feedback_keybinding_hints_near_control`, Multica visual parity).
//!
//! - [`card_board`] — the Linear-style status board (63l.1): five status columns
//!   of sleek bordered, rounded cards with per-column scroll, a dashed
//!   empty-state, and a returned layout model the P0.2 mouse layer hit-tests.
//! - [`filter_chip`] — the `All / Members / Agents / Mine`-style chip bar
//!   rendered on the issue list (P4.3) and reused by the skill manager (P4.6).
//! - [`working_chip`] — the top-right avatar stack + count badge showing which
//!   agents are currently working (Multica `WorkspaceAgentWorkingChip`, UX §2
//!   journey 5).
//! - [`transcript`] — the 5-colour message taxonomy renderer for the task-detail
//!   transcript (P4.4, Multica UX §7 verbatim).
//! - [`sidebar`] — the task-detail right-hand progressive-disclosure metadata
//!   column (P4.4, Multica UX §12.8).

pub mod actor_row;
pub mod card_board;
pub mod danger_access_modal;
pub mod editor_pane;
pub mod file_tree;
pub mod filter_chip;
pub mod frosted_banner;
pub mod key_entry;
pub mod label_chip;
pub mod offline_empty_state;
pub mod presence_dot;
pub mod sidebar;
pub mod sparkline_dual;
pub mod transcript;
pub mod working_chip;
