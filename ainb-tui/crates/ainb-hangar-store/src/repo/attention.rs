//! Typed repository wrapper over the `attention` table (migration 0025) — the
//! control-plane inbox (architecture §4.3, spec P2).
//!
//! [`AttentionRepo`] is a thin, stateless sqlx layer over the durable set of
//! input requests every session on the host raises. The daemon's ingest
//! producer classifies a session and calls [`AttentionRepo::insert`]; the
//! control-centre surfaces read the open set via [`AttentionRepo::list_open`]
//! (workspace-scoped) or [`AttentionRepo::list_fleet`] (host-wide); the answer
//! router flips a row through [`AttentionRepo::mark_answered_if_open`].
//!
//! **First-answer-wins** is the whole point of the conditional flip:
//! [`AttentionRepo::mark_answered_if_open`] only updates a row that is still
//! `open`, and returns the number of rows it changed. `0` means the row was
//! already answered (or gone) — the second surface to answer the same question
//! loses the race and is told "already answered by X" WITHOUT a second delivery.
//! That is exactly-once answering enforced at the database, not in application
//! code that could race two connections.

use sqlx::{Row, SqlitePool};

/// The request family an [`AttentionRow`] is about — the `kind` column,
/// CHECK-constrained to these six by migration 0025.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionKind {
    /// A Claude `AskUserQuestion` tool-use (structured, pick-option-N).
    AskUserQuestion,
    /// An approval / permission prompt awaiting yes/no.
    Approval,
    /// A Codex `request-user` free-text prompt.
    CodexRequestUser,
    /// An API/tool error the session is stuck on.
    Error,
    /// An explicit `WAITING:` / needs-input marker.
    Waiting,
    /// An ATC (or agent) escalation that needs a human.
    Escalation,
}

impl AttentionKind {
    /// The wire / column token for this kind (matches the migration's CHECK set).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AskUserQuestion => "ask_user_question",
            Self::Approval => "approval",
            Self::CodexRequestUser => "codex_request_user",
            Self::Error => "error",
            Self::Waiting => "waiting",
            Self::Escalation => "escalation",
        }
    }

    /// Parse a stored `kind` token back into the enum.
    ///
    /// Returns `None` for an unrecognised token (only reachable via raw SQL
    /// tamper — the column carries a CHECK constraint).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ask_user_question" => Some(Self::AskUserQuestion),
            "approval" => Some(Self::Approval),
            "codex_request_user" => Some(Self::CodexRequestUser),
            "error" => Some(Self::Error),
            "waiting" => Some(Self::Waiting),
            "escalation" => Some(Self::Escalation),
            _ => None,
        }
    }
}

/// Parameters for inserting one attention row.
///
/// `id` is minted by the caller (the daemon's ingest producer, like
/// [`super::inbox::NewInboxEntry`]) so the repo stays free of an id-generation
/// dependency. A fresh row is always `open`; only [`AttentionRepo::mark_answered_if_open`]
/// flips it.
#[derive(Debug, Clone)]
pub struct NewAttention {
    /// Primary key (ULID string).
    pub id: String,
    /// The id of the session that raised the request.
    pub session_id: String,
    /// The raising session's working directory (empty when unknown). Carried so
    /// the answer router's C1 guard can correlate by cwd without re-discovery.
    pub cwd: String,
    /// The owning workspace's resolved row id, or `None` for a host session that
    /// belongs to no ainb workspace (a hand-started fleet session).
    pub workspace_id: Option<String>,
    /// The request family.
    pub kind: AttentionKind,
    /// The full serialised request-context JSON (rendered by the surfaces).
    pub payload: String,
    /// `true` when this row came from the pane-classifier fallback for an
    /// unhooked session (degraded fidelity, still answerable).
    pub degraded: bool,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
}

