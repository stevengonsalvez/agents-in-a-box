//! The task-lifecycle state machine (T8 — `statig` typed transitions).
//!
//! The `agent_task_queue` lifecycle
//! (`queued -> dispatched -> running -> done | failed | cancelled`, plus the
//! stale-dispatch reclaim `dispatched -> queued` and the queued-TTL
//! `queued -> failed`) is modelled here as a `statig` compile-time hierarchical
//! state machine. Each legal edge of
//! [`TaskState::legal_transitions`](ainb_hangar_core::task::state::TaskState::legal_transitions)
//! is realised by exactly one [`LifecycleEvent`]; every other `(state, event)`
//! pair is rejected (no transition).
//!
//! # What this types, and what it does NOT
//!
//! This types the **in-process transition ordering** the daemon's claim loop
//! walks ([`crate::run_loop::execute_claimed`] drives `dispatched -> running`
//! then a terminal `done` / `failed`): before issuing each store-service DB
//! write, the loop advances a [`LifecycleGuard`] through the corresponding
//! typed edge, so an out-of-order finalize (e.g. `complete` before `start`)
//! surfaces as a typed [`IllegalLifecycleTransition`] rather than a silent
//! mis-step.
//!
//! It does **not** replace the DB-level enforcers. The per-(issue, agent) SQL
//! claim guard (`idx_one_pending_task_per_issue_agent`, migration 0012) and the
//! idempotent conditional-`UPDATE` finalize in
//! [`ainb_hangar_store::service`] remain authoritative for atomicity and races
//! across daemons. The `statig` machine types the single daemon's own ordered
//! call sequence; the SQL layer arbitrates concurrency.
//!
//! The [`tests`] module sweeps the full `(state × event)` product and asserts
//! the machine realises **exactly** the canonical edge set from
//! `TaskState::legal_transitions()`, so the typed FSM can never drift from the
//! one source of truth in `ainb-hangar-core`.

use ainb_hangar_core::task::state::TaskState;
use statig::prelude::*;

/// The triggers of the task lifecycle FSM — one per transition verb the control
/// plane drives.
///
/// Each variant maps onto the corresponding store-service call
/// ([`ClaimTaskService`](ainb_hangar_store::service::claim) /
/// [`StartTaskService`](ainb_hangar_store::service::start) / … ) or sweeper pass.
/// The claim loop fires [`Start`](Self::Start), [`Complete`](Self::Complete),
/// and [`Fail`](Self::Fail); the stale-dispatch sweeper and cancel RPC fire
/// [`ReclaimDispatched`](Self::ReclaimDispatched) and [`Cancel`](Self::Cancel)
/// respectively — all six are modelled so the typed FSM is a faithful whole-of-
/// lifecycle model, not just the happy path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// A runtime claimed the task: `queued -> dispatched`.
    Claim,
    /// The runner confirmed the agent subprocess is live: `dispatched -> running`.
    Start,
    /// A stale dispatch was reclaimed by the sweeper: `dispatched -> queued`.
    ///
    /// Modelled for lifecycle completeness (the exhaustive transition-table test
    /// exercises it). The reclaim is driven at the SQL level by the store's
    /// stale-dispatch sweeper — never fired through this guard by the claim loop —
    /// so it is not constructed in a non-test build.
    #[cfg_attr(not(test), allow(dead_code))]
    ReclaimDispatched,
    /// The run finished successfully: `running -> done`.
    Complete,
    /// The run failed (runner error, or a queued-TTL sweep): `running -> failed`
    /// or `queued -> failed`.
    Fail,
    /// A human cancelled the run: `running -> cancelled`.
    Cancel,
}

/// Raised when a [`LifecycleEvent`] is not a legal edge from the current state.
///
/// `Copy`/`Send`/`Sync`/`'static` so it threads cleanly through `anyhow` in the
/// claim loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("illegal in-process task transition: {from:?} rejected event {event:?}")]
pub struct IllegalLifecycleTransition {
    /// The state the FSM was in when the event was fired.
    pub from: TaskState,
    /// The event that was rejected.
    pub event: LifecycleEvent,
}

/// Shared storage for the lifecycle state machine.
///
/// The lifecycle carries no state-local data — every state is a unit — so this
/// is a zero-sized marker; the `#[state_machine]` impl below defines the states
/// and their legal edges.
#[derive(Debug, Default, Clone, Copy)]
pub struct TaskLifecycle;

#[state_machine(
    initial = "State::queued()",
    state(derive(Debug, Clone, Copy, PartialEq, Eq))
)]
impl TaskLifecycle {
    /// `queued`: awaiting dispatch. Claimable, or failable by the queued-TTL
    /// sweep.
    #[state]
    fn queued(event: &LifecycleEvent) -> Outcome<State> {
        match event {
            LifecycleEvent::Claim => Transition(State::dispatched()),
            LifecycleEvent::Fail => Transition(State::failed()),
            _ => Handled,
        }
    }

