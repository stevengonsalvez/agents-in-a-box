//! The issue-activity DIFF engine (multica parity #13).
//!
//! hangar has TWO independent issue-update writers — the daemon's
//! `handle_issue_update` and the CLI's `run_issue_update` — and multica's
//! "one row per CHANGED FIELD" semantics must be identical on both. That
//! decision lives here, once, so the two cannot drift.
//!
//! # Best-effort, always
//!
//! Every method here swallows store faults after logging them: an audit write
//! must NEVER fail the mutation it describes (multica's listeners
//! `slog.Error`-and-return). [`ActivityService::record_issue_diff`] returns how
//! many rows it managed to write and keeps going after a per-row failure.
//!
//! # Field order
//!
//! A multi-field edit writes its rows in a STABLE order —
//! `state → assignee → priority → title → due_date` — so a timeline rendered
//! from a single update call reads deterministically.
//!
//! That order has to survive the READ, and the timeline sorts by
//! `(created_at, id)`. A whole diff runs well inside one millisecond, and
//! `ulid::Ulid::new()` is NOT monotonic within a millisecond (its low bits are
//! random, not a counter), so a shared `created_at` would let the id tiebreak
//! shuffle the fields arbitrarily. [`ActivityService::record_issue_diff`]
//! therefore reads the clock ONCE and stamps row `i` at `base + i` ms: the
//! written order is the read order, deterministically, at the cost of a few
//! milliseconds of skew on a multi-field edit.
//!
//! # multica DEVIATIONS (deliberate)
//!
//! * `priority_changed` details carry NUMBERS (`{"from":1,"to":3}`). hangar's
//!   priority is an `i64` `0..3`; multica's is a string enum. Rendering the
//!   number is the honest hangar shape.
//! * `due_date_changed` details carry epoch-millis numbers or a typed `null`,
//!   where multica writes `""` for an absent side.
//! * `task_completed` / `task_failed` details carry `{"task_id":"…"}`, additive
//!   over multica's `{}` — `details` is free-form, so this is append-only.
//! * There is no `description_updated`: hangar's `IssueFieldUpdate` has no
//!   description field, so no description-edit path exists to instrument.
//! * hangar has no request-auth context, so an owner-driven edit is attributed
//!   to the single bootstrapped default member (or `system` when none resolves).
//!   When per-request actor identity lands, the CALLERS' actor helpers change —
//!   nothing here does.

use ainb_hangar_core::activity::{ActivityAction, ActivityActor};
use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::IdGen;
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::repo::activity::{ActivityRepo, NewActivity};
use crate::repo::issue::Issue;

/// Stateless diff + record helpers over `activity_log`.
pub struct ActivityService;

impl ActivityService {
    /// Diff `before` vs `after` and record one activity row per CHANGED field.
    ///
    /// Returns how many rows were written (`0` for a no-op edit). Best-effort:
    /// a per-row store fault is logged and the remaining fields are still
    /// attempted.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_issue_diff(
        pool: &SqlitePool,
        idgen: &dyn IdGen,
        clock: &dyn HangarClock,
        workspace_id: &str,
        actor: &ActivityActor,
        before: &Issue,
        after: &Issue,
    ) -> usize {
        let base = clock.now_ms();
        let mut written = 0usize;
        for (i, (action, details)) in issue_diff(before, after).into_iter().enumerate() {
            // `base + i` ms: see the module docs — a shared timestamp would let
            // the non-monotonic ULID tiebreak shuffle the field order on read.
            let at = base.saturating_add(i64::try_from(i).unwrap_or(0));
            if Self::record_at(
                pool,
                idgen,
                workspace_id,
                &after.id,
                actor,
                action,
                details,
                at,
            )
            .await
            {
                written += 1;
            }
        }
        written
    }

    /// Record one activity row. Returns `false` (after logging) when the write
    /// failed — the caller carries on regardless.
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        pool: &SqlitePool,
        idgen: &dyn IdGen,
        clock: &dyn HangarClock,
        workspace_id: &str,
        issue_id: &str,
        actor: &ActivityActor,
        action: ActivityAction,
        details: Value,
    ) -> bool {
        Self::record_at(
            pool,
            idgen,
            workspace_id,
            issue_id,
            actor,
            action,
            details,
            clock.now_ms(),
        )
        .await
    }

    /// [`Self::record`] with an explicit `created_at`, so a diff can stamp its
    /// rows at increasing timestamps inside one clock read.
    #[allow(clippy::too_many_arguments)]
    async fn record_at(
        pool: &SqlitePool,
        idgen: &dyn IdGen,
        workspace_id: &str,
        issue_id: &str,
        actor: &ActivityActor,
        action: ActivityAction,
        details: Value,
        created_at: i64,
    ) -> bool {
        let new = NewActivity {
            workspace_id,
            issue_id: Some(issue_id),
            actor,
            action,
            details,
            created_at,
        };
        match ActivityRepo::record(pool, &idgen.new_ulid(), &new).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    issue_id,
                    action = action.as_db_str(),
                    error = %e,
                    "failed to record issue activity (mutation itself is unaffected)"
                );
                false
            }
        }
    }
}

/// The pure diff: the ordered `(action, details)` pairs for what changed between
/// two full issue rows. Exposed for tests and for any future caller that wants
/// the decision without the write.
#[must_use]
pub fn issue_diff(before: &Issue, after: &Issue) -> Vec<(ActivityAction, Value)> {
    let mut out = Vec::new();

    if before.state != after.state {
        out.push((
            ActivityAction::StatusChanged,
            json!({ "from": before.state, "to": after.state }),
        ));
    }

    if before.assignee != after.assignee {
        let mut d = serde_json::Map::new();
        if let Some(a) = &before.assignee {
            d.insert("from_type".into(), json!(a.kind().as_str()));
            d.insert("from_id".into(), json!(a.id()));
        }
        if let Some(a) = &after.assignee {
            d.insert("to_type".into(), json!(a.kind().as_str()));
            d.insert("to_id".into(), json!(a.id()));
        }
        out.push((ActivityAction::AssigneeChanged, Value::Object(d)));
    }

    if before.priority != after.priority {
        out.push((
            ActivityAction::PriorityChanged,
            json!({ "from": before.priority, "to": after.priority }),
        ));
    }

    if before.title != after.title {
        out.push((
            ActivityAction::TitleChanged,
            json!({ "from": before.title, "to": after.title }),
        ));
    }

    if before.due_date != after.due_date {
        out.push((
            ActivityAction::DueDateChanged,
            json!({ "from": before.due_date, "to": after.due_date }),
        ));
    }

    out
}
