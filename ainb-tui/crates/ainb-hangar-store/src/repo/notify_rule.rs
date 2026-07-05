//! Typed repository over the `notify_rule` table (migration 0037) — the
//! per-attention-kind notification routing rules (tcp T5).
//!
//! A rule maps an [`AttentionKind`] to the set of PUSH [`Channel`]s an attention
//! of that kind should fan out to, on top of always appearing on the board. Rules
//! are two-tier:
//!
//! - a GLOBAL row (`workspace_id IS NULL`) per kind — the seeded default every
//!   workspace inherits;
//! - an optional PER-WORKSPACE override row per (workspace, kind) — takes
//!   precedence over the global row for that workspace.
//!
//! [`NotifyRuleRepo::resolve`] is the single authoritative precedence rule the
//! daemon calls at attention-raise time (compute-once-at-emit): workspace
//! override → global → a coded fallback. Because the daemon stamps the resolved
//! set onto the attention row and its `AttentionRaised` event, EVERY consumer
//! filters on the SAME decision — a rule edit in flight can never split-brain the
//! fan-out.

use ainb_hangar_core::channel::ChannelSet;
use sqlx::{Row, SqlitePool};

use super::attention::AttentionKind;

/// The coded fallback used ONLY when a kind has neither a workspace override nor
/// a global row (unreachable after migration 0037 seeds all six globals, but a
/// defensive floor so a manually-truncated table or a kind the resolver has not
/// seen still degrades to a sane, non-silent default rather than panicking).
///
/// `escalation` is the loudest fallback (a human is being paged); every other
/// kind falls back to the board only (empty set) — a missing rule must never
/// spuriously spam a channel, but an escalation must never be silently dropped.
#[must_use]
fn coded_fallback(kind: AttentionKind) -> ChannelSet {
    use ainb_hangar_core::channel::Channel::{Os, Phone, Web};
    match kind {
        AttentionKind::Escalation => ChannelSet::from_channels([Phone, Web, Os]),
        _ => ChannelSet::NONE,
    }
}

/// One resolved rule row for the settings grid: a kind, its effective channel
/// set, and whether the effective value is a per-workspace OVERRIDE (vs the
/// inherited global default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyRuleRow {
    /// The attention kind this rule governs.
    pub kind: AttentionKind,
    /// The effective channel set for the resolved scope.
    pub channels: ChannelSet,
    /// `true` when a per-workspace override supplied `channels`; `false` when the
    /// value is inherited from the global default. Always `false` for the global
    /// scope itself.
    pub overridden: bool,
}

/// Stateless typed wrapper over the `notify_rule` table.
pub struct NotifyRuleRepo;

impl NotifyRuleRepo {
    /// Resolve the channel set for `kind` in `workspace_id`'s scope.
    ///
    /// Precedence, most-specific first:
    /// 1. the `(workspace_id, kind)` override row, when `workspace_id` is `Some`
    ///    and such a row exists;
    /// 2. the `(NULL, kind)` global row;
    /// 3. [`coded_fallback`] when neither exists.
    ///
    /// `workspace_id = None` (a host session belonging to no workspace) skips step
    /// 1 and resolves straight against the global row — exactly what the fleet-wide
    /// attention rows want.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a query fails.
    pub async fn resolve(
        pool: &SqlitePool,
        kind: AttentionKind,
        workspace_id: Option<&str>,
    ) -> Result<ChannelSet, sqlx::Error> {
        if let Some(ws) = workspace_id {
            if let Some(channels) = fetch_channels(pool, Some(ws), kind).await? {
                return Ok(channels);
            }
        }
        if let Some(channels) = fetch_channels(pool, None, kind).await? {
            return Ok(channels);
        }
        Ok(coded_fallback(kind))
    }

    /// The full rule grid for a scope: one [`NotifyRuleRow`] per attention kind,
    /// in [`AttentionKind`] declaration order (the settings-grid row order).
    ///
    /// - `workspace_id = Some(ws)` → each kind's EFFECTIVE value for that
    ///   workspace, with `overridden` set when a workspace row supplied it.
    /// - `workspace_id = None` → the global rows, all `overridden = false`.
    ///
    /// Every kind is always present (falling back through the same precedence as
    /// [`Self::resolve`]), so the grid never shows a gap.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a query fails.
    pub async fn list(
        pool: &SqlitePool,
        workspace_id: Option<&str>,
    ) -> Result<Vec<NotifyRuleRow>, sqlx::Error> {
        let mut rows = Vec::with_capacity(ALL_KINDS.len());
        for &kind in ALL_KINDS {
            let (channels, overridden) = match workspace_id {
                Some(ws) => match fetch_channels(pool, Some(ws), kind).await? {
                    Some(c) => (c, true),
                    None => (Self::global_or_fallback(pool, kind).await?, false),
                },
                None => (Self::global_or_fallback(pool, kind).await?, false),
            };
            rows.push(NotifyRuleRow {
                kind,
                channels,
                overridden,
            });
        }
        Ok(rows)
    }