    /// `dispatched`: claimed but not yet confirmed running. Starts, or is
    /// reclaimed back to `queued` by the stale-dispatch sweep.
    #[state]
    fn dispatched(event: &LifecycleEvent) -> Outcome<State> {
        match event {
            LifecycleEvent::Start => Transition(State::running()),
            LifecycleEvent::ReclaimDispatched => Transition(State::queued()),
            _ => Handled,
        }
    }

    /// `running`: actively executing. Completes, fails, or is cancelled.
    #[state]
    fn running(event: &LifecycleEvent) -> Outcome<State> {
        match event {
            LifecycleEvent::Complete => Transition(State::done()),
            LifecycleEvent::Fail => Transition(State::failed()),
            LifecycleEvent::Cancel => Transition(State::cancelled()),
            _ => Handled,
        }
    }

    /// `done`: terminal success — rejects every event (no outgoing edge).
    #[state]
    fn done() -> Outcome<State> {
        Handled
    }

    /// `failed`: terminal failure — rejects every event (no outgoing edge).
    #[state]
    fn failed() -> Outcome<State> {
        Handled
    }

    /// `cancelled`: terminal cancel — rejects every event (no outgoing edge).
    #[state]
    fn cancelled() -> Outcome<State> {
        Handled
    }
}

/// Map the generated `statig` [`State`] onto the canonical
/// [`TaskState`](ainb_hangar_core::task::state::TaskState).
fn state_of(state: &State) -> TaskState {
    // `statig` emits struct-style variants (`Queued {}`, …); match with `{ .. }`.
    match state {
        State::Queued { .. } => TaskState::Queued,
        State::Dispatched { .. } => TaskState::Dispatched,
        State::Running { .. } => TaskState::Running,
        State::Done { .. } => TaskState::Done,
        State::Failed { .. } => TaskState::Failed,
        State::Cancelled { .. } => TaskState::Cancelled,
    }
}

/// The canonical shortest event path from the `queued` initial state to `target`.
///
/// Used to seed a fresh machine at an arbitrary state so a single transition can
/// be evaluated in isolation (the `queued -> ... -> target` prefix is replayed,
/// then the event under test is fired).
fn seed_path(target: TaskState) -> &'static [LifecycleEvent] {
    use LifecycleEvent::{Claim, Complete, Fail, Start};
    match target {
        TaskState::Queued => &[],
        TaskState::Dispatched => &[Claim],
        TaskState::Running => &[Claim, Start],
        TaskState::Done => &[Claim, Start, Complete],
        TaskState::Failed => &[Claim, Start, Fail],
        TaskState::Cancelled => &[Claim, Start, LifecycleEvent::Cancel],
    }
}

/// Drive a fresh typed FSM: seed it to `from`, then optionally fire `event`,
/// returning the state it settles in.
///
/// The state machine — not a hand-maintained table — decides the outcome; this
/// is the single seam every transition query (production and test) funnels
/// through, so the `statig` definition is the only source of transition truth.
fn drive(from: TaskState, event: Option<LifecycleEvent>) -> TaskState {
    let mut sm = TaskLifecycle::default().uninitialized_state_machine().init();
    for ev in seed_path(from) {
        sm.handle(ev);
    }
    if let Some(ev) = event {
        sm.handle(&ev);
    }
    state_of(sm.state())
}

/// The state reached by firing `event` from `from`, or `None` when the event is
/// not a legal edge (the machine stayed put).
///
/// Every legal edge changes state (there are no self-loops), so an unchanged
/// state unambiguously means "rejected".
#[must_use]
pub fn next_state(from: TaskState, event: LifecycleEvent) -> Option<TaskState> {
    let to = drive(from, Some(event));
    (to != from).then_some(to)
}

/// A live in-process guard tracking one task's lifecycle position.
///
/// The claim loop holds a guard per claimed task and advances it in lockstep
/// with the store-service DB writes, so the daemon's own ordered call sequence
/// is typed by the `statig` FSM.
#[derive(Debug, Clone, Copy)]
pub struct LifecycleGuard {
    state: TaskState,
}

impl LifecycleGuard {
    /// A freshly-claimed task: the in-process FSM begins at `dispatched`.
    ///
    /// The `queued -> dispatched` claim already committed at the DB level
    /// (`ClaimTaskService`, arbitrated by the migration-0012 SQL index) before a
    /// task reaches [`crate::run_loop::execute_claimed`]; the guard picks up at
    /// `dispatched` and types every remaining edge through the FSM.
    #[must_use]
    pub const fn claimed() -> Self {
        Self {
            state: TaskState::Dispatched,
        }
    }

    /// The task's current lifecycle state.
    #[must_use]
    pub const fn state(&self) -> TaskState {
        self.state
    }

