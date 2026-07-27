//! The autopilot ACCOUNTABILITY LEDGER (`autopilot_rule_version`, migration
//! 0061, multica parity #14).
//!
//! One append-only row per SUBSTANTIVE publish of an autopilot rule, each naming
//! the accountable human and snapshotting the rule as published. Never updated,
//! never deleted, never trimmed.
//!
//! # The writer takes a live transaction, deliberately
//!
//! [`AutopilotRuleVersionRepo::publish_in_tx`] is the only writer and it takes
//! `&mut SqliteConnection` (a transaction handle), never a pool. A version row
//! can only be written as part of the mutation it records, so **no commit can
//! land a config change without its ledger entry** — and conversely a rejected
//! mutation (a malformed cron, say) leaves no orphan version row. That is what
//! makes multica's invariant real here: *creation itself is a transaction that
//! also writes rule-version v1*, so there is no window in which an autopilot
//! exists with no accountable human.
//!
//! # Version numbering and the race guard
//!
//! `version` is `COALESCE(MAX(version), 0) + 1` computed INSIDE the same
//! transaction. `idx_autopilot_rule_version_seq` (UNIQUE on
//! `(autopilot_id, version)`) is the guard: SQLite serialises writers, so a
//! loser sees the constraint and its whole mutation rolls back — correct, not
//! silently duplicated.
//!
//! Over an empty set that expression yields `1`, which is exactly how a pre-0061
//! autopilot (deliberately NOT backfilled, migration decision 5) mints v1 on its
//! first edit after upgrade.

use ainb_hangar_core::actor::ActorRef;
use ainb_hangar_core::autopilot::rule_version::RuleChangeKind;
use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
use sqlx::SqlitePool;

use super::autopilot::AutopilotRepoError;

/// One stored ledger row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleVersion {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning workspace (`workspace.id`).
    pub workspace_id: String,
    /// The autopilot this version describes. No FK: the fact outlives the rule.
    pub autopilot_id: String,
    /// 1-based, monotonic per autopilot.
    pub version: i64,
    /// Raw `change_kind` token. Decode with
    /// [`RuleChangeKind::from_db_str`] — an unknown token from a newer daemon
    /// is rendered raw rather than erroring.
    pub change_kind: String,
    /// The accountable actor ref (`member:<id>` / `agent:<id>`); `None` when the
    /// mutation carried no actor (an honest unknown, never a fabricated human).
    pub published_by: Option<String>,
    /// Serialised JSON object: the rule as published, plus `changed`.
    pub config_summary: String,
    /// Publish instant (epoch-ms).
    pub created_at: i64,
}

/// Stateless typed wrapper over `autopilot_rule_version`.
pub struct AutopilotRuleVersionRepo;

impl AutopilotRuleVersionRepo {
    /// Append one ledger row INSIDE the caller's transaction, returning the
    /// minted version number.
    ///
    /// The `version` is computed from `MAX(version) + 1` within the same
    /// transaction; `idx_autopilot_rule_version_seq` rejects a duplicate, which
    /// rolls the caller's whole mutation back.
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure — most notably the
    /// unique-sequence conflict under concurrent writers.
    pub async fn publish_in_tx(
        tx: &mut sqlx::SqliteConnection,
        clock: &dyn HangarClock,
        workspace: &WorkspaceId,
        autopilot_id: &AutopilotId,
        change_kind: RuleChangeKind,
        published_by: Option<&ActorRef>,
        config_summary: &serde_json::Value,
    ) -> Result<i64, AutopilotRepoError> {
        let next: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM autopilot_rule_version \
             WHERE autopilot_id = ?",
        )
        .bind(autopilot_id.as_str())
        .fetch_one(&mut *tx)
        .await?;

        let id = SystemIdGen.new_ulid();
        sqlx::query(
            "INSERT INTO autopilot_rule_version \
             (id, workspace_id, autopilot_id, version, change_kind, published_by, \
              config_summary, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(workspace.as_str())
        .bind(autopilot_id.as_str())
        .bind(next)
        .bind(change_kind.as_db_str())
        .bind(published_by.map(ToString::to_string))
        .bind(config_summary.to_string())
        .bind(clock.now_ms())
        .execute(&mut *tx)
        .await?;

        Ok(next)
    }