    /// Upsert the rule for a scope + kind (last write wins). `workspace_id = None`
    /// writes the GLOBAL row; `Some(ws)` writes a per-workspace OVERRIDE. The
    /// upsert keys on the matching partial unique index (global vs workspace), so
    /// re-setting a kind replaces its channels rather than inserting a duplicate.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails — e.g. a `kind` CHECK violation
    /// (impossible through [`AttentionKind`]) or a dangling `workspace_id` FK.
    pub async fn set(
        pool: &SqlitePool,
        workspace_id: Option<&str>,
        kind: AttentionKind,
        channels: ChannelSet,
    ) -> Result<(), sqlx::Error> {
        // The two partial unique indexes need two distinct conflict targets: a
        // global upsert conflicts on `(kind) WHERE workspace_id IS NULL`, a
        // workspace upsert on `(workspace_id, kind) WHERE workspace_id IS NOT
        // NULL`. SQLite resolves the right partial index from the conflict target
        // + its predicate, so each branch names its own.
        match workspace_id {
            None => {
                sqlx::query(
                    "INSERT INTO notify_rule (workspace_id, kind, channels) \
                     VALUES (NULL, ?, ?) \
                     ON CONFLICT (kind) WHERE workspace_id IS NULL \
                     DO UPDATE SET channels = excluded.channels",
                )
                .bind(kind.as_str())
                .bind(channels.to_db())
                .execute(pool)
                .await?;
            }
            Some(ws) => {
                sqlx::query(
                    "INSERT INTO notify_rule (workspace_id, kind, channels) \
                     VALUES (?, ?, ?) \
                     ON CONFLICT (workspace_id, kind) WHERE workspace_id IS NOT NULL \
                     DO UPDATE SET channels = excluded.channels",
                )
                .bind(ws)
                .bind(kind.as_str())
                .bind(channels.to_db())
                .execute(pool)
                .await?;
            }
        }
        Ok(())
    }

