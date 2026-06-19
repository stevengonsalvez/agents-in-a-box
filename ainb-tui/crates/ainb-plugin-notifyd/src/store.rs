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

/// One row in the append-only `events` log. The event store's source
/// of truth: every managed hook fire is recorded here verbatim
/// (bounded), then folded into [`StateRow`] by the materializer.
///
/// `seq` is assigned by SQLite (`AUTOINCREMENT`) on insert, so
/// [`EventRow`]s handed to [`Store::append_event`] carry `seq = 0`
/// (ignored); rows returned by [`Store::events_since`] carry the real
/// monotonic `seq` a materializer pages through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    /// Monotonic sequence (SQLite rowid). `0` on rows being inserted.
    pub seq: i64,
    /// Epoch milliseconds — when the hook fired.
    pub ts: i64,
    /// Host agent's session id (universal hook field).
    pub session_id: String,
    /// Working directory at hook-fire time (universal hook field).
    pub cwd: String,
    /// Path to the session transcript (universal hook field). Stamped
    /// so a reader/materializer never recomputes `cwd→slug`.
    pub transcript_path: String,
    /// Host agent: `claude` | `codex` | … Defaults to `claude`.
    pub agent: String,
    /// Raw hook event name: `Stop` | `SessionStart` | `PreToolUse` | …
    pub event_type: String,
    /// Discriminator parsed from the payload (e.g. `AskUserQuestion`
    /// for `PreToolUse`, `idle_prompt` for `Notification`, the
    /// `error_type` for `StopFailure`). `None` when not applicable.
    pub matcher: Option<String>,
    /// The raw hook stdin JSON (bounded), serialized as a string.
    pub payload: String,
}

