//! SQLite-backed persistence for notification envelopes.
//!
//! The store owns its own database file (`notifications.db` next to
//! the socket) — not the cache `usage.db` owned by the
//! `ainb-plugin-session-reader` plugin. Two reasons:
//!
//! 1. Concern separation: `notifyd` is a hot writer; `session-reader`
//!    is a batch parser. Sharing one file mixes lifecycles.
//! 2. Schema migrations stay independent: a notifyd schema change
//!    must not risk the session cache.
//!
//! The schema and indexes match the spec's "Data model" section.

use std::path::Path;
use std::sync::Mutex;

use chrono::Utc;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use thiserror::Error;
use uuid::Uuid;

use crate::envelope::Envelope;

/// Errors surfaced by the [`Store`].
#[derive(Debug, Error)]
pub enum StoreError {
    /// Underlying SQLite failure.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// JSON (de)serialization failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Retention policy applied on every insert. Mirrors the runtime
/// config in `~/.agents-in-a-box/config.toml [notifyd]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Delete rows whose `ts` is older than this many days. `0` =
    /// disabled.
    pub retention_days: u32,
    /// Cap the table at this many rows; the oldest are pruned on
    /// every insert. `0` = disabled.
    pub max_rows: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        // Spec defaults: 7 days, 10k rows.
        Self {
            retention_days: 7,
            max_rows: 10_000,
        }
    }
}

/// One row in the `notifications` table. Mirrors the [`Envelope`]
/// plus per-row state columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationRecord {
    /// Row primary key (UUID v4).
    pub id: String,
    /// Epoch milliseconds — the time the hook fired.
    pub ts: i64,
    /// Host agent: `claude` | `codex`.
    pub agent: String,
    /// Host agent's session id.
    pub session_id: String,
    /// Working directory at the time of the hook.
    pub cwd: String,
    /// `basename(cwd)`.
    pub project: String,
    /// Raw event name preserved from the host agent.
    pub raw_event: String,
    /// Original hook payload, serialized as a JSON string.
    pub payload_json: String,
    /// 0 / 1 — has the user opened the detail pane for this row.
    pub read: bool,
    /// 0 / 1 — has the user dismissed (archived) this row.
    pub dismissed: bool,
}

