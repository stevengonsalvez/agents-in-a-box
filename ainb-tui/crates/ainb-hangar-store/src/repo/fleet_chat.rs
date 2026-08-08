//! Channels, guardrail confirm cards and the copilot activity feed
//! (migration 0083, buzz-port part 2).
//!
//! Three small tables under one roof because they are one concern: what the
//! copilot channel is, what it asked a human for, and what it did.
//!
//! Two rules carry across from the chat bus and are the reason this module
//! exists rather than raw queries at the call sites:
//!
//! * **`seq` is the cursor, `id` is the identity.** `fleet_activity` pages by
//!   the commit-ordered `seq` `SQLite` assigns inside the write transaction,
//!   never by a ULID and never by a wall clock.
//! * **A confirm card is single-use.** Every terminal write is guarded by
//!   `WHERE state = 'open'`, so exactly one of (answer, second answer, expiry)
//!   wins and the losers see zero rows affected. That is what makes "answering
//!   an already-answered card is a typed error, never a second execution" a
//!   property of the store instead of a race at the RPC layer.

use sqlx::{Row, SqlitePool};

// ------------------------------------------------------------------ channels

/// One channel and the scope it minted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetChannelRow {
    /// Daemon-minted ULID.
    pub id: String,
    /// `copilot` or `broadcast`.
    pub kind: String,
    /// Human-readable name.
    pub name: String,
    /// The minted scope, always `channel:<id>`.
    pub scope_key: String,
    /// Member session keys.
    pub recipients: Vec<String>,
    /// Creation time in epoch milliseconds.
    pub created_at: i64,
}

/// Typed access to `fleet_channel` and `fleet_channel_member`.
pub struct FleetChannelRepo;

impl FleetChannelRepo {
    /// Insert one channel AND its membership in ONE transaction.
    ///
    /// Together, so a concurrent `fleet/message_send` on the minted scope can
    /// never observe a channel whose member set is still being written: an
    /// empty membership is a scope that refuses every target, which would turn
    /// a race into a refused send rather than a queued one.
    pub async fn insert(
        pool: &SqlitePool,
        channel: &FleetChannelRow,
    ) -> Result<FleetChannelRow, sqlx::Error> {
        let mut tx = pool.begin().await?;
        sqlx::query(
            "INSERT INTO fleet_channel (id, kind, name, scope_key, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&channel.id)
        .bind(&channel.kind)
        .bind(&channel.name)
        .bind(&channel.scope_key)
        .bind(channel.created_at)
        .execute(&mut *tx)
        .await?;
        for session_key in &channel.recipients {
            sqlx::query(
                "INSERT OR IGNORE INTO fleet_channel_member (channel_id, session_key) \
                 VALUES (?, ?)",
            )
            .bind(&channel.id)
            .bind(session_key)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(channel.clone())
    }

    /// Every channel in creation order, each with its membership.
    pub async fn list(pool: &SqlitePool) -> Result<Vec<FleetChannelRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, kind, name, scope_key, created_at FROM fleet_channel \
             ORDER BY created_at ASC, id ASC",
        )
        .fetch_all(pool)
        .await?;
        let mut channels = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: String = row.try_get("id")?;
            let recipients = Self::members(pool, &id).await?;
            channels.push(FleetChannelRow {
                kind: row.try_get("kind")?,
                name: row.try_get("name")?,
                scope_key: row.try_get("scope_key")?,
                created_at: row.try_get("created_at")?,
                recipients,
                id,
            });
        }
        Ok(channels)
    }

    /// One channel by the scope it minted, or `None` when the scope names no
    /// channel. The channel-scope rule in `fleet/message_send` starts here.
    pub async fn by_scope(
        pool: &SqlitePool,
        scope_key: &str,
    ) -> Result<Option<FleetChannelRow>, sqlx::Error> {
        let Some(row) = sqlx::query(
            "SELECT id, kind, name, scope_key, created_at FROM fleet_channel WHERE scope_key = ?",
        )
        .bind(scope_key)
        .fetch_optional(pool)
        .await?
        else {
            return Ok(None);
        };
        let id: String = row.try_get("id")?;
        let recipients = Self::members(pool, &id).await?;
        Ok(Some(FleetChannelRow {
            kind: row.try_get("kind")?,
            name: row.try_get("name")?,
            scope_key: row.try_get("scope_key")?,
            created_at: row.try_get("created_at")?,
            recipients,
            id,
        }))
    }

    /// The newest channel of `kind`, the copilot singleton lookup.
    pub async fn newest_of_kind(
        pool: &SqlitePool,
        kind: &str,
    ) -> Result<Option<FleetChannelRow>, sqlx::Error> {
        let scope: Option<String> = sqlx::query_scalar(
            "SELECT scope_key FROM fleet_channel WHERE kind = ? \
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(kind)
        .fetch_optional(pool)
        .await?;
        match scope {
            Some(scope) => Self::by_scope(pool, &scope).await,
            None => Ok(None),
        }
    }

    async fn members(pool: &SqlitePool, channel_id: &str) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT session_key FROM fleet_channel_member WHERE channel_id = ? \
             ORDER BY session_key ASC",
        )
        .bind(channel_id)
        .fetch_all(pool)
        .await
    }
}

