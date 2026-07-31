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

/// The issue-ACTIVITY diff engine (multica parity #13): one `activity_log` row
/// per changed field, shared by the daemon and CLI issue-update writers so the
/// two cannot drift on what counts as a change.
pub mod activity;

pub mod claim;

/// The shared idempotent-finalize primitive.
///
/// [`finalize::finalize_idempotent`] is what the four FSM finalize services
/// build on. Also re-exported at the crate root as
/// [`crate::idempotent_finalize`] so the P1.4 retry sweeper can reuse it.
pub mod finalize;

/// `{queued|dispatched|running} -> cancelled`.
pub mod cancel;
/// Child-done → parent cascade: post a roll-up comment on the parent when a
/// sub-issue completes and closes its stage barrier (migration 0046).
pub mod child_done;
/// `running -> done` with the structured result payload.
pub mod complete;
/// `{running|queued} -> failed` with a typed [`fail::FailureReason`].
pub mod fail;
/// Comment `@mention` ROUTING (multica parity #2-rest): resolve every mention
/// in a comment body to an agent / member / squad-leader, gate it, act on it,
/// and report a per-target outcome code. One seam behind both the `comment_add`
/// write and the `comment_mention_preview` dry run.
pub mod mention;
/// Role-gated PULL of board work: a card in a role-gated column is the queue,
/// and exactly one eligible agent takes it. The seam that replaces squad
/// BROADCAST (one run per member) with one owner per card.
pub mod pull;
/// Spawn a `parent_task_id`-chained child row for a retryable failed task.
pub mod retry;
/// Route a squad assignment to its leader by enqueueing a leader-keyed task.
pub mod squad_assign;
/// `dispatched -> running`.
pub mod start;