/// A fully-materialised `attention` row read back from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionRow {
    /// Primary key.
    pub id: String,
    /// The session that raised the request.
    pub session_id: String,
    /// The raising session's working directory (empty when unknown).
    pub cwd: String,
    /// The owning workspace, or `None` for a non-workspace host session.
    pub workspace_id: Option<String>,
    /// The request family.
    pub kind: AttentionKind,
    /// The serialised request-context JSON.
    pub payload: String,
    /// `open` while awaiting an answer, `answered` once resolved.
    pub state: String,
    /// `true` when sourced from the degraded pane-classifier fallback.
    pub degraded: bool,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
    /// Which surface/actor answered, or `None` while open.
    pub answered_by: Option<String>,
    /// The delivered answer text, or `None` while open.
    pub answer: Option<String>,
    /// When the answer landed (epoch milliseconds), or `None` while open.
    pub answered_at: Option<i64>,
}

/// Stateless typed wrapper over the `attention` table.
pub struct AttentionRepo;

impl AttentionRepo {
    /// Insert one attention row (always `open`).
    ///
    /// The `workspace_id` FK to `workspace(id)` is enforced only when non-NULL,
    /// so a fleet-wide session with no workspace inserts freely while a bad
    /// workspace id is still rejected.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert fails — e.g. a `kind` CHECK
    /// violation (impossible through [`AttentionKind`], only via raw SQL) or a
    /// dangling `workspace_id` FK violation.
    pub async fn insert(pool: &SqlitePool, row: &NewAttention) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO attention \
             (id, session_id, cwd, workspace_id, kind, payload, state, degraded, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'open', ?, ?)",
        )
        .bind(&row.id)
        .bind(&row.session_id)
        .bind(&row.cwd)
        .bind(&row.workspace_id)
        .bind(row.kind.as_str())
        .bind(&row.payload)
        .bind(i64::from(row.degraded))
        .bind(row.created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// List OPEN attention rows for a workspace scope, oldest first.
    ///
    /// - `Some(ws)` → the open rows owned by that workspace.
    /// - `None`     → the open rows that belong to NO workspace (`workspace_id
    ///   IS NULL`) — hand-started host sessions.
    ///
    /// For the host-wide control centre that wants every open row regardless of
    /// workspace, use [`AttentionRepo::list_fleet`]. Ordered `created_at ASC, id
    /// ASC` so the longest-waiting request is first (the `id` tiebreak keeps two
    /// same-millisecond rows deterministic).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a stored `kind` token is
    /// malformed (impossible given the CHECK constraint).
    pub async fn list_open(
        pool: &SqlitePool,
        workspace_id: Option<&str>,
    ) -> Result<Vec<AttentionRow>, sqlx::Error> {
        let rows = match workspace_id {
            Some(ws) => {
                sqlx::query(
                    "SELECT id, session_id, cwd, workspace_id, kind, payload, state, degraded, \
                            created_at, answered_by, answer, answered_at \
                     FROM attention \
                     WHERE state = 'open' AND workspace_id = ? \
                     ORDER BY created_at ASC, id ASC",
                )
                .bind(ws)
                .fetch_all(pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, session_id, cwd, workspace_id, kind, payload, state, degraded, \
                            created_at, answered_by, answer, answered_at \
                     FROM attention \
                     WHERE state = 'open' AND workspace_id IS NULL \
                     ORDER BY created_at ASC, id ASC",
                )
                .fetch_all(pool)
                .await?
            }
        };
        rows.iter().map(row_from_sqlite).collect()
    }

    /// List EVERY open attention row across every workspace (and the
    /// no-workspace host sessions), oldest first.
    ///
    /// This is the fleet-wide control-centre feed: the converged surface answers
    /// for the whole host, not one workspace. Ordered `created_at ASC, id ASC`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_fleet(pool: &SqlitePool) -> Result<Vec<AttentionRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, session_id, cwd, workspace_id, kind, payload, state, degraded, \
                    created_at, answered_by, answer, answered_at \
             FROM attention \
             WHERE state = 'open' \
             ORDER BY created_at ASC, id ASC",
        )
        .fetch_all(pool)
        .await?;
        rows.iter().map(row_from_sqlite).collect()
    }

    /// Fetch a single attention row by id, `None` when it does not exist.
    ///
    /// The answer router reads the row here to recover the `session_id` + `cwd`
    /// it needs for last-mile delivery before it attempts the flip.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a stored `kind` token is
    /// malformed.
    pub async fn get(
        pool: &SqlitePool,
        id: &str,
    ) -> Result<Option<AttentionRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, session_id, cwd, workspace_id, kind, payload, state, degraded, \
                    created_at, answered_by, answer, answered_at \
             FROM attention WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;
        row.as_ref().map(row_from_sqlite).transpose()
    }

    /// Flip a row `open` → `answered`, but ONLY if it is still open.
    ///
    /// Returns the number of rows changed:
    /// - `1` — this caller won the race; the answer is now recorded and the
    ///   last-mile delivery should proceed.
    /// - `0` — the row was already answered (a second surface lost the race) or
    ///   does not exist; the caller must NOT deliver again, and reports "already
    ///   answered by X" (read the winner via [`AttentionRepo::get`]).
    ///
    /// The `WHERE … AND state = 'open'` predicate is the first-answer-wins guard:
    /// two concurrent answers serialise at the database and exactly one flips the
    /// row, so the answer is delivered exactly once.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the update fails.
    pub async fn mark_answered_if_open(
        pool: &SqlitePool,
        id: &str,
        answered_by: &str,
        answer: &str,
        answered_at: i64,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE attention \
             SET state = 'answered', answered_by = ?, answer = ?, answered_at = ? \
             WHERE id = ? AND state = 'open'",
        )
        .bind(answered_by)
        .bind(answer)
        .bind(answered_at)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Revert a row `answered` → `open`, undoing a claim whose last-mile delivery
    /// failed, but ONLY the claim this caller made.
    ///
    /// The answer router flips a row `open` → `answered` (first-answer-wins) and
    /// only THEN attempts the last-mile send. A transient send miss must not
    /// strand the request in `answered` — the row would leave the open feed
    /// forever while the raising agent is still blocked, and a re-answer would
    /// report "already answered" without ever delivering. This compensating
    /// update puts the row back to `open` (clearing `answered_by` / `answer` /
    /// `answered_at`) so it stays answerable.
    ///
    /// The `answered_by = ? AND answered_at = ?` predicate scopes the revert to
    /// the exact claim the caller just won, so it can never clobber a different
    /// winner. Returns the number of rows reverted (`1` when the caller's claim
    /// was undone, `0` when the row had already moved on).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the update fails.
    pub async fn reopen(
        pool: &SqlitePool,
        id: &str,
        answered_by: &str,
        answered_at: i64,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE attention \
             SET state = 'open', answered_by = NULL, answer = NULL, answered_at = NULL \
             WHERE id = ? AND state = 'answered' AND answered_by = ? AND answered_at = ?",
        )
        .bind(id)
        .bind(answered_by)
        .bind(answered_at)
        .execute(pool)
        .await?;
        Ok(res.rows_affected())
    }
}