// ------------------------------------------------------------- confirm cards

/// One guardrail confirm card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetConfirmRow {
    /// Daemon-minted ULID.
    pub confirm_id: String,
    /// Scope the card belongs to.
    pub scope_key: String,
    /// The MCP tool the copilot asked to run.
    pub tool: String,
    /// Arguments, ALREADY projected to the tool's declared schema keys.
    pub arguments: String,
    /// The session the tool would act on.
    pub target_session_key: Option<String>,
    /// `open`, `approved`, `denied`, or `expired`.
    pub state: String,
    /// Operator-edited arguments, for an `edit` answer only.
    pub edited_arguments: Option<String>,
    /// Creation time in epoch milliseconds.
    pub created_at: i64,
    /// Server-side expiry in epoch milliseconds.
    pub expires_at: i64,
    /// Answer time in epoch milliseconds.
    pub answered_at: Option<i64>,
}

const CONFIRM_COLUMNS: &str = "confirm_id, scope_key, tool, arguments, target_session_key, \
     state, edited_arguments, created_at, expires_at, answered_at";

/// Typed access to `fleet_confirm`.
pub struct FleetConfirmRepo;

impl FleetConfirmRepo {
    /// Mint one OPEN card.
    pub async fn insert(pool: &SqlitePool, card: &FleetConfirmRow) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO fleet_confirm \
             (confirm_id, scope_key, tool, arguments, target_session_key, state, \
              edited_arguments, created_at, expires_at, answered_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&card.confirm_id)
        .bind(&card.scope_key)
        .bind(&card.tool)
        .bind(&card.arguments)
        .bind(&card.target_session_key)
        .bind(&card.state)
        .bind(&card.edited_arguments)
        .bind(card.created_at)
        .bind(card.expires_at)
        .bind(card.answered_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// One card by id, whatever its state.
    pub async fn get(
        pool: &SqlitePool,
        confirm_id: &str,
    ) -> Result<Option<FleetConfirmRow>, sqlx::Error> {
        sqlx::query(&format!(
            "SELECT {CONFIRM_COLUMNS} FROM fleet_confirm WHERE confirm_id = ?"
        ))
        .bind(confirm_id)
        .fetch_optional(pool)
        .await?
        .as_ref()
        .map(confirm_from)
        .transpose()
    }

    /// The OPEN, UNLAPSED cards at `now`, oldest first, optionally one scope.
    ///
    /// The expiry term is not decoration: the TTL that parks a card is a
    /// `tokio` timer inside the copilot's turn, and that timer dies with the
    /// process. Without `expires_at > now` here, a card whose daemon was
    /// restarted stays `open` in the table and keeps rendering as answerable
    /// days later, on every client.
    pub async fn list_open(
        pool: &SqlitePool,
        scope_key: Option<&str>,
        now: i64,
    ) -> Result<Vec<FleetConfirmRow>, sqlx::Error> {
        let rows = match scope_key {
            Some(scope) => {
                sqlx::query(&format!(
                    "SELECT {CONFIRM_COLUMNS} FROM fleet_confirm \
                     WHERE state = 'open' AND expires_at > ? AND scope_key = ? \
                     ORDER BY created_at ASC, confirm_id ASC"
                ))
                .bind(now)
                .bind(scope)
                .fetch_all(pool)
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {CONFIRM_COLUMNS} FROM fleet_confirm \
                     WHERE state = 'open' AND expires_at > ? \
                     ORDER BY created_at ASC, confirm_id ASC"
                ))
                .bind(now)
                .fetch_all(pool)
                .await?
            }
        };
        rows.iter().map(confirm_from).collect()
    }

    /// Answer an OPEN, UNLAPSED card, returning the resolved row.
    ///
    /// `None` means this caller LOST: the card was already answered, already
    /// expired, its TTL has lapsed, or it never existed. Single-use is enforced
    /// here, in the `WHERE state = 'open'` guard, because `SQLite` serialises
    /// writers and so exactly one of two concurrent answers can match.
    ///
    /// The `expires_at > ?` term makes the TTL a property of the DATA rather
    /// than of a live process: an answer arriving after the lifetime lapsed is
    /// refused rather than returning a success receipt for a destructive tool
    /// call that has no waiter left to run it.
    pub async fn resolve(
        pool: &SqlitePool,
        confirm_id: &str,
        state: &str,
        edited_arguments: Option<&str>,
        answered_at: i64,
    ) -> Result<Option<FleetConfirmRow>, sqlx::Error> {
        let affected = sqlx::query(
            "UPDATE fleet_confirm SET state = ?, edited_arguments = ?, answered_at = ? \
             WHERE confirm_id = ? AND state = 'open' AND expires_at > ?",
        )
        .bind(state)
        .bind(edited_arguments)
        .bind(answered_at)
        .bind(confirm_id)
        .bind(answered_at)
        .execute(pool)
        .await?
        .rows_affected();
        if affected == 0 {
            return Ok(None);
        }
        Self::get(pool, confirm_id).await
    }

    /// Resolve one open card as EXPIRED, whatever its `expires_at` says.
    ///
    /// Deliberately not guarded by the expiry term [`Self::resolve`] carries:
    /// this is the transition INTO the lapsed state, and it is also how the
    /// in-turn timer records an expiry the moment it fires.
    pub async fn expire(
        pool: &SqlitePool,
        confirm_id: &str,
        at: i64,
    ) -> Result<Option<FleetConfirmRow>, sqlx::Error> {
        let affected = sqlx::query(
            "UPDATE fleet_confirm SET state = 'expired', answered_at = ? \
             WHERE confirm_id = ? AND state = 'open'",
        )
        .bind(at)
        .bind(confirm_id)
        .execute(pool)
        .await?
        .rows_affected();
        if affected == 0 {
            return Ok(None);
        }
        Self::get(pool, confirm_id).await
    }

    /// Expire every card whose lifetime lapsed while nothing was watching.
    ///
    /// The daemon runs this once at boot, which is what makes the TTL survive a
    /// crash, an upgrade or a restart: the waiters map and its timers are
    /// process state, the rows are not. This is the "expiry sweep" migration
    /// 0083's `idx_fleet_confirm_open` was indexed for.
    pub async fn sweep_expired(pool: &SqlitePool, now: i64) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "UPDATE fleet_confirm SET state = 'expired', answered_at = ? \
             WHERE state = 'open' AND expires_at <= ?",
        )
        .bind(now)
        .bind(now)
        .execute(pool)
        .await?
        .rows_affected())
    }

    /// Count the open cards and the age of the oldest, for `hangar/daemon_health`.
    ///
    /// "Why is the copilot stuck" is answered by part 1's pool fields plus this
    /// pair: a card nobody answered holds a turn, and a turn holds its queue.
    pub async fn open_stats(pool: &SqlitePool) -> Result<(i64, Option<i64>), sqlx::Error> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS open_count, MIN(created_at) AS oldest \
             FROM fleet_confirm WHERE state = 'open'",
        )
        .fetch_one(pool)
        .await?;
        Ok((row.try_get("open_count")?, row.try_get("oldest")?))
    }
}

