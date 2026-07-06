//! Task-FSM services.
//!
//! Clock-aware operations that transition `agent_task_queue` rows through their
//! lifecycle.
//!
//! Unlike the [`crate::repo`] wrappers (stateless read + enqueue primitives),
//! services own the *transition* semantics the P1 daemon drives — claiming,
//! starting, completing, failing, and cancelling tasks — and take a
//! [`HangarClock`](ainb_hangar_core::clock::HangarClock) so their timestamps are
//! deterministic under test.
//!
//! P1.2 introduces [`claim`]; P1.3 adds the four finalize services
//! ([`start`], [`complete`], [`fail`], [`cancel`]) over the shared
//! [`finalize`] idempotent-transition primitive; P1.5 adds [`retry`], which
//! spawns a `parent_task_id`-chained child row when a failed task is eligible.

pub mod claim;

/// The shared idempotent-finalize primitive.
///
/// [`finalize::finalize_idempotent`] is what the four FSM finalize services
/// build on. Also re-exported at the crate root as
/// [`crate::idempotent_finalize`] so the P1.4 retry sweeper can reuse it.
pub mod finalize;

/// `{queued|dispatched|running} -> cancelled`.
pub mod cancel;
/// `running -> done` with the structured result payload.
pub mod complete;
/// `{running|queued} -> failed` with a typed [`fail::FailureReason`].
pub mod fail;
/// Spawn a `parent_task_id`-chained child row for a retryable failed task.
pub mod retry;
/// Route a squad assignment to its leader by enqueueing a leader-keyed task.
pub mod squad_assign;
/// `dispatched -> running`.
pub mod start;
