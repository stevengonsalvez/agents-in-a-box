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
    pub async fn record_issue_diff(
        pool: &SqlitePool,
        idgen: &dyn IdGen,
        clock: &dyn HangarClock,
        workspace_id: &str,
        actor: &ActivityActor,
        before: &Issue,
        after: &Issue,
    ) -> usize {
        let mut written = 0usize;
        for (action, details) in issue_diff(before, after) {
            if Self::record(
                pool,
                idgen,
                clock,
                workspace_id,
                &after.id,
                actor,
                action,
                details,
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
        let new = NewActivity {
            workspace_id,
            issue_id: Some(issue_id),
            actor,
            action,
            details,
            created_at: clock.now_ms(),
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