fn confirm_from(row: &sqlx::sqlite::SqliteRow) -> Result<FleetConfirmRow, sqlx::Error> {
    Ok(FleetConfirmRow {
        confirm_id: row.try_get("confirm_id")?,
        scope_key: row.try_get("scope_key")?,
        tool: row.try_get("tool")?,
        arguments: row.try_get("arguments")?,
        target_session_key: row.try_get("target_session_key")?,
        state: row.try_get("state")?,
        edited_arguments: row.try_get("edited_arguments")?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
        answered_at: row.try_get("answered_at")?,
    })
}

// ----------------------------------------------------------- activity feed

/// One append-only copilot activity row to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFleetActivity {
    /// Daemon-minted ULID.
    pub id: String,
    /// Scope the action was taken in.
    pub scope_key: String,
    /// The MCP tool invoked.
    pub tool: String,
    /// `read`, `write`, or `destructive`.
    pub class: String,
    /// The session acted on, when the tool named one.
    pub target_session_key: Option<String>,
    /// `ok`, `denied`, `expired`, or `error`.
    pub outcome: String,
    /// Short daemon-authored detail; never the model's justification.
    pub detail: Option<String>,
    /// Creation time in epoch milliseconds.
    pub created_at: i64,
}

/// One persisted activity row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetActivityRowRecord {
    /// Commit-ordered cursor.
    pub seq: i64,
    /// Stable external identity.
    pub id: String,
    /// Scope the action was taken in.
    pub scope_key: String,
    /// The MCP tool invoked.
    pub tool: String,
    /// Guardrail class.
    pub class: String,
    /// The session acted on.
    pub target_session_key: Option<String>,
    /// Outcome.
    pub outcome: String,
    /// Short detail.
    pub detail: Option<String>,
    /// Creation time in epoch milliseconds.
    pub created_at: i64,
}