    /// List one rule's versions, newest-first, capped at `limit`.
    ///
    /// Workspace-scoped: a foreign autopilot id yields an empty set rather than
    /// leaking another tenant's provenance.
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn list(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        autopilot_id: &AutopilotId,
        limit: u32,
    ) -> Result<Vec<RuleVersion>, AutopilotRepoError> {
        let rows = sqlx::query_as::<_, RuleVersion>(
            "SELECT id, workspace_id, autopilot_id, version, change_kind, published_by, \
                    config_summary, created_at \
             FROM autopilot_rule_version \
             WHERE autopilot_id = ? AND workspace_id = ? \
             ORDER BY version DESC \
             LIMIT ?",
        )
        .bind(autopilot_id.as_str())
        .bind(workspace.as_str())
        .bind(i64::from(limit))
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }

    /// The DISPATCH-TIME read: the newest version for this rule (multica
    /// migration 187's index), i.e. who is accountable for an unattended run.
    ///
    /// `None` for an unversioned (pre-0061, never-edited) rule — an honest
    /// unknown.
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn latest(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        autopilot_id: &AutopilotId,
    ) -> Result<Option<RuleVersion>, AutopilotRepoError> {
        let row = sqlx::query_as::<_, RuleVersion>(
            "SELECT id, workspace_id, autopilot_id, version, change_kind, published_by, \
                    config_summary, created_at \
             FROM autopilot_rule_version \
             WHERE autopilot_id = ? AND workspace_id = ? \
             ORDER BY version DESC \
             LIMIT 1",
        )
        .bind(autopilot_id.as_str())
        .bind(workspace.as_str())
        .fetch_optional(pool)
        .await?;
        Ok(row)
    }

    /// The newest version of EVERY rule in a workspace, as
    /// `(autopilot_id, version, published_by)`.
    ///
    /// One query for the whole list screen — deliberately not N+1 per autopilot.
    ///
    /// # Errors
    ///
    /// Returns [`AutopilotRepoError::Db`] on a SQL failure.
    pub async fn latest_by_autopilot(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
    ) -> Result<Vec<(String, i64, Option<String>)>, AutopilotRepoError> {
        use sqlx::Row;
        let rows = sqlx::query(
            "SELECT v.autopilot_id, v.version, v.published_by \
             FROM autopilot_rule_version v \
             JOIN ( \
                 SELECT autopilot_id, MAX(version) AS version \
                 FROM autopilot_rule_version WHERE workspace_id = ? \
                 GROUP BY autopilot_id \
             ) m ON m.autopilot_id = v.autopilot_id AND m.version = v.version \
             WHERE v.workspace_id = ?",
        )
        .bind(workspace.as_str())
        .bind(workspace.as_str())
        .fetch_all(pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.get::<String, _>("autopilot_id"),
                    r.get::<i64, _>("version"),
                    r.get::<Option<String>, _>("published_by"),
                )
            })
            .collect())
    }

    /// The dispatch-time read INSIDE a transaction: the newest version's
    /// `published_by` for this rule.
    ///
    /// Used by the fire path so a run's accountable actor is resolved in the
    /// same transaction that inserts the run.
    ///
    /// # Errors
    ///
    /// Returns [`sqlx::Error`] on a SQL failure.
    pub async fn latest_publisher_in_tx(
        tx: &mut sqlx::SqliteConnection,
        autopilot_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<Option<String>> = sqlx::query_scalar(
            "SELECT published_by FROM autopilot_rule_version \
             WHERE autopilot_id = ? ORDER BY version DESC LIMIT 1",
        )
        .bind(autopilot_id)
        .fetch_optional(&mut *tx)
        .await?;
        Ok(row.flatten())
    }
}

impl sqlx::FromRow<'_, sqlx::sqlite::SqliteRow> for RuleVersion {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            workspace_id: row.try_get("workspace_id")?,
            autopilot_id: row.try_get("autopilot_id")?,
            version: row.try_get("version")?,
            change_kind: row.try_get("change_kind")?,
            published_by: row.try_get("published_by")?,
            config_summary: row.try_get("config_summary")?,
            created_at: row.try_get("created_at")?,
        })
    }
}