/// Thin handle around a SQLite connection. Cheap to drop and reopen,
/// but the daemon keeps one open for the process lifetime.
///
/// The connection sits behind a `Mutex` because rusqlite's
/// `Connection` is `!Sync` (it holds a `RefCell` internally), and
/// the daemon serves accept-loop tasks from multiple worker threads.
/// Contention is minimal — inserts are short and we already serialise
/// to a single SQLite file.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open (or create) the database at `path`. Applies the schema
    /// + indexes on first use; idempotent on subsequent opens.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        // Parent dir is the daemon's responsibility (it ensures the
        // base directory exists before construction); we still
        // create parents here as a defence-in-depth so a misconfig
        // does not crash with EACCES.
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        // WAL mode keeps readers (TUI) and the writer (notifyd) from
        // blocking each other.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("store mutex poisoned")
    }

    /// Apply the schema. Each statement is `CREATE ... IF NOT
    /// EXISTS` so re-running is a no-op.
    fn migrate(&self) -> Result<(), StoreError> {
        self.conn().execute_batch(
            "
            CREATE TABLE IF NOT EXISTS notifications (
                id          TEXT PRIMARY KEY,
                ts          INTEGER NOT NULL,
                agent       TEXT NOT NULL,
                session_id  TEXT NOT NULL DEFAULT '',
                cwd         TEXT NOT NULL DEFAULT '',
                project     TEXT NOT NULL DEFAULT '',
                raw_event   TEXT NOT NULL,
                payload     TEXT NOT NULL DEFAULT '{}',
                read        INTEGER NOT NULL DEFAULT 0,
                dismissed   INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_notifications_ts
                ON notifications(ts);
            CREATE INDEX IF NOT EXISTS idx_notifications_session
                ON notifications(session_id);
            CREATE INDEX IF NOT EXISTS idx_notifications_project
                ON notifications(project);
            CREATE INDEX IF NOT EXISTS idx_notifications_agent
                ON notifications(agent);
            CREATE INDEX IF NOT EXISTS idx_notifications_unread
                ON notifications(read, dismissed)
                WHERE read = 0 AND dismissed = 0;
            ",
        )?;
        Ok(())
    }

    /// Persist an envelope as a new row. Returns the generated id.
    pub fn insert(&self, env: &Envelope) -> Result<String, StoreError> {
        let id = Uuid::new_v4().to_string();
        let payload_json = serde_json::to_string(&env.payload)?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO notifications (id, ts, agent, session_id, cwd, project, raw_event, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &id,
                env.ts,
                &env.agent,
                &env.session_id,
                &env.cwd,
                &env.project,
                &env.raw_event,
                &payload_json,
            ],
        )?;
        Ok(id)
    }

    /// Insert + apply retention in a single call. The retention
    /// sweep is the spec's "run on each insert" model — cheap when
    /// the table is small, still bounded when bursts happen.
    pub fn insert_and_prune(
        &self,
        env: &Envelope,
        policy: &RetentionPolicy,
    ) -> Result<String, StoreError> {
        let id = self.insert(env)?;
        self.prune(policy)?;
        Ok(id)
    }

    /// Delete rows beyond the configured policy. Idempotent.
    pub fn prune(&self, policy: &RetentionPolicy) -> Result<u64, StoreError> {
        let conn = self.conn();
        let mut deleted: u64 = 0;
        if policy.retention_days > 0 {
            let cutoff_ms = (Utc::now().timestamp_millis())
                .saturating_sub((policy.retention_days as i64) * 86_400_000);
            let n = conn.execute(
                "DELETE FROM notifications WHERE ts < ?1",
                params![cutoff_ms],
            )?;
            deleted += n as u64;
        }
        if policy.max_rows > 0 {
            let total: u64 =
                conn.query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))?;
            if total > policy.max_rows {
                let over = total - policy.max_rows;
                // Delete the oldest `over` rows.
                let n = conn.execute(
                    "DELETE FROM notifications
                     WHERE id IN (
                         SELECT id FROM notifications ORDER BY ts ASC LIMIT ?1
                     )",
                    params![over as i64],
                )?;
                deleted += n as u64;
            }
        }
        Ok(deleted)
    }

    /// Count rows currently in the table. Test helper + status verb.
    pub fn count(&self) -> Result<u64, StoreError> {
        let conn = self.conn();
        let n: u64 = conn.query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))?;
        Ok(n)
    }

    /// Count rows that are unread + not dismissed. Used by the
    /// Sessions row badge.
    pub fn unread_count(&self) -> Result<u64, StoreError> {
        let conn = self.conn();
        let n: u64 = conn.query_row(
            "SELECT COUNT(*) FROM notifications WHERE read = 0 AND dismissed = 0",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Unread count grouped by session_id. Used to render the
    /// per-session `●N` badge on the Sessions screen.
    pub fn unread_by_session(&self) -> Result<Vec<(String, u64)>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT session_id, COUNT(*)
             FROM notifications
             WHERE read = 0 AND dismissed = 0
             GROUP BY session_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Unread count grouped by `cwd` (working directory at hook-fire
    /// time). The ainb-tui session list uses this to render a
    /// per-session `●N` badge: ainb's `Session.working_dir` matches
    /// the notification's `cwd` for any session whose host agent has
    /// been firing hooks. This is the cwd-based correlation layer
    /// between ainb's internal `Uuid` ids and the agents' hook-side
    /// session_id strings.
    pub fn unread_by_cwd(&self) -> Result<Vec<(String, u64)>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT cwd, COUNT(*)
             FROM notifications
             WHERE read = 0 AND dismissed = 0 AND cwd != ''
             GROUP BY cwd",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Look up the most-recent notification whose `cwd` matches
    /// `target_cwd`. Used by the Inbox screen when the user presses
    /// Enter on a row: from the row's `cwd` we resolve back to ainb's
    /// `Session` (via `Session.working_dir`) and attach its tmux pane.
    pub fn latest_by_cwd(
        &self,
        target_cwd: &str,
    ) -> Result<Option<NotificationRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, ts, agent, session_id, cwd, project, raw_event, payload, read, dismissed
             FROM notifications
             WHERE cwd = ?1
             ORDER BY ts DESC
             LIMIT 1",
        )?;
        let row = stmt
            .query_row(params![target_cwd], |r| {
                Ok(NotificationRecord {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    agent: r.get(2)?,
                    session_id: r.get(3)?,
                    cwd: r.get(4)?,
                    project: r.get(5)?,
                    raw_event: r.get(6)?,
                    payload_json: r.get(7)?,
                    read: r.get::<_, i64>(8)? != 0,
                    dismissed: r.get::<_, i64>(9)? != 0,
                })
            })
            .optional()?;
        Ok(row)
    }

    /// Most recent record overall. Used by `ainb hooks status`.
    pub fn latest(&self) -> Result<Option<NotificationRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, ts, agent, session_id, cwd, project, raw_event, payload, read, dismissed
             FROM notifications
             ORDER BY ts DESC
             LIMIT 1",
        )?;
        let row = stmt
            .query_row([], |r| {
                Ok(NotificationRecord {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    agent: r.get(2)?,
                    session_id: r.get(3)?,
                    cwd: r.get(4)?,
                    project: r.get(5)?,
                    raw_event: r.get(6)?,
                    payload_json: r.get(7)?,
                    read: r.get::<_, i64>(8)? != 0,
                    dismissed: r.get::<_, i64>(9)? != 0,
                })
            })
            .optional()?;
        Ok(row)
    }

    /// Page through records by descending `ts`. Powers the TUI Inbox
    /// screen's incremental polling.
    pub fn list(
        &self,
        include_dismissed: bool,
        agent_filter: Option<&str>,
        project_filter: Option<&str>,
        limit: u32,
    ) -> Result<Vec<NotificationRecord>, StoreError> {
        let mut where_clauses: Vec<String> = Vec::new();
        if !include_dismissed {
            where_clauses.push("dismissed = 0".into());
        }
        if agent_filter.is_some() {
            where_clauses.push("agent = ?".into());
        }
        if project_filter.is_some() {
            where_clauses.push("project = ?".into());
        }
        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT id, ts, agent, session_id, cwd, project, raw_event, payload, read, dismissed
             FROM notifications
             {where_sql}
             ORDER BY ts DESC
             LIMIT {limit}"
        );
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::new();
        if let Some(a) = agent_filter.as_ref() {
            params_vec.push(a);
        }
        if let Some(p) = project_filter.as_ref() {
            params_vec.push(p);
        }
        let rows = stmt.query_map(rusqlite::params_from_iter(params_vec), |r| {
            Ok(NotificationRecord {
                id: r.get(0)?,
                ts: r.get(1)?,
                agent: r.get(2)?,
                session_id: r.get(3)?,
                cwd: r.get(4)?,
                project: r.get(5)?,
                raw_event: r.get(6)?,
                payload_json: r.get(7)?,
                read: r.get::<_, i64>(8)? != 0,
                dismissed: r.get::<_, i64>(9)? != 0,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Mark a single row as read. Idempotent.
    pub fn mark_read(&self, id: &str) -> Result<bool, StoreError> {
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE notifications SET read = 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(n > 0)
    }

    /// Mark a single row as dismissed (archived). Idempotent.
    pub fn dismiss(&self, id: &str) -> Result<bool, StoreError> {
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE notifications SET dismissed = 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(n > 0)
    }

    /// Dismiss every row that is currently visible — read or unread,
    /// not dismissed. Used by `Shift+C` in the Inbox screen.
    pub fn dismiss_visible(&self) -> Result<u64, StoreError> {
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE notifications SET dismissed = 1 WHERE dismissed = 0",
            [],
        )?;
        Ok(n as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(agent: &str, event: &str, ts: i64) -> Envelope {
        Envelope {
            protocol_version: 1,
            agent: agent.into(),
            raw_event: event.into(),
            session_id: format!("session-{ts}"),
            cwd: "/tmp/x".into(),
            project: "x".into(),
            ts,
            payload: json!({"k": "v"}),
        }
    }

    #[test]
    fn migrate_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store1 = Store::open(dir.path().join("a.db")).unwrap();
        drop(store1);
        let store2 = Store::open(dir.path().join("a.db")).unwrap();
        assert_eq!(store2.count().unwrap(), 0);
    }

    #[test]
    fn insert_then_list_roundtrips_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let e = env("claude", "Stop", 1000);
        let id = store.insert(&e).unwrap();
        let rows = store.list(false, None, None, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].agent, "claude");
        assert_eq!(rows[0].raw_event, "Stop");
        assert!(!rows[0].read);
        assert!(!rows[0].dismissed);
    }

    #[test]
    fn list_orders_by_ts_desc() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        store.insert(&env("claude", "Stop", 1)).unwrap();
        store.insert(&env("codex", "Stop", 3)).unwrap();
        store.insert(&env("claude", "Stop", 2)).unwrap();
        let rows = store.list(false, None, None, 10).unwrap();
        assert_eq!(rows.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![3, 2, 1]);
    }

    #[test]
    fn list_filters_by_agent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        store.insert(&env("claude", "Stop", 1)).unwrap();
        store.insert(&env("codex", "Stop", 2)).unwrap();
        let claude_only = store.list(false, Some("claude"), None, 10).unwrap();
        assert_eq!(claude_only.len(), 1);
        assert_eq!(claude_only[0].agent, "claude");
    }

    #[test]
    fn mark_read_then_unread_count_decrements() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let id = store.insert(&env("claude", "Stop", 1)).unwrap();
        assert_eq!(store.unread_count().unwrap(), 1);
        store.mark_read(&id).unwrap();
        assert_eq!(store.unread_count().unwrap(), 0);
    }

    #[test]
    fn dismiss_hides_row_from_default_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let id = store.insert(&env("claude", "Stop", 1)).unwrap();
        store.dismiss(&id).unwrap();
        assert_eq!(store.list(false, None, None, 10).unwrap().len(), 0);
        assert_eq!(store.list(true, None, None, 10).unwrap().len(), 1);
    }

    #[test]
    fn prune_enforces_max_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        for i in 0..5 {
            store.insert(&env("claude", "Stop", i)).unwrap();
        }
        let policy = RetentionPolicy {
            retention_days: 0,
            max_rows: 3,
        };
        let deleted = store.prune(&policy).unwrap();
        assert_eq!(deleted, 2);
        assert_eq!(store.count().unwrap(), 3);
        // Survivors are the newest 3.
        let surviving_ts: Vec<i64> =
            store.list(false, None, None, 10).unwrap().iter().map(|r| r.ts).collect();
        assert_eq!(surviving_ts, vec![4, 3, 2]);
    }

    #[test]
    fn unread_by_session_groups_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        // Three for s-1, one for s-2; mark one of s-1 read.
        let mut env_a = env("claude", "Stop", 1);
        env_a.session_id = "s-1".into();
        let id1 = store.insert(&env_a).unwrap();
        let mut env_b = env("claude", "Stop", 2);
        env_b.session_id = "s-1".into();
        store.insert(&env_b).unwrap();
        let mut env_c = env("claude", "Stop", 3);
        env_c.session_id = "s-1".into();
        store.insert(&env_c).unwrap();
        let mut env_d = env("codex", "Stop", 4);
        env_d.session_id = "s-2".into();
        store.insert(&env_d).unwrap();
        store.mark_read(&id1).unwrap();
        let mut counts = store.unread_by_session().unwrap();
        counts.sort();
        assert_eq!(counts, vec![("s-1".into(), 2), ("s-2".into(), 1)]);
    }

    #[test]
    fn unread_by_cwd_groups_by_working_dir_and_skips_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        // Three events under /tmp/proj-a, two under /tmp/proj-b, one
        // with empty cwd (must NOT appear in the result), and one
        // dismissed under /tmp/proj-a (must NOT count toward unread).
        let mut e = env("claude", "Stop", 1);
        e.cwd = "/tmp/proj-a".into();
        let _ = store.insert(&e).unwrap();
        e.ts = 2;
        let _ = store.insert(&e).unwrap();
        e.ts = 3;
        let dismissed_id = store.insert(&e).unwrap();
        store.dismiss(&dismissed_id).unwrap();
        e.ts = 4;
        let _ = store.insert(&e).unwrap();
        e.cwd = "/tmp/proj-b".into();
        e.ts = 5;
        let _ = store.insert(&e).unwrap();
        e.ts = 6;
        let _ = store.insert(&e).unwrap();
        e.cwd = "".into();
        e.ts = 7;
        let _ = store.insert(&e).unwrap();

        let mut counts = store.unread_by_cwd().unwrap();
        counts.sort();
        assert_eq!(
            counts,
            vec![("/tmp/proj-a".into(), 3), ("/tmp/proj-b".into(), 2),]
        );
    }

    #[test]
    fn latest_by_cwd_returns_most_recent_match() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let mut e = env("claude", "Stop", 100);
        e.cwd = "/tmp/proj-a".into();
        store.insert(&e).unwrap();
        e.ts = 200;
        e.raw_event = "Notification:idle_prompt".into();
        store.insert(&e).unwrap();
        e.cwd = "/tmp/proj-b".into();
        e.ts = 300;
        e.raw_event = "Stop".into();
        store.insert(&e).unwrap();

        let latest = store.latest_by_cwd("/tmp/proj-a").unwrap().unwrap();
        // Most recent among proj-a is the idle_prompt at ts=200.
        assert_eq!(latest.cwd, "/tmp/proj-a");
        assert_eq!(latest.raw_event, "Notification:idle_prompt");
        assert_eq!(latest.ts, 200);

        let nope = store.latest_by_cwd("/tmp/does-not-exist").unwrap();
        assert!(nope.is_none());
    }
}