const ACTIVITY_COLUMNS: &str =
    "seq, id, scope_key, tool, class, target_session_key, outcome, detail, created_at";

/// Typed access to `fleet_activity`.
pub struct FleetActivityRepo;

impl FleetActivityRepo {
    /// Append one row and return it with the `seq` `SQLite` just assigned.
    pub async fn insert(
        pool: &SqlitePool,
        activity: &NewFleetActivity,
    ) -> Result<FleetActivityRowRecord, sqlx::Error> {
        sqlx::query(
            "INSERT INTO fleet_activity \
             (id, scope_key, tool, class, target_session_key, outcome, detail, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&activity.id)
        .bind(&activity.scope_key)
        .bind(&activity.tool)
        .bind(&activity.class)
        .bind(&activity.target_session_key)
        .bind(&activity.outcome)
        .bind(&activity.detail)
        .bind(activity.created_at)
        .execute(pool)
        .await?;
        let row = sqlx::query(&format!(
            "SELECT {ACTIVITY_COLUMNS} FROM fleet_activity WHERE id = ?"
        ))
        .bind(&activity.id)
        .fetch_one(pool)
        .await?;
        activity_from(&row)
    }

    /// Page rows after a `seq` cursor, oldest first, optionally one scope.
    pub async fn list(
        pool: &SqlitePool,
        scope_key: Option<&str>,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<FleetActivityRowRecord>, sqlx::Error> {
        let rows = match scope_key {
            Some(scope) => {
                sqlx::query(&format!(
                    "SELECT {ACTIVITY_COLUMNS} FROM fleet_activity \
                     WHERE scope_key = ? AND seq > ? ORDER BY seq ASC LIMIT ?"
                ))
                .bind(scope)
                .bind(after_seq)
                .bind(limit.max(0))
                .fetch_all(pool)
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {ACTIVITY_COLUMNS} FROM fleet_activity \
                     WHERE seq > ? ORDER BY seq ASC LIMIT ?"
                ))
                .bind(after_seq)
                .bind(limit.max(0))
                .fetch_all(pool)
                .await?
            }
        };
        rows.iter().map(activity_from).collect()
    }
}

