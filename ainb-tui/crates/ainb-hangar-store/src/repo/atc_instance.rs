//! Typed repository over the `atc_instance` + `atc_retry` tables (migration
//! 0028) — the ATC registry and per-session retry ledger (D12, spec P9 §4.7).
//!
//! P9 moves ATC off the launchd/systemd side-files onto the daemon store: an
//! instance is REGISTERED here (by `ainb fleet atc setup`, via the daemon RPC),
//! its heartbeat becomes a daemon cron job (the heartbeat cron reads
//! `next_tick_at` exactly like the autopilot scheduler reads `autopilot`), and
//! the previously-JSON continue-retry cap + escalated flag + note become durable
//! rows the store is the single writer of. `meta.json` / `task-log.md` survive as
//! human-readable audit; THESE rows are the machine truth.

use sqlx::{Row, SqlitePool};

/// A registered ATC instance (the `atc_instance` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtcInstanceRow {
    /// The sanitized instance name (PK).
    pub name: String,
    /// The directory the ATC session drives from (empty when unset).
    pub cwd: String,
    /// The ATC session's tmux target, or `None` when not spawned.
    pub tmux_session: Option<String>,
    /// The cron the daemon fires the heartbeat on (UTC).
    pub heartbeat_cron: String,
    /// The per-session auto-`continue` cap (D12).
    pub err_retry_cap: i64,
    /// Minutes of a quiet fleet before the heartbeat downgrades to an idle ping.
    pub idle_pause_min: i64,
    /// Cached next-firing instant (epoch-ms); `None` = not scheduled.
    pub next_tick_at: Option<i64>,
    /// Whether the heartbeat cron considers this instance.
    pub enabled: bool,
    /// Epoch-ms of the last fired heartbeat, or `None`.
    pub last_heartbeat_at: Option<i64>,
    /// Epoch-ms the instance was registered.
    pub created_at: i64,
}

/// One `atc_retry` ledger row — a (instance, monitored session) pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtcRetryRow {
    /// The owning instance.
    pub instance_name: String,
    /// The monitored session.
    pub session_id: String,
    /// Code-owned count of auto-`continue`s presented for the session.
    pub continue_count: i64,
    /// `true` once the session has been escalated to a human.
    pub escalated: bool,
    /// The ATC session's free-text note, or `None`.
    pub note: Option<String>,
    /// Epoch-ms of the last ledger change.
    pub updated_at: i64,
}

/// The fields `AtcInstanceRepo::register` upserts.
#[derive(Debug, Clone)]
pub struct RegisterAtc {
    /// Sanitized instance name.
    pub name: String,
    /// Working directory.
    pub cwd: String,
    /// tmux target, or `None`.
    pub tmux_session: Option<String>,
    /// Heartbeat cron expression (UTC).
    pub heartbeat_cron: String,
    /// Per-session auto-`continue` cap.
    pub err_retry_cap: i64,
    /// Idle-pause threshold in minutes.
    pub idle_pause_min: i64,
    /// The cached next-firing instant (epoch-ms), computed by the caller from the
    /// heartbeat cron.
    pub next_tick_at: Option<i64>,
}

/// Stateless typed wrapper over the `atc_instance` + `atc_retry` tables.
pub struct AtcInstanceRepo;