    /// Advance the guard by firing `event` through the typed FSM.
    ///
    /// Returns the new state on a legal edge; on an illegal edge the guard is
    /// left unchanged and an [`IllegalLifecycleTransition`] is returned.
    ///
    /// # Errors
    ///
    /// Returns [`IllegalLifecycleTransition`] when `event` is not a legal edge
    /// from the current state.
    pub fn fire(&mut self, event: LifecycleEvent) -> Result<TaskState, IllegalLifecycleTransition> {
        match next_state(self.state, event) {
            Some(to) => {
                self.state = to;
                Ok(to)
            }
            None => Err(IllegalLifecycleTransition {
                from: self.state,
                event,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        IllegalLifecycleTransition, LifecycleEvent, LifecycleGuard, TaskState, drive, next_state,
    };

    /// Every lifecycle state, for the exhaustive sweep.
    const ALL_STATES: [TaskState; 6] = [
        TaskState::Queued,
        TaskState::Dispatched,
        TaskState::Running,
        TaskState::Done,
        TaskState::Failed,
        TaskState::Cancelled,
    ];

    /// Every lifecycle event, for the exhaustive sweep.
    const ALL_EVENTS: [LifecycleEvent; 6] = [
        LifecycleEvent::Claim,
        LifecycleEvent::Start,
        LifecycleEvent::ReclaimDispatched,
        LifecycleEvent::Complete,
        LifecycleEvent::Fail,
        LifecycleEvent::Cancel,
    ];

    /// The seed prefix must land the machine in exactly the requested state —
    /// the precondition the exhaustive sweep relies on to evaluate a single edge
    /// in isolation.
    #[test]
    fn seed_path_reaches_each_state() {
        for from in ALL_STATES {
            assert_eq!(
                drive(from, None),
                from,
                "seed_path did not reach {from:?}",
            );
        }
    }

    /// THE exhaustive transition-table test: sweep the full `(state × event)`
    /// product and assert the `statig` FSM realises **exactly** the canonical
    /// edge set from `TaskState::legal_transitions()` — no missing edge, no
    /// extra/illegal transition. This pins the typed FSM to the single source of
    /// truth in `ainb-hangar-core` so the two can never drift.
    #[test]
    fn statig_fsm_matches_canonical_transition_table() {
        use std::collections::HashSet;

        // Edges the typed FSM actually realises, swept over every pair.
        let mut realized: HashSet<(TaskState, TaskState)> = HashSet::new();
        for from in ALL_STATES {
            for event in ALL_EVENTS {
                if let Some(to) = next_state(from, event) {
                    assert_ne!(to, from, "a realised transition must change state");
                    realized.insert((from, to));
                }
            }
        }

        // The canonical edge set, straight from the one source of truth.
        let canonical: HashSet<(TaskState, TaskState)> =
            TaskState::legal_transitions().iter().copied().collect();

        assert_eq!(
            realized, canonical,
            "statig FSM edge set diverged from TaskState::legal_transitions()",
        );
        assert_eq!(
            realized.len(),
            7,
            "expected exactly the 7 canonical lifecycle edges",
        );
    }

    /// Terminal states have no outgoing edge — they reject every event.
    #[test]
    fn terminal_states_reject_all_events() {
        for from in [TaskState::Done, TaskState::Failed, TaskState::Cancelled] {
            for event in ALL_EVENTS {
                assert_eq!(
                    next_state(from, event),
                    None,
                    "{from:?} must reject {event:?}",
                );
            }
        }
    }

    /// The claim loop's happy path, driven through the guard:
    /// `dispatched --Start--> running --Complete--> done`.
    #[test]
    fn guard_walks_happy_path() {
        let mut guard = LifecycleGuard::claimed();
        assert_eq!(guard.state(), TaskState::Dispatched);
        assert_eq!(guard.fire(LifecycleEvent::Start), Ok(TaskState::Running));
        assert_eq!(guard.fire(LifecycleEvent::Complete), Ok(TaskState::Done));
    }

    /// The claim loop's failure path, driven through the guard:
    /// `dispatched --Start--> running --Fail--> failed`.
    #[test]
    fn guard_walks_failure_path() {
        let mut guard = LifecycleGuard::claimed();
        assert_eq!(guard.fire(LifecycleEvent::Start), Ok(TaskState::Running));
        assert_eq!(guard.fire(LifecycleEvent::Fail), Ok(TaskState::Failed));
    }

    /// An out-of-order finalize (`complete` before `start`) is a typed error and
    /// leaves the guard untouched — the guarantee the `statig` migration adds
    /// over ad-hoc ordered calls.
    #[test]
    fn guard_rejects_out_of_order_finalize() {
        let mut guard = LifecycleGuard::claimed();
        assert_eq!(
            guard.fire(LifecycleEvent::Complete),
            Err(IllegalLifecycleTransition {
                from: TaskState::Dispatched,
                event: LifecycleEvent::Complete,
            }),
        );
        assert_eq!(guard.state(), TaskState::Dispatched);
    }
}
