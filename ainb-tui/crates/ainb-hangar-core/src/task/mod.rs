//! The task domain: the `agent_task_queue` lifecycle FSM and (in later P1
//! sub-beads) the typed task row and FSM services.

/// The task lifecycle finite-state machine ([`state::TaskState`]).
pub mod state;