impl AtcInstanceRepo {
    /// Register (or re-register) an ATC instance, upserting by name.
    ///
    /// Re-running `atc setup <name>` is idempotent: it refreshes the config +
    /// reschedules, re-enabling the instance and leaving `created_at` at its
    /// original value.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails.
    pub async fn register(
        pool: &SqlitePool,
        req: &RegisterAtc,
        now_ms: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO atc_instance \
                 (name, cwd, tmux_session, heartbeat_cron, err_retry_cap, idle_pause_min, \
                  next_tick_at, enabled, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?) \
             ON CONFLICT(name) DO UPDATE SET \
                 cwd = excluded.cwd, tmux_session = excluded.tmux_session, \
                 heartbeat_cron = excluded.heartbeat_cron, err_retry_cap = excluded.err_retry_cap, \
                 idle_pause_min = excluded.idle_pause_min, next_tick_at = excluded.next_tick_at, \
                 enabled = 1",
        )
        .bind(&req.name)
        .bind(&req.cwd)
        .bind(&req.tmux_session)
        .bind(&req.heartbeat_cron)
        .bind(req.err_retry_cap)
        .bind(req.idle_pause_min)
        .bind(req.next_tick_at)
        .bind(now_ms)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Fetch one instance by name, `None` when it is not registered.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn get(pool: &SqlitePool, name: &str) -> Result<Option<AtcInstanceRow>, sqlx::Error> {
        let row = sqlx::query(SELECT_INSTANCE_COLS).bind(name).fetch_optional(pool).await?;
        Ok(row.as_ref().map(instance_from_sqlite))
    }

    /// List every registered instance, name-ordered.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list(pool: &SqlitePool) -> Result<Vec<AtcInstanceRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT name, cwd, tmux_session, heartbeat_cron, err_retry_cap, idle_pause_min, \
                    next_tick_at, enabled, last_heartbeat_at, created_at \
             FROM atc_instance ORDER BY name ASC",
        )
        .fetch_all(pool)
        .await?;
        Ok(rows.iter().map(instance_from_sqlite).collect())
    }

    /// List every enabled instance with a scheduled next tick, earliest first —
    /// the heartbeat cron's hot read (mirrors the autopilot scheduler).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_schedulable(pool: &SqlitePool) -> Result<Vec<AtcInstanceRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT name, cwd, tmux_session, heartbeat_cron, err_retry_cap, idle_pause_min, \
                    next_tick_at, enabled, last_heartbeat_at, created_at \
             FROM atc_instance \
             WHERE enabled = 1 AND next_tick_at IS NOT NULL \
             ORDER BY next_tick_at ASC",
        )
        .fetch_all(pool)
        .await?;
        Ok(rows.iter().map(instance_from_sqlite).collect())
    }

    /// Persist a recomputed `next_tick_at` for an instance (the heartbeat cron
    /// reschedule).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails.
    pub async fn set_next_tick(
        pool: &SqlitePool,
        name: &str,
        next_tick_at: Option<i64>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE atc_instance SET next_tick_at = ? WHERE name = ?")
            .bind(next_tick_at)
            .bind(name)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Stamp `last_heartbeat_at` after a fired heartbeat.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails.
    pub async fn mark_heartbeat(
        pool: &SqlitePool,
        name: &str,
        now_ms: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE atc_instance SET last_heartbeat_at = ? WHERE name = ?")
            .bind(now_ms)
            .bind(name)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Enable / disable an instance, writing the supplied `next_tick_at`
    /// (a freshly-computed value on enable; `None` on disable so the heartbeat
    /// cron stops scheduling it).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails.
    pub async fn set_enabled(
        pool: &SqlitePool,
        name: &str,
        enabled: bool,
        next_tick_at: Option<i64>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE atc_instance SET enabled = ?, next_tick_at = ? WHERE name = ?")
            .bind(i64::from(enabled))
            .bind(next_tick_at)
            .bind(name)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Read one retry-ledger row, `None` when the session has never been recorded.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn retry_get(
        pool: &SqlitePool,
        instance_name: &str,
        session_id: &str,
    ) -> Result<Option<AtcRetryRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT instance_name, session_id, continue_count, escalated, note, updated_at \
             FROM atc_retry WHERE instance_name = ? AND session_id = ?",
        )
        .bind(instance_name)
        .bind(session_id)
        .fetch_optional(pool)
        .await?;
        Ok(row.as_ref().map(retry_from_sqlite))
    }

    /// Record one auto-`continue` for a session: increment `continue_count` and
    /// return the NEW count. Upserts, so a first continue creates the row.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails.
    pub async fn record_continue(
        pool: &SqlitePool,
        instance_name: &str,
        session_id: &str,
        now_ms: i64,
    ) -> Result<i64, sqlx::Error> {
        sqlx::query(
            "INSERT INTO atc_retry \
                 (instance_name, session_id, continue_count, escalated, updated_at) \
             VALUES (?, ?, 1, 0, ?) \
             ON CONFLICT(instance_name, session_id) DO UPDATE SET \
                 continue_count = continue_count + 1, updated_at = excluded.updated_at",
        )
        .bind(instance_name)
        .bind(session_id)
        .bind(now_ms)
        .execute(pool)
        .await?;
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT continue_count FROM atc_retry WHERE instance_name = ? AND session_id = ?",
        )
        .bind(instance_name)
        .bind(session_id)
        .fetch_one(pool)
        .await?;
        Ok(count)
    }

    /// Mark a session escalated (the human was pulled in). Upserts.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails.
    pub async fn mark_escalated(
        pool: &SqlitePool,
        instance_name: &str,
        session_id: &str,
        now_ms: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO atc_retry \
                 (instance_name, session_id, continue_count, escalated, updated_at) \
             VALUES (?, ?, 0, 1, ?) \
             ON CONFLICT(instance_name, session_id) DO UPDATE SET \
                 escalated = 1, updated_at = excluded.updated_at",
        )
        .bind(instance_name)
        .bind(session_id)
        .bind(now_ms)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Reset a session's retry ledger — call when the session recovers so a fresh
    /// error gets a fresh continue budget (clears count + escalated + note).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails.
    pub async fn reset_retry(
        pool: &SqlitePool,
        instance_name: &str,
        session_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM atc_retry WHERE instance_name = ? AND session_id = ?")
            .bind(instance_name)
            .bind(session_id)
            .execute(pool)
            .await?;
        Ok(())
    }
}