    /// Remove a per-workspace override for `kind`, reverting that workspace to the
    /// global default. A no-op when no override exists (and never touches the
    /// global row). Returns the number of rows removed (`0` or `1`).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the delete fails.
    pub async fn clear_override(
        pool: &SqlitePool,
        workspace_id: &str,
        kind: AttentionKind,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query(
            "DELETE FROM notify_rule WHERE workspace_id = ? AND kind = ?",
        )
        .bind(workspace_id)
        .bind(kind.as_str())
        .execute(pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// The global channel set for `kind`, or [`coded_fallback`] if the global row
    /// is somehow absent.
    async fn global_or_fallback(
        pool: &SqlitePool,
        kind: AttentionKind,
    ) -> Result<ChannelSet, sqlx::Error> {
        Ok(fetch_channels(pool, None, kind).await?.unwrap_or_else(|| coded_fallback(kind)))
    }
}

/// Every attention kind in declaration order — the settings-grid row order and
/// the seeded-rule order.
const ALL_KINDS: &[AttentionKind] = &[
    AttentionKind::AskUserQuestion,
    AttentionKind::Approval,
    AttentionKind::CodexRequestUser,
    AttentionKind::Error,
    AttentionKind::Waiting,
    AttentionKind::Escalation,
];

/// Fetch one rule's stored channel set, `None` when no row exists for that exact
/// scope + kind. `scope = None` selects the global row (`workspace_id IS NULL`);
/// `Some(ws)` selects that workspace's override row.
async fn fetch_channels(
    pool: &SqlitePool,
    scope: Option<&str>,
    kind: AttentionKind,
) -> Result<Option<ChannelSet>, sqlx::Error> {
    let row = match scope {
        Some(ws) => {
            sqlx::query("SELECT channels FROM notify_rule WHERE workspace_id = ? AND kind = ?")
                .bind(ws)
                .bind(kind.as_str())
                .fetch_optional(pool)
                .await?
        }
        None => {
            sqlx::query("SELECT channels FROM notify_rule WHERE workspace_id IS NULL AND kind = ?")
                .bind(kind.as_str())
                .fetch_optional(pool)
                .await?
        }
    };
    row.map(|r| r.try_get::<String, _>("channels").map(|s| ChannelSet::from_db(&s)))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use ainb_hangar_core::channel::Channel;

    async fn seed_workspace(pool: &SqlitePool, ws: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(ws)
            .bind(ws)
            .bind(ws)
            .bind(1_000_i64)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn migration_seeds_the_six_global_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        // The seeded defaults resolve for a no-workspace host session.
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::Escalation, None).await.unwrap(),
            ChannelSet::from_channels([Channel::Phone, Channel::Web, Channel::Os]),
        );
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::AskUserQuestion, None).await.unwrap(),
            ChannelSet::from_channels([Channel::Web, Channel::Os]),
        );
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::Approval, None).await.unwrap(),
            ChannelSet::from_channels([Channel::Web, Channel::Os]),
        );
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::CodexRequestUser, None).await.unwrap(),
            ChannelSet::from_channels([Channel::Web, Channel::Os]),
        );
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::Error, None).await.unwrap(),
            ChannelSet::from_channels([Channel::Os]),
        );
        // waiting is board-only (empty set), NOT a missing rule.
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::Waiting, None).await.unwrap(),
            ChannelSet::NONE,
        );
    }

    #[tokio::test]
    async fn workspace_override_beats_global_and_falls_back_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_workspace(pool, "ws-a").await;

        // No override yet → the workspace resolves the global default.
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::AskUserQuestion, Some("ws-a"))
                .await
                .unwrap(),
            ChannelSet::from_channels([Channel::Web, Channel::Os]),
        );

        // Set a workspace override: ASK → phone only.
        NotifyRuleRepo::set(
            pool,
            Some("ws-a"),
            AttentionKind::AskUserQuestion,
            ChannelSet::from_channels([Channel::Phone]),
        )
        .await
        .unwrap();

        // The override wins for ws-a...
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::AskUserQuestion, Some("ws-a"))
                .await
                .unwrap(),
            ChannelSet::from_channels([Channel::Phone]),
        );
        // ...but the global row is untouched (a no-workspace session, and a
        // DIFFERENT workspace, both still see web+os).
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::AskUserQuestion, None).await.unwrap(),
            ChannelSet::from_channels([Channel::Web, Channel::Os]),
        );
        seed_workspace(pool, "ws-b").await;
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::AskUserQuestion, Some("ws-b"))
                .await
                .unwrap(),
            ChannelSet::from_channels([Channel::Web, Channel::Os]),
        );

        // Clearing the override reverts ws-a to the global default.
        assert_eq!(
            NotifyRuleRepo::clear_override(pool, "ws-a", AttentionKind::AskUserQuestion)
                .await
                .unwrap(),
            1,
        );
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::AskUserQuestion, Some("ws-a"))
                .await
                .unwrap(),
            ChannelSet::from_channels([Channel::Web, Channel::Os]),
        );
    }

    #[tokio::test]
    async fn set_upserts_the_global_row_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        // Overwrite a global default; the row is replaced, not duplicated.
        NotifyRuleRepo::set(pool, None, AttentionKind::Error, ChannelSet::from_channels([Channel::Phone, Channel::Os]))
            .await
            .unwrap();
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::Error, None).await.unwrap(),
            ChannelSet::from_channels([Channel::Phone, Channel::Os]),
        );
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM notify_rule WHERE workspace_id IS NULL AND kind = 'error'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(n, 1, "a global upsert must replace, not duplicate");
    }

    #[tokio::test]
    async fn resolve_falls_back_to_coded_default_when_the_rule_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        // Simulate a truncated table (an unknown/missing kind rule): delete every
        // global row, then resolve. The coded fallback holds — escalation stays
        // loud, everything else degrades to board-only, never a panic.
        sqlx::query("DELETE FROM notify_rule").execute(pool).await.unwrap();
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::Escalation, None).await.unwrap(),
            ChannelSet::from_channels([Channel::Phone, Channel::Web, Channel::Os]),
        );
        assert_eq!(
            NotifyRuleRepo::resolve(pool, AttentionKind::Error, None).await.unwrap(),
            ChannelSet::NONE,
        );
    }

    #[tokio::test]
    async fn list_returns_every_kind_marking_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_workspace(pool, "ws-a").await;

        NotifyRuleRepo::set(
            pool,
            Some("ws-a"),
            AttentionKind::Error,
            ChannelSet::from_channels([Channel::Phone]),
        )
        .await
        .unwrap();

        let grid = NotifyRuleRepo::list(pool, Some("ws-a")).await.unwrap();
        assert_eq!(grid.len(), ALL_KINDS.len(), "one row per kind, no gaps");

        // The error row is the override; every other row is the inherited global.
        let err = grid.iter().find(|r| r.kind == AttentionKind::Error).unwrap();
        assert!(err.overridden, "error was overridden for ws-a");
        assert_eq!(err.channels, ChannelSet::from_channels([Channel::Phone]));

        let ask = grid.iter().find(|r| r.kind == AttentionKind::AskUserQuestion).unwrap();
        assert!(!ask.overridden, "ask inherits the global default");
        assert_eq!(ask.channels, ChannelSet::from_channels([Channel::Web, Channel::Os]));

        // The global grid marks nothing overridden.
        let global = NotifyRuleRepo::list(pool, None).await.unwrap();
        assert!(global.iter().all(|r| !r.overridden));
    }
}