/// Map one raw `attention` row into an [`AttentionRow`].
fn row_from_sqlite(row: &sqlx::sqlite::SqliteRow) -> Result<AttentionRow, sqlx::Error> {
    let kind_token: String = row.try_get("kind")?;
    let kind = AttentionKind::parse(&kind_token).ok_or_else(|| sqlx::Error::ColumnDecode {
        index: "kind".to_string(),
        source: format!("unknown attention kind {kind_token:?}").into(),
    })?;
    let degraded: i64 = row.try_get("degraded")?;
    Ok(AttentionRow {
        id: row.try_get("id")?,
        session_id: row.try_get("session_id")?,
        cwd: row.try_get("cwd")?,
        workspace_id: row.try_get("workspace_id")?,
        kind,
        payload: row.try_get("payload")?,
        state: row.try_get("state")?,
        degraded: degraded != 0,
        created_at: row.try_get("created_at")?,
        answered_by: row.try_get("answered_by")?,
        answer: row.try_get("answer")?,
        answered_at: row.try_get("answered_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    /// Seed one workspace so the FK-scoped inserts resolve.
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

    fn ask(id: &str, session: &str, ws: Option<&str>, ts: i64) -> NewAttention {
        NewAttention {
            id: id.into(),
            session_id: session.into(),
            cwd: format!("/work/{session}"),
            workspace_id: ws.map(std::string::ToString::to_string),
            kind: AttentionKind::AskUserQuestion,
            payload: format!("{{\"kind\":\"ASK\",\"id\":\"{id}\"}}"),
            degraded: false,
            created_at: ts,
        }
    }

    #[tokio::test]
    async fn insert_then_list_open_oldest_first_only_open() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_workspace(store.pool(), "ws-a").await;

        for (id, ts) in [("a3", 3000_i64), ("a1", 1000), ("a2", 2000)] {
            AttentionRepo::insert(store.pool(), &ask(id, "sess", Some("ws-a"), ts))
                .await
                .unwrap();
        }

        let open = AttentionRepo::list_open(store.pool(), Some("ws-a")).await.unwrap();
        let ids: Vec<_> = open.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["a1", "a2", "a3"], "open list is oldest-first");
        assert!(open.iter().all(|r| r.state == "open"));
        assert_eq!(open[0].kind, AttentionKind::AskUserQuestion);
        assert_eq!(open[0].cwd, "/work/sess");
        assert!(open[0].payload.contains("ASK"));
    }

    #[tokio::test]
    async fn mark_answered_is_first_answer_wins() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_workspace(store.pool(), "ws-a").await;
        AttentionRepo::insert(store.pool(), &ask("a1", "sess", Some("ws-a"), 1000))
            .await
            .unwrap();

        // First answer wins → exactly one row flipped.
        let first =
            AttentionRepo::mark_answered_if_open(store.pool(), "a1", "tui", "option 2", 5000)
                .await
                .unwrap();
        assert_eq!(first, 1, "the first answer flips the row");

        // Second answer loses → zero rows flipped, the winner is preserved.
        let second =
            AttentionRepo::mark_answered_if_open(store.pool(), "a1", "web", "option 3", 6000)
                .await
                .unwrap();
        assert_eq!(second, 0, "a second answer flips nothing (first-answer-wins)");

        // The answered row leaves the open list and keeps the FIRST answer.
        assert!(
            AttentionRepo::list_open(store.pool(), Some("ws-a")).await.unwrap().is_empty(),
            "an answered row is no longer open"
        );
        let row = AttentionRepo::get(store.pool(), "a1").await.unwrap().unwrap();
        assert_eq!(row.state, "answered");
        assert_eq!(row.answered_by.as_deref(), Some("tui"));
        assert_eq!(row.answer.as_deref(), Some("option 2"));
        assert_eq!(row.answered_at, Some(5000));
    }

    #[tokio::test]
    async fn reopen_undoes_only_the_matching_claim() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_workspace(store.pool(), "ws-a").await;
        AttentionRepo::insert(store.pool(), &ask("a1", "sess", Some("ws-a"), 1000))
            .await
            .unwrap();

        // Claim the row (first-answer-wins), then a failed send reopens it.
        assert_eq!(
            AttentionRepo::mark_answered_if_open(store.pool(), "a1", "tui", "option 2", 5000)
                .await
                .unwrap(),
            1
        );

        // A revert for a DIFFERENT claimant / timestamp changes nothing.
        assert_eq!(
            AttentionRepo::reopen(store.pool(), "a1", "web", 5000).await.unwrap(),
            0,
            "reopen only undoes the exact claim it was asked to"
        );
        assert_eq!(
            AttentionRepo::reopen(store.pool(), "a1", "tui", 4999).await.unwrap(),
            0,
            "a mismatched answered_at does not revert"
        );

        // The matching claim reverts the row back to open + clears the answer.
        assert_eq!(
            AttentionRepo::reopen(store.pool(), "a1", "tui", 5000).await.unwrap(),
            1
        );
        let row = AttentionRepo::get(store.pool(), "a1").await.unwrap().unwrap();
        assert_eq!(row.state, "open", "a failed delivery leaves the row answerable");
        assert!(row.answered_by.is_none());
        assert!(row.answer.is_none());
        assert!(row.answered_at.is_none());
        assert_eq!(
            AttentionRepo::list_open(store.pool(), Some("ws-a")).await.unwrap().len(),
            1,
            "the reopened row is back in the open feed"
        );
    }

    #[tokio::test]
    async fn mark_answered_on_missing_row_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let n = AttentionRepo::mark_answered_if_open(store.pool(), "nope", "tui", "x", 1)
            .await
            .unwrap();
        assert_eq!(n, 0, "answering a non-existent row flips nothing");
    }

    #[tokio::test]
    async fn list_open_none_scopes_to_no_workspace_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_workspace(store.pool(), "ws-a").await;

        // One workspace row, one host (no-workspace) row.
        AttentionRepo::insert(store.pool(), &ask("a1", "s1", Some("ws-a"), 1000))
            .await
            .unwrap();
        AttentionRepo::insert(store.pool(), &ask("h1", "s2", None, 2000)).await.unwrap();

        // Some(ws) → only the workspace row.
        let a = AttentionRepo::list_open(store.pool(), Some("ws-a")).await.unwrap();
        assert_eq!(a.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["a1"]);
        assert_eq!(a[0].workspace_id.as_deref(), Some("ws-a"));

        // None → only the no-workspace (host) row.
        let host = AttentionRepo::list_open(store.pool(), None).await.unwrap();
        assert_eq!(host.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["h1"]);
        assert!(host[0].workspace_id.is_none());
    }

    #[tokio::test]
    async fn list_fleet_spans_every_workspace_and_host() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_workspace(store.pool(), "ws-a").await;
        seed_workspace(store.pool(), "ws-b").await;

        AttentionRepo::insert(store.pool(), &ask("a1", "s1", Some("ws-a"), 1000))
            .await
            .unwrap();
        AttentionRepo::insert(store.pool(), &ask("b1", "s2", Some("ws-b"), 2000))
            .await
            .unwrap();
        AttentionRepo::insert(store.pool(), &ask("h1", "s3", None, 3000)).await.unwrap();

        // Fleet-wide sees all three, oldest first, regardless of workspace.
        let fleet = AttentionRepo::list_fleet(store.pool()).await.unwrap();
        assert_eq!(
            fleet.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["a1", "b1", "h1"],
            "fleet list is host-wide, oldest first"
        );

        // The per-workspace list stays isolated to its own tenant.
        let a = AttentionRepo::list_open(store.pool(), Some("ws-a")).await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].id, "a1");

        // Answering one workspace's row does not remove another's from the fleet.
        AttentionRepo::mark_answered_if_open(store.pool(), "a1", "tui", "ok", 9000)
            .await
            .unwrap();
        let fleet_after = AttentionRepo::list_fleet(store.pool()).await.unwrap();
        assert_eq!(
            fleet_after.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["b1", "h1"],
            "only the answered row leaves the fleet feed"
        );
    }

    #[tokio::test]
    async fn degraded_flag_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let mut row = ask("d1", "sess", None, 1000);
        row.degraded = true;
        AttentionRepo::insert(store.pool(), &row).await.unwrap();
        let got = AttentionRepo::get(store.pool(), "d1").await.unwrap().unwrap();
        assert!(got.degraded, "the pane-fallback degraded flag round-trips");
    }
}