fn activity_from(row: &sqlx::sqlite::SqliteRow) -> Result<FleetActivityRowRecord, sqlx::Error> {
    Ok(FleetActivityRowRecord {
        seq: row.try_get("seq")?,
        id: row.try_get("id")?,
        scope_key: row.try_get("scope_key")?,
        tool: row.try_get("tool")?,
        class: row.try_get("class")?,
        target_session_key: row.try_get("target_session_key")?,
        outcome: row.try_get("outcome")?,
        detail: row.try_get("detail")?,
        created_at: row.try_get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn card(confirm_id: &str, expires_at: i64) -> FleetConfirmRow {
        FleetConfirmRow {
            confirm_id: confirm_id.to_string(),
            scope_key: "channel:01J0CHANNEL".to_string(),
            tool: "kill".to_string(),
            arguments: r#"{"session":"claude:one"}"#.to_string(),
            target_session_key: Some("claude:one".to_string()),
            state: "open".to_string(),
            edited_arguments: None,
            created_at: 1_000,
            expires_at,
            answered_at: None,
        }
    }

    /// The TTL is a property of the ROW, so it survives the process that minted
    /// it. A daemon restart drops the waiters map and its timers; without this,
    /// the card it left behind stays answerable forever and an approve returns
    /// a success receipt for a destructive call nothing will ever run.
    #[tokio::test]
    async fn a_lapsed_card_is_neither_listed_nor_answerable_and_the_sweep_closes_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        FleetConfirmRepo::insert(pool, &card("LAPSED", 5_000)).await.unwrap();
        FleetConfirmRepo::insert(pool, &card("LIVE", 50_000)).await.unwrap();

        let now = 10_000;
        let open = FleetConfirmRepo::list_open(pool, None, now).await.unwrap();
        assert_eq!(
            open.iter().map(|row| row.confirm_id.as_str()).collect::<Vec<_>>(),
            vec!["LIVE"],
            "a card past its expiry is still being offered to an operator"
        );

        assert!(
            FleetConfirmRepo::resolve(pool, "LAPSED", "approved", None, now)
                .await
                .unwrap()
                .is_none(),
            "a lapsed card accepted an answer, which is a false receipt on a security control"
        );
        assert!(
            FleetConfirmRepo::resolve(pool, "LIVE", "approved", None, now)
                .await
                .unwrap()
                .is_some(),
            "a live card refused a legitimate answer"
        );

        // The boot sweep is what makes the lapse durable rather than a filter
        // every future reader has to remember.
        FleetConfirmRepo::insert(pool, &card("ALSO-LAPSED", 6_000)).await.unwrap();
        assert_eq!(FleetConfirmRepo::sweep_expired(pool, now).await.unwrap(), 2);
        for confirm_id in ["LAPSED", "ALSO-LAPSED"] {
            let row = FleetConfirmRepo::get(pool, confirm_id).await.unwrap().expect("card");
            assert_eq!(row.state, "expired", "{confirm_id} was not swept");
        }
        assert_eq!(
            FleetConfirmRepo::sweep_expired(pool, now).await.unwrap(),
            0,
            "the sweep is not idempotent"
        );
    }
}