/// The single-instance select column list, shared by `get` (needs the `WHERE`).
const SELECT_INSTANCE_COLS: &str = "SELECT name, cwd, tmux_session, heartbeat_cron, err_retry_cap, idle_pause_min, \
            next_tick_at, enabled, last_heartbeat_at, created_at \
     FROM atc_instance WHERE name = ?";

/// Map one raw `atc_instance` row into an [`AtcInstanceRow`].
fn instance_from_sqlite(row: &sqlx::sqlite::SqliteRow) -> AtcInstanceRow {
    let enabled: i64 = row.get("enabled");
    AtcInstanceRow {
        name: row.get("name"),
        cwd: row.get("cwd"),
        tmux_session: row.get("tmux_session"),
        heartbeat_cron: row.get("heartbeat_cron"),
        err_retry_cap: row.get("err_retry_cap"),
        idle_pause_min: row.get("idle_pause_min"),
        next_tick_at: row.get("next_tick_at"),
        enabled: enabled != 0,
        last_heartbeat_at: row.get("last_heartbeat_at"),
        created_at: row.get("created_at"),
    }
}

/// Map one raw `atc_retry` row into an [`AtcRetryRow`].
fn retry_from_sqlite(row: &sqlx::sqlite::SqliteRow) -> AtcRetryRow {
    let escalated: i64 = row.get("escalated");
    AtcRetryRow {
        instance_name: row.get("instance_name"),
        session_id: row.get("session_id"),
        continue_count: row.get("continue_count"),
        escalated: escalated != 0,
        note: row.get("note"),
        updated_at: row.get("updated_at"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn reg(name: &str, cron: &str, next: Option<i64>) -> RegisterAtc {
        RegisterAtc {
            name: name.to_string(),
            cwd: format!("/work/{name}"),
            tmux_session: Some(format!("atc-{name}")),
            heartbeat_cron: cron.to_string(),
            err_retry_cap: 3,
            idle_pause_min: 60,
            next_tick_at: next,
        }
    }

    #[tokio::test]
    async fn register_then_get_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        AtcInstanceRepo::register(store.pool(), &reg("main", "*/2 * * * *", Some(2000)), 1000)
            .await
            .unwrap();
        let got = AtcInstanceRepo::get(store.pool(), "main").await.unwrap().unwrap();
        assert_eq!(got.name, "main");
        assert_eq!(got.heartbeat_cron, "*/2 * * * *");
        assert_eq!(got.err_retry_cap, 3);
        assert_eq!(got.next_tick_at, Some(2000));
        assert!(got.enabled);
        assert_eq!(got.created_at, 1000);
        assert_eq!(AtcInstanceRepo::list(store.pool()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn register_is_idempotent_and_keeps_created_at() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        AtcInstanceRepo::register(store.pool(), &reg("main", "*/2 * * * *", Some(2000)), 1000)
            .await
            .unwrap();
        // Disable then re-run setup: re-register must re-enable + keep created_at.
        AtcInstanceRepo::set_enabled(store.pool(), "main", false, None).await.unwrap();
        AtcInstanceRepo::register(store.pool(), &reg("main", "*/5 * * * *", Some(9000)), 8000)
            .await
            .unwrap();
        let got = AtcInstanceRepo::get(store.pool(), "main").await.unwrap().unwrap();
        assert!(got.enabled, "re-register re-enables");
        assert_eq!(got.heartbeat_cron, "*/5 * * * *", "config refreshed");
        assert_eq!(
            got.created_at, 1000,
            "created_at preserved across re-register"
        );
        assert_eq!(
            AtcInstanceRepo::list(store.pool()).await.unwrap().len(),
            1,
            "no duplicate row"
        );
    }

    #[tokio::test]
    async fn schedulable_excludes_disabled_and_unscheduled() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        AtcInstanceRepo::register(store.pool(), &reg("a", "* * * * *", Some(3000)), 1)
            .await
            .unwrap();
        AtcInstanceRepo::register(store.pool(), &reg("b", "* * * * *", Some(1000)), 1)
            .await
            .unwrap();
        AtcInstanceRepo::register(store.pool(), &reg("c", "* * * * *", None), 1)
            .await
            .unwrap();
        AtcInstanceRepo::set_enabled(store.pool(), "a", false, Some(3000))
            .await
            .unwrap();
        let sched = AtcInstanceRepo::list_schedulable(store.pool()).await.unwrap();
        // `a` is disabled, `c` has no next_tick → only `b`, and earliest-first.
        assert_eq!(
            sched.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["b"]
        );
    }

    #[tokio::test]
    async fn retry_ledger_records_and_resets() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        AtcInstanceRepo::register(store.pool(), &reg("main", "* * * * *", Some(1)), 1)
            .await
            .unwrap();
        assert_eq!(
            AtcInstanceRepo::record_continue(store.pool(), "main", "s1", 100).await.unwrap(),
            1
        );
        assert_eq!(
            AtcInstanceRepo::record_continue(store.pool(), "main", "s1", 200).await.unwrap(),
            2
        );
        AtcInstanceRepo::mark_escalated(store.pool(), "main", "s1", 300).await.unwrap();
        let row = AtcInstanceRepo::retry_get(store.pool(), "main", "s1").await.unwrap().unwrap();
        assert_eq!(row.continue_count, 2);
        assert!(row.escalated);
        // Recovery clears the ledger so a fresh error gets a fresh budget.
        AtcInstanceRepo::reset_retry(store.pool(), "main", "s1").await.unwrap();
        assert!(AtcInstanceRepo::retry_get(store.pool(), "main", "s1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn mark_heartbeat_and_reschedule() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        AtcInstanceRepo::register(store.pool(), &reg("main", "* * * * *", Some(1000)), 1)
            .await
            .unwrap();
        AtcInstanceRepo::mark_heartbeat(store.pool(), "main", 5000).await.unwrap();
        AtcInstanceRepo::set_next_tick(store.pool(), "main", Some(6000)).await.unwrap();
        let got = AtcInstanceRepo::get(store.pool(), "main").await.unwrap().unwrap();
        assert_eq!(got.last_heartbeat_at, Some(5000));
        assert_eq!(got.next_tick_at, Some(6000));
    }
}