/// One materialized `current_state` row — the latest folded state for a
/// `(session_id, cwd)` pair. Written by the Wave 3 transition loop and
/// read by every reader; defined here so the schema + round-trip ship
/// in Wave 2 exactly as a materializer will consume them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRow {
    /// Host agent's session id.
    pub session_id: String,
    /// Working directory.
    pub cwd: String,
    /// Folded classification: `ASK` | `ERR` | `WAIT` | `IDLE` |
    /// `RUNNING` | `DONE`.
    pub kind: String,
    /// JSON context (ASK question+options, ERR error_type, …). `None`
    /// when the kind carries no detail.
    pub context: Option<String>,
    /// Parent session id, when this session is a fleet child.
    pub parent: Option<String>,
    /// `ts` of the newest event that produced this state.
    pub last_event_ts: i64,
    /// Provenance: `hook` (event-sourced) | `tmux` (fallback scan).
    pub source: String,
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
        // A second writer is coming: the daemon's accept-loop persists
        // notifications while the ingest tailer (and, in Wave 3, the
        // materializer) writes the event log + current_state. WAL lets
        // them not block, but a brief window where both hold the write
        // lock can still surface SQLITE_BUSY — give writers up to 5s to
        // retry transparently rather than erroring out.
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
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

            -- Event-sourcing tables (Wave 2). Additive `IF NOT EXISTS`
            -- so this migration is safe on an existing notifications.db
            -- without a version bump.

            -- Append-only event log: every managed hook fire, verbatim.
            CREATE TABLE IF NOT EXISTS events (
                seq             INTEGER PRIMARY KEY AUTOINCREMENT,
                ts              INTEGER NOT NULL,
                session_id      TEXT NOT NULL,
                cwd             TEXT NOT NULL,
                transcript_path TEXT NOT NULL DEFAULT '',
                agent           TEXT NOT NULL DEFAULT 'claude',
                event_type      TEXT NOT NULL,
                matcher         TEXT,
                payload         TEXT NOT NULL DEFAULT '{}'
            );
            CREATE INDEX IF NOT EXISTS idx_events_session
                ON events(session_id, seq);

            -- Materialized read model: latest folded state per session.
            CREATE TABLE IF NOT EXISTS current_state (
                session_id     TEXT NOT NULL,
                cwd            TEXT NOT NULL,
                kind           TEXT NOT NULL,
                context        TEXT,
                parent         TEXT,
                last_event_ts  INTEGER NOT NULL,
                source         TEXT NOT NULL,
                PRIMARY KEY (session_id, cwd)
            );

            -- Durable ingest cursor: byte offset already consumed from
            -- events.jsonl. Single row pinned at id=0.
            CREATE TABLE IF NOT EXISTS ingest_offset (
                id          INTEGER PRIMARY KEY CHECK (id = 0),
                byte_offset INTEGER NOT NULL
            );

            -- File-identity fingerprint for events.jsonl, so the ingest tailer
            -- can detect a truncate-then-regrow (the file replaced / rotated out
            -- from under a stale offset). Single row pinned at id=0: the inode
            -- the offset was taken against + a length checkpoint. When the inode
            -- changes (rotation) or the on-disk length drops below the
            -- checkpoint (truncation), the offset is stale and reset to 0.
            CREATE TABLE IF NOT EXISTS ingest_fileid (
                id     INTEGER PRIMARY KEY CHECK (id = 0),
                inode  INTEGER NOT NULL,
                len    INTEGER NOT NULL
            );

            -- Durable materialize cursor (Wave 3): the highest events.seq the
            -- transition loop has folded into current_state. Mirrors
            -- ingest_offset's single-pinned-row shape so a daemon restart
            -- resumes the fold without re-applying already-folded events.
            CREATE TABLE IF NOT EXISTS materialize_offset (
                id   INTEGER PRIMARY KEY CHECK (id = 0),
                seq  INTEGER NOT NULL
            );
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

    /// Every non-dismissed notification with `ts > since_ms`, newest
    /// first, capped at `limit`. Backs the ainb-tui per-session
    /// attention marker: the TUI pulls recent hook activity in one
    /// cheap read (the `ts` filter keeps it off the full table) and
    /// classifies each row via
    /// [`classify_attention`](crate::classify_attention) to decide the
    /// `[!]` / `[?]` / `[✓]` marker for the originating session.
    pub fn recent_since(
        &self,
        since_ms: i64,
        limit: u32,
    ) -> Result<Vec<NotificationRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, ts, agent, session_id, cwd, project, raw_event, payload, read, dismissed
             FROM notifications
             WHERE ts > ?1 AND dismissed = 0
             ORDER BY ts DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since_ms, limit], |r| {
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

    // --- event-sourcing (Wave 2) --------------------------------------------

    /// Append one event to the append-only `events` log. `row.seq` is
    /// ignored — SQLite assigns the monotonic `seq`. Returns it.
    pub fn append_event(&self, row: &EventRow) -> Result<i64, StoreError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO events
                (ts, session_id, cwd, transcript_path, agent, event_type, matcher, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                row.ts,
                &row.session_id,
                &row.cwd,
                &row.transcript_path,
                &row.agent,
                &row.event_type,
                &row.matcher,
                &row.payload,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Every event with `seq > after_seq`, ascending. The materializer
    /// pages through the log by passing the highest `seq` it has folded.
    pub fn events_since(&self, after_seq: i64) -> Result<Vec<EventRow>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT seq, ts, session_id, cwd, transcript_path, agent, event_type, matcher, payload
             FROM events
             WHERE seq > ?1
             ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![after_seq], |r| {
            Ok(EventRow {
                seq: r.get(0)?,
                ts: r.get(1)?,
                session_id: r.get(2)?,
                cwd: r.get(3)?,
                transcript_path: r.get(4)?,
                agent: r.get(5)?,
                event_type: r.get(6)?,
                matcher: r.get(7)?,
                payload: r.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Upsert one materialized state row, keyed on `(session_id, cwd)`.
    /// Last-write-wins on the whole row (the materializer always carries
    /// the freshest fold), EXCEPT `parent`, which is sticky across the session
    /// lifetime: `COALESCE(excluded.parent, parent)` keeps a previously-recorded
    /// parentage when a later batch's events omit it. Parentage is established
    /// once (the first event that carries it) and never legitimately cleared, so
    /// a NULL in a later fold must not clobber it — without the COALESCE a
    /// session's `parent` would flicker to NULL on any event that lacks it.
    pub fn upsert_current_state(&self, row: &StateRow) -> Result<(), StoreError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO current_state
                (session_id, cwd, kind, context, parent, last_event_ts, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(session_id, cwd) DO UPDATE SET
                kind          = excluded.kind,
                context       = excluded.context,
                parent        = COALESCE(excluded.parent, parent),
                last_event_ts = excluded.last_event_ts,
                source        = excluded.source",
            params![
                &row.session_id,
                &row.cwd,
                &row.kind,
                &row.context,
                &row.parent,
                row.last_event_ts,
                &row.source,
            ],
        )?;
        Ok(())
    }

    /// Look up one materialized state row.
    pub fn get_current_state(
        &self,
        session_id: &str,
        cwd: &str,
    ) -> Result<Option<StateRow>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT session_id, cwd, kind, context, parent, last_event_ts, source
             FROM current_state
             WHERE session_id = ?1 AND cwd = ?2",
        )?;
        let row = stmt
            .query_row(params![session_id, cwd], |r| {
                Ok(StateRow {
                    session_id: r.get(0)?,
                    cwd: r.get(1)?,
                    kind: r.get(2)?,
                    context: r.get(3)?,
                    parent: r.get(4)?,
                    last_event_ts: r.get(5)?,
                    source: r.get(6)?,
                })
            })
            .optional()?;
        Ok(row)
    }

    /// Every materialized state row. Backs the readers' `current_state`
    /// query (Wave 4).
    pub fn list_current_state(&self) -> Result<Vec<StateRow>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT session_id, cwd, kind, context, parent, last_event_ts, source
             FROM current_state
             ORDER BY last_event_ts DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(StateRow {
                session_id: r.get(0)?,
                cwd: r.get(1)?,
                kind: r.get(2)?,
                context: r.get(3)?,
                parent: r.get(4)?,
                last_event_ts: r.get(5)?,
                source: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Every `RUNNING` row whose `context` carries a `stopped_ts` marker — the
    /// stop-pending rows the transition loop's time-driven re-sweep promotes to
    /// `IDLE` on age. Filtering in SQL (`kind = 'RUNNING'` + a `LIKE` on the
    /// marker key) keeps the re-sweep off the full `current_state` table; the
    /// caller re-parses `context` to read the exact `stopped_ts`. The `LIKE` is
    /// a cheap pre-filter, not the source of truth — the JSON parse is.
    pub fn list_stop_pending_running(&self) -> Result<Vec<StateRow>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT session_id, cwd, kind, context, parent, last_event_ts, source
             FROM current_state
             WHERE kind = 'RUNNING' AND context LIKE '%stopped_ts%'",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(StateRow {
                session_id: r.get(0)?,
                cwd: r.get(1)?,
                kind: r.get(2)?,
                context: r.get(3)?,
                parent: r.get(4)?,
                last_event_ts: r.get(5)?,
                source: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Read the durable ingest cursor (byte offset already consumed from
    /// `events.jsonl`). `0` when nothing has been ingested yet.
    pub fn read_ingest_offset(&self) -> Result<u64, StoreError> {
        let conn = self.conn();
        let off: Option<i64> = conn
            .query_row(
                "SELECT byte_offset FROM ingest_offset WHERE id = 0",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(off.unwrap_or(0).max(0) as u64)
    }

    /// Persist the ingest cursor. Single pinned row at `id = 0`.
    pub fn write_ingest_offset(&self, byte_offset: u64) -> Result<(), StoreError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO ingest_offset (id, byte_offset) VALUES (0, ?1)
             ON CONFLICT(id) DO UPDATE SET byte_offset = excluded.byte_offset",
            params![byte_offset as i64],
        )?;
        Ok(())
    }

    /// Commit a whole ingest pass ATOMICALLY: every event in `rows` is inserted
    /// AND the ingest offset is advanced to `new_offset` inside ONE SQLite
    /// transaction. Either the entire pass lands or none of it does.
    ///
    /// This closes the dup-on-crash window: with separate autocommits (one per
    /// `append_event` plus a final `write_ingest_offset`), a crash AFTER the
    /// inserts but BEFORE the offset write would, on restart, re-read the same
    /// suffix and re-insert every row — duplicate `events` seqs. Under one tx a
    /// crash rolls the suffix back (re-read on restart, no dups) or commits it
    /// all (offset matches rows). `seq` is still SQLite-assigned per insert.
    pub fn ingest_batch(&self, rows: &[EventRow], new_offset: u64) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO events
                    (ts, session_id, cwd, transcript_path, agent, event_type, matcher, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for row in rows {
                stmt.execute(params![
                    row.ts,
                    &row.session_id,
                    &row.cwd,
                    &row.transcript_path,
                    &row.agent,
                    &row.event_type,
                    &row.matcher,
                    &row.payload,
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO ingest_offset (id, byte_offset) VALUES (0, ?1)
             ON CONFLICT(id) DO UPDATE SET byte_offset = excluded.byte_offset",
            params![new_offset as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Prune the append-only `events` log to the same retention shape the
    /// notifications table uses: drop rows older than `retention_days` and cap
    /// the table at `max_rows` (oldest `seq` first). Returns rows deleted.
    /// Idempotent; a `0` field disables that axis. Bounds unbounded `events`
    /// growth on a long-lived daemon (the materialize cursor only moves forward,
    /// so already-folded rows are safe to drop once aged out).
    pub fn prune_events(&self, policy: &RetentionPolicy) -> Result<u64, StoreError> {
        let conn = self.conn();
        let mut deleted: u64 = 0;
        if policy.retention_days > 0 {
            let cutoff_ms = (Utc::now().timestamp_millis())
                .saturating_sub((policy.retention_days as i64) * 86_400_000);
            let n = conn.execute("DELETE FROM events WHERE ts < ?1", params![cutoff_ms])?;
            deleted += n as u64;
        }
        if policy.max_rows > 0 {
            let total: u64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
            if total > policy.max_rows {
                let over = total - policy.max_rows;
                let n = conn.execute(
                    "DELETE FROM events
                     WHERE seq IN (SELECT seq FROM events ORDER BY seq ASC LIMIT ?1)",
                    params![over as i64],
                )?;
                deleted += n as u64;
            }
        }
        Ok(deleted)
    }

    /// Read the persisted events.jsonl file-identity fingerprint (`inode`, the
    /// length checkpoint). `None` when nothing has been ingested yet (no
    /// fingerprint recorded), so the first pass establishes it.
    pub fn read_ingest_fileid(&self) -> Result<Option<(u64, u64)>, StoreError> {
        let conn = self.conn();
        let row: Option<(i64, i64)> = conn
            .query_row(
                "SELECT inode, len FROM ingest_fileid WHERE id = 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(i, l)| (i.max(0) as u64, l.max(0) as u64)))
    }

    /// Persist the events.jsonl file-identity fingerprint. Single pinned row at
    /// `id = 0`. Written alongside the offset each pass so a restart can detect
    /// a rotate / truncate-regrow.
    pub fn write_ingest_fileid(&self, inode: u64, len: u64) -> Result<(), StoreError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO ingest_fileid (id, inode, len) VALUES (0, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET inode = excluded.inode, len = excluded.len",
            params![inode as i64, len as i64],
        )?;
        Ok(())
    }

    /// Read the durable materialize cursor (the highest `events.seq` already
    /// folded into `current_state`). `0` when nothing has been folded yet, so
    /// the first pass replays the whole log via [`Self::events_since`]`(0)`.
    pub fn read_materialize_seq(&self) -> Result<i64, StoreError> {
        let conn = self.conn();
        let seq: Option<i64> = conn
            .query_row("SELECT seq FROM materialize_offset WHERE id = 0", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(seq.unwrap_or(0).max(0))
    }

    /// Persist the materialize cursor. Single pinned row at `id = 0`.
    pub fn write_materialize_seq(&self, seq: i64) -> Result<(), StoreError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO materialize_offset (id, seq) VALUES (0, ?1)
             ON CONFLICT(id) DO UPDATE SET seq = excluded.seq",
            params![seq],
        )?;
        Ok(())
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
    fn recent_since_filters_by_ts_and_skips_dismissed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        store.insert(&env("claude", "Stop", 100)).unwrap();
        store.insert(&env("claude", "Notification", 200)).unwrap();
        let dismissed = store.insert(&env("claude", "PermissionRequest", 300)).unwrap();
        store.dismiss(&dismissed).unwrap();

        // `since` is exclusive: ts=100 is excluded, 200 included, 300 dismissed.
        let rows = store.recent_since(100, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts, 200);
        assert_eq!(rows[0].raw_event, "Notification");

        // Newest-first ordering across the window.
        let all = store.recent_since(0, 10).unwrap();
        assert_eq!(all.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![200, 100]);
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

    // --- event-sourcing (Wave 2) --------------------------------------------

    fn event(seq: i64, ts: i64, kind: &str, matcher: Option<&str>) -> EventRow {
        EventRow {
            seq,
            ts,
            session_id: "sess-1".into(),
            cwd: "/tmp/proj".into(),
            transcript_path: "/t/sess-1.jsonl".into(),
            agent: "claude".into(),
            event_type: kind.into(),
            matcher: matcher.map(str::to_string),
            payload: r#"{"k":"v"}"#.into(),
        }
    }

    #[test]
    fn event_tables_create_on_fresh_db() {
        // A brand-new db gets the events/current_state/ingest_offset
        // tables via migrate(); all event APIs work immediately.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("fresh.db")).unwrap();
        assert_eq!(store.events_since(0).unwrap().len(), 0);
        assert_eq!(store.read_ingest_offset().unwrap(), 0);
        assert!(store.list_current_state().unwrap().is_empty());
    }

    #[test]
    fn event_tables_create_on_preexisting_notifications_db() {
        // Open once (creates only the notifications table in the "old"
        // world), insert a notification, drop, then re-open: the new
        // event tables must be added without disturbing existing data.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        {
            let store = Store::open(&path).unwrap();
            store.insert(&env("claude", "Stop", 1)).unwrap();
            assert_eq!(store.count().unwrap(), 1);
        }
        // Re-open: migrate() is idempotent and additive.
        let store = Store::open(&path).unwrap();
        assert_eq!(store.count().unwrap(), 1, "existing rows preserved");
        // And the event tables are now usable.
        let seq = store.append_event(&event(0, 100, "Stop", None)).unwrap();
        assert!(seq > 0);
        assert_eq!(store.events_since(0).unwrap().len(), 1);
    }

    #[test]
    fn append_event_and_events_since_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let s1 = store.append_event(&event(0, 100, "SessionStart", None)).unwrap();
        let s2 = store
            .append_event(&event(0, 200, "PreToolUse", Some("AskUserQuestion")))
            .unwrap();
        let s3 = store.append_event(&event(0, 300, "Stop", None)).unwrap();
        assert!(s1 < s2 && s2 < s3, "seq is monotonic");

        // events_since(0) returns all, ascending, with fields intact.
        let all = store.events_since(0).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].event_type, "SessionStart");
        assert_eq!(all[0].seq, s1);
        assert_eq!(all[1].event_type, "PreToolUse");
        assert_eq!(all[1].matcher.as_deref(), Some("AskUserQuestion"));
        assert_eq!(all[1].transcript_path, "/t/sess-1.jsonl");
        assert_eq!(all[1].payload, r#"{"k":"v"}"#);

        // Paging: only events after a given seq.
        let tail = store.events_since(s2).unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].event_type, "Stop");
        assert_eq!(tail[0].seq, s3);
    }

    #[test]
    fn current_state_upsert_get_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let row = StateRow {
            session_id: "s".into(),
            cwd: "/tmp/p".into(),
            kind: "ASK".into(),
            context: Some(r#"{"header":"pick"}"#.into()),
            parent: Some("parent-1".into()),
            last_event_ts: 100,
            source: "hook".into(),
        };
        store.upsert_current_state(&row).unwrap();
        assert_eq!(
            store.get_current_state("s", "/tmp/p").unwrap().unwrap(),
            row
        );

        // Upsert on the same key overwrites (last-write-wins).
        let updated = StateRow {
            kind: "DONE".into(),
            context: None,
            last_event_ts: 200,
            ..row.clone()
        };
        store.upsert_current_state(&updated).unwrap();
        let got = store.get_current_state("s", "/tmp/p").unwrap().unwrap();
        assert_eq!(got.kind, "DONE");
        assert_eq!(got.context, None);
        assert_eq!(got.last_event_ts, 200);

        // Distinct (session,cwd) is a separate row.
        let other = StateRow {
            session_id: "s2".into(),
            ..row.clone()
        };
        store.upsert_current_state(&other).unwrap();
        assert_eq!(store.list_current_state().unwrap().len(), 2);
        assert!(store.get_current_state("missing", "/x").unwrap().is_none());
    }

    #[test]
    fn parent_is_sticky_across_upserts_when_a_later_row_omits_it() {
        // Finding 5: parent established once must survive a later upsert that
        // carries parent=None (a batch whose events lacked the parent field).
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let base = StateRow {
            session_id: "child".into(),
            cwd: "/p".into(),
            kind: "RUNNING".into(),
            context: None,
            parent: Some("par-1".into()),
            last_event_ts: 100,
            source: "hook".into(),
        };
        store.upsert_current_state(&base).unwrap();

        // Later upsert with parent = None must NOT clobber the established one.
        let later = StateRow {
            kind: "ASK".into(),
            parent: None,
            last_event_ts: 200,
            ..base.clone()
        };
        store.upsert_current_state(&later).unwrap();
        let got = store.get_current_state("child", "/p").unwrap().unwrap();
        assert_eq!(got.kind, "ASK", "non-parent fields still last-write-wins");
        assert_eq!(
            got.parent.as_deref(),
            Some("par-1"),
            "parent is sticky across batches (COALESCE)"
        );

        // A NEW non-null parent still updates it (re-parenting is allowed).
        let reparent = StateRow {
            parent: Some("par-2".into()),
            last_event_ts: 300,
            ..base.clone()
        };
        store.upsert_current_state(&reparent).unwrap();
        assert_eq!(
            store.get_current_state("child", "/p").unwrap().unwrap().parent.as_deref(),
            Some("par-2"),
            "a fresh non-null parent overrides"
        );
    }

    #[test]
    fn list_stop_pending_running_finds_only_marked_running_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let mk = |sid: &str, kind: &str, ctx: Option<&str>| StateRow {
            session_id: sid.into(),
            cwd: "/p".into(),
            kind: kind.into(),
            context: ctx.map(str::to_string),
            parent: None,
            last_event_ts: 1,
            source: "hook".into(),
        };
        store
            .upsert_current_state(&mk("pending", "RUNNING", Some(r#"{"stopped_ts":42}"#)))
            .unwrap();
        store.upsert_current_state(&mk("plain", "RUNNING", None)).unwrap();
        store
            .upsert_current_state(&mk("idle", "IDLE", Some(r#"{"idle_minutes":9}"#)))
            .unwrap();
        store
            .upsert_current_state(&mk("ask", "ASK", Some(r#"{"stopped_ts":1}"#)))
            .unwrap();

        let pending = store.list_stop_pending_running().unwrap();
        assert_eq!(pending.len(), 1, "only RUNNING + stopped_ts marker matches");
        assert_eq!(pending[0].session_id, "pending");
    }

    #[test]
    fn ingest_batch_commits_rows_and_offset_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        let rows = vec![
            event(0, 100, "SessionStart", None),
            event(0, 200, "Stop", None),
        ];
        store.ingest_batch(&rows, 4096).unwrap();
        assert_eq!(store.events_since(0).unwrap().len(), 2);
        assert_eq!(
            store.read_ingest_offset().unwrap(),
            4096,
            "offset advanced in the same tx as the inserts"
        );
        // An empty batch still moves the offset (e.g. a corrupt-only pass).
        store.ingest_batch(&[], 8192).unwrap();
        assert_eq!(store.events_since(0).unwrap().len(), 2);
        assert_eq!(store.read_ingest_offset().unwrap(), 8192);
    }

    #[test]
    fn prune_events_trims_by_max_rows_and_age() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        for i in 0..5 {
            store.append_event(&event(0, i, "Stop", None)).unwrap();
        }
        let deleted = store
            .prune_events(&RetentionPolicy {
                retention_days: 0,
                max_rows: 3,
            })
            .unwrap();
        assert_eq!(deleted, 2, "oldest two over the cap removed");
        let remaining = store.events_since(0).unwrap();
        assert_eq!(remaining.len(), 3);
        // Survivors are the newest (highest ts/seq).
        let ts: Vec<i64> = remaining.iter().map(|r| r.ts).collect();
        assert_eq!(ts, vec![2, 3, 4]);
    }

    #[test]
    fn ingest_fileid_persists_and_defaults_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.db");
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.read_ingest_fileid().unwrap(), None);
            store.write_ingest_fileid(12345, 678).unwrap();
            assert_eq!(store.read_ingest_fileid().unwrap(), Some((12345, 678)));
            store.write_ingest_fileid(999, 1000).unwrap();
            assert_eq!(store.read_ingest_fileid().unwrap(), Some((999, 1000)));
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.read_ingest_fileid().unwrap(),
            Some((999, 1000)),
            "fingerprint survives reopen"
        );
    }

    #[test]
    fn ingest_offset_persists_and_defaults_to_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.db");
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.read_ingest_offset().unwrap(), 0);
            store.write_ingest_offset(4096).unwrap();
            assert_eq!(store.read_ingest_offset().unwrap(), 4096);
            // Overwrite the single pinned row.
            store.write_ingest_offset(8192).unwrap();
            assert_eq!(store.read_ingest_offset().unwrap(), 8192);
        }
        // Survives a reopen (durable cursor).
        let store = Store::open(&path).unwrap();
        assert_eq!(store.read_ingest_offset().unwrap(), 8192);
    }
}
