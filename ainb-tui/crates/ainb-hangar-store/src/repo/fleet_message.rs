//! Persisted chat-bus messages plus their per-recipient delivery receipts.
//!
//! `fleet_message.seq` is the ONE cursor for the message stream: `SQLite`
//! assigns it inside the write transaction and serialises writers, so seq
//! order IS commit order and a page-to-head forwarder cannot skip a row.
//! `id` (a daemon-minted ULID) is the stable external identity used by the
//! wire and by threading; it is NEVER a cursor. Every list reader here pages
//! by `seq`; callers resolve a wire `after_id` through [`FleetMessageRepo::seq_for_id`]
//! first.
//!
//! Insert idempotency mirrors the fleet action/start receipt contract: a
//! reused `request_id` whose stored `request_fingerprint` matches returns the
//! original row; a reused `request_id` with a DIFFERENT fingerprint is
//! rejected, never silently absorbed.
//!
//! Delivery legs use the receipt-claim pattern: a resolver first CLAIMS the
//! `PENDING` row by stamping its fingerprint (single winner, because `SQLite`
//! serialises writers), then resolves it to exactly one terminal state under
//! that same fingerprint.

use sqlx::{Row, SqlitePool};

/// One chat message to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFleetMessage {
    /// Daemon-minted ULID, the stable external identity.
    pub id: String,
    /// Client idempotency token; `None` for daemon-authored rows.
    pub request_id: Option<String>,
    /// Stable hash of the request content; replay with a different value is rejected.
    pub request_fingerprint: Option<String>,
    /// Minted scope string, for example `session:<key>` or `broadcast:<ulid>`.
    pub scope_key: String,
    /// Replies only: the message id this row answers (the thread join).
    pub origin_message_id: Option<String>,
    /// `operator` or a `session_key`.
    pub sender: String,
    /// `user`, `agent`, or `marker` (schema-checked).
    pub kind: String,
    /// Message body.
    pub body: String,
    /// Creation time in epoch milliseconds.
    pub created_at: i64,
}

/// Persisted chat message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetMessageRow {
    /// Commit-ordered cursor assigned by `SQLite`.
    pub seq: i64,
    /// Stable external identity.
    pub id: String,
    /// Client idempotency token when client-authored.
    pub request_id: Option<String>,
    /// Stored replay fingerprint.
    pub request_fingerprint: Option<String>,
    /// Scope the message belongs to.
    pub scope_key: String,
    /// Thread join to the prompting message, replies only.
    pub origin_message_id: Option<String>,
    /// `operator` or a `session_key`.
    pub sender: String,
    /// Message kind.
    pub kind: String,
    /// Message body.
    pub body: String,
    /// Creation time in epoch milliseconds.
    pub created_at: i64,
}

/// Persisted delivery receipt for one (message, recipient) leg.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetMessageDeliveryRow {
    /// Message this leg delivers.
    pub message_id: String,
    /// Recipient session.
    pub session_key: String,
    /// `PENDING`, `DELIVERED`, `FAILED`, `UNKNOWN`, or `REJECTED`.
    pub state: String,
    /// Receipt-claim fingerprint once a resolver has claimed the leg.
    pub fingerprint: Option<String>,
    /// Enumerated outcome detail, including the resume-path fingerprint.
    pub detail: Option<String>,
    /// Terminal-resolution time in epoch milliseconds.
    pub resolved_at: Option<i64>,
}

/// Chat-bus persistence failures.
#[derive(Debug, thiserror::Error)]
pub enum FleetMessageError {
    /// A reused `request_id` carried a different request fingerprint.
    #[error("request_id {request_id:?} was reused for a different message")]
    RequestFingerprintMismatch {
        /// The reused idempotency token.
        request_id: String,
    },
    /// The requested message is absent.
    #[error("fleet message {id:?} was not found")]
    MessageNotFound {
        /// Missing message identity.
        id: String,
    },
    /// `SQLite` failed.
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
}

const MESSAGE_COLUMNS: &str = "seq, id, request_id, request_fingerprint, scope_key, \
     origin_message_id, sender, kind, body, created_at";

const DELIVERY_COLUMNS: &str = "message_id, session_key, state, fingerprint, detail, resolved_at";

/// Typed access to `fleet_message` and `fleet_message_delivery`.
pub struct FleetMessageRepo;

impl FleetMessageRepo {
    /// Insert one daemon-authored message inside a CALLER'S transaction.
    ///
    /// The turn-end write set (agent reply, delivery receipt, cleared open
    /// turn) commits together or not at all, and this is its message half; see
    /// [`super::fleet_acp_session::FleetAcpSessionRepo::commit_turn_end`].
    /// There is no `request_id` replay path here on purpose: a daemon-authored
    /// reply carries no idempotency token, so there is nothing to absorb.
    pub(crate) async fn insert_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        message: &NewFleetMessage,
    ) -> Result<FleetMessageRow, sqlx::Error> {
        sqlx::query(
            "INSERT INTO fleet_message \
             (id, request_id, request_fingerprint, scope_key, origin_message_id, sender, kind, body, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&message.id)
        .bind(&message.request_id)
        .bind(&message.request_fingerprint)
        .bind(&message.scope_key)
        .bind(&message.origin_message_id)
        .bind(&message.sender)
        .bind(&message.kind)
        .bind(&message.body)
        .bind(message.created_at)
        .execute(&mut **tx)
        .await?;
        sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM fleet_message WHERE id = ?"
        ))
        .bind(&message.id)
        .fetch_optional(&mut **tx)
        .await?
        .as_ref()
        .map(message_from)
        .transpose()?
        .ok_or(sqlx::Error::RowNotFound)
    }

    /// Claim AND resolve one `PENDING` leg in a CALLER'S transaction.
    ///
    /// One statement, not the two-step claim [`Self::claim_delivery`] +
    /// [`Self::resolve_delivery`] pair the cross-process resolvers use: inside
    /// one transaction the intermediate claimed-but-unresolved state is never
    /// observable, so it buys nothing. The single winner is decided by exactly
    /// the same predicate (`state = 'PENDING' AND fingerprint IS NULL`), so a
    /// leg convergence already owns is refused here as well.
    pub(crate) async fn resolve_pending_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        message_id: &str,
        session_key: &str,
        fingerprint: &str,
        state: &str,
        detail: Option<&str>,
        resolved_at: i64,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE fleet_message_delivery \
             SET state = ?, detail = ?, resolved_at = ?, fingerprint = ? \
             WHERE message_id = ? AND session_key = ? AND state = 'PENDING' \
               AND fingerprint IS NULL",
        )
        .bind(state)
        .bind(detail)
        .bind(resolved_at)
        .bind(fingerprint)
        .bind(message_id)
        .bind(session_key)
        .execute(&mut **tx)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Insert one message carrying no delivery legs (daemon-authored rows: the
    /// agent replies and markers that answer a prompt rather than address one).
    pub async fn insert_message(
        pool: &SqlitePool,
        message: &NewFleetMessage,
    ) -> Result<FleetMessageRow, FleetMessageError> {
        Self::insert_message_with_deliveries(pool, message, &[]).await
    }

    /// Insert one message AND its `PENDING` delivery legs in ONE transaction.
    ///
    /// A replayed `request_id` with a matching `request_fingerprint` returns
    /// the original row; a mismatched replay is rejected (the fleet
    /// action/start receipt contract).
    ///
    /// The legs share the message's transaction deliberately: a replay that
    /// arrives while the first call is still running must observe a COMPLETE
    /// leg set. Writing the legs afterwards leaves a window where the replay
    /// sees a message with no legs and can only answer UNKNOWN for targets that
    /// are about to resolve DELIVERED, which is exactly the case an idempotency
    /// token exists to cover (request timeout, then retry).
    pub async fn insert_message_with_deliveries(
        pool: &SqlitePool,
        message: &NewFleetMessage,
        targets: &[String],
    ) -> Result<FleetMessageRow, FleetMessageError> {
        let mut tx = pool.begin().await?;
        let result = sqlx::query(
            "INSERT INTO fleet_message \
             (id, request_id, request_fingerprint, scope_key, origin_message_id, sender, kind, body, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(request_id) DO NOTHING",
        )
        .bind(&message.id)
        .bind(&message.request_id)
        .bind(&message.request_fingerprint)
        .bind(&message.scope_key)
        .bind(&message.origin_message_id)
        .bind(&message.sender)
        .bind(&message.kind)
        .bind(&message.body)
        .bind(message.created_at)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() > 0 {
            for session_key in targets {
                sqlx::query(
                    "INSERT INTO fleet_message_delivery (message_id, session_key, state) \
                     VALUES (?, ?, 'PENDING')",
                )
                .bind(&message.id)
                .bind(session_key)
                .execute(&mut *tx)
                .await?;
            }
            let row = sqlx::query(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM fleet_message WHERE id = ?"
            ))
            .bind(&message.id)
            .fetch_optional(&mut *tx)
            .await?
            .as_ref()
            .map(message_from)
            .transpose()?
            .ok_or_else(|| FleetMessageError::MessageNotFound {
                id: message.id.clone(),
            })?;
            tx.commit().await?;
            return Ok(row);
        }
        // ON CONFLICT(request_id) only fires for a non-NULL token, so the
        // replayed request_id is present by construction here.
        let request_id = message.request_id.clone().unwrap_or_default();
        let existing = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM fleet_message WHERE request_id = ?"
        ))
        .bind(&request_id)
        .fetch_optional(&mut *tx)
        .await?
        .as_ref()
        .map(message_from)
        .transpose()?
        .ok_or_else(|| FleetMessageError::MessageNotFound {
            id: message.id.clone(),
        })?;
        tx.rollback().await?;
        if existing.request_fingerprint != message.request_fingerprint {
            return Err(FleetMessageError::RequestFingerprintMismatch { request_id });
        }
        Ok(existing)
    }

    /// Fetch one message by its stable id.
    pub async fn get_message(
        pool: &SqlitePool,
        id: &str,
    ) -> Result<Option<FleetMessageRow>, sqlx::Error> {
        sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM fleet_message WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .as_ref()
        .map(message_from)
        .transpose()
    }

    /// Resolve a wire `after_id` to its `seq` cursor. Ids are identities, never
    /// cursors; this is the one sanctioned crossing between the two.
    pub async fn seq_for_id(pool: &SqlitePool, id: &str) -> Result<Option<i64>, sqlx::Error> {
        sqlx::query_scalar("SELECT seq FROM fleet_message WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
    }

    /// The newest committed row, or `None` on an empty log: the head a
    /// subscriber acknowledges and starts its cursor from.
    pub async fn head(pool: &SqlitePool) -> Result<Option<FleetMessageRow>, sqlx::Error> {
        sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM fleet_message ORDER BY seq DESC LIMIT 1"
        ))
        .fetch_optional(pool)
        .await?
        .as_ref()
        .map(message_from)
        .transpose()
    }

    /// Page one scope's messages after a `seq` cursor, oldest first.
    pub async fn list_by_scope(
        pool: &SqlitePool,
        scope_key: &str,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<FleetMessageRow>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM fleet_message \
             WHERE scope_key = ? AND seq > ? ORDER BY seq ASC LIMIT ?"
        ))
        .bind(scope_key)
        .bind(after_seq)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        rows.iter().map(message_from).collect()
    }

    /// Page every message after a `seq` cursor, oldest first (the digest view
    /// and the subscribe forwarder's page-to-head read).
    pub async fn list_all(
        pool: &SqlitePool,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<FleetMessageRow>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM fleet_message \
             WHERE seq > ? ORDER BY seq ASC LIMIT ?"
        ))
        .bind(after_seq)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        rows.iter().map(message_from).collect()
    }

    /// Page the replies threaded to one origin message, oldest first (the
    /// broadcast thread join).
    pub async fn list_by_origin(
        pool: &SqlitePool,
        origin_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<FleetMessageRow>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM fleet_message \
             WHERE origin_message_id = ? AND seq > ? ORDER BY seq ASC LIMIT ?"
        ))
        .bind(origin_id)
        .bind(after_seq)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        rows.iter().map(message_from).collect()
    }

    /// The resume re-prime corpus: the LAST `limit` messages BELOW `before_seq`
    /// that a session either received (a `fleet_message_delivery` row exists for
    /// it, broadcasts included) or sent (`sender` = `session_key`), returned
    /// oldest first.
    ///
    /// This is the delivery JOIN, deliberately not a raw scope filter, so a
    /// broadcast-delivered prompt is never lost from a rebuilt context.
    ///
    /// `before_seq` is the in-flight prompt's `seq`, and it is what makes the
    /// corpus HISTORY. A delivery row exists from the moment a message is
    /// queued, so an unbounded window renders the prompt about to be asked, and
    /// every message still sitting behind it in the queue, under a header that
    /// says all of it already happened. Under a burst deeper than the row limit
    /// the in-flight prompt is not even in the window.
    pub async fn list_for_session(
        pool: &SqlitePool,
        session_key: &str,
        before_seq: i64,
        limit: i64,
    ) -> Result<Vec<FleetMessageRow>, sqlx::Error> {
        // UNION of two indexed halves (idx_fleet_message_sender and
        // idx_fleet_message_delivery_session); newest window first, then
        // reversed so the caller renders in seq order.
        let rows = sqlx::query(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM fleet_message WHERE sender = ? AND seq < ? \
             UNION \
             SELECT m.seq, m.id, m.request_id, m.request_fingerprint, m.scope_key, \
                    m.origin_message_id, m.sender, m.kind, m.body, m.created_at \
             FROM fleet_message m \
             JOIN fleet_message_delivery d ON d.message_id = m.id \
             WHERE d.session_key = ? AND m.seq < ? \
             ORDER BY seq DESC LIMIT ?"
        ))
        .bind(session_key)
        .bind(before_seq)
        .bind(session_key)
        .bind(before_seq)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        let mut messages: Vec<FleetMessageRow> =
            rows.iter().map(message_from).collect::<Result<_, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    /// Insert one `PENDING` delivery leg for a recipient.
    pub async fn insert_delivery(
        pool: &SqlitePool,
        message_id: &str,
        session_key: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO fleet_message_delivery (message_id, session_key, state) \
             VALUES (?, ?, 'PENDING')",
        )
        .bind(message_id)
        .bind(session_key)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Claim one `PENDING` leg by stamping a resolver fingerprint. Returns
    /// whether THIS caller won the claim; concurrent claimers get exactly one
    /// winner because `SQLite` serialises writers.
    pub async fn claim_delivery(
        pool: &SqlitePool,
        message_id: &str,
        session_key: &str,
        fingerprint: &str,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE fleet_message_delivery SET fingerprint = ? \
             WHERE message_id = ? AND session_key = ? AND state = 'PENDING' \
               AND fingerprint IS NULL",
        )
        .bind(fingerprint)
        .bind(message_id)
        .bind(session_key)
        .execute(pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Resolve a claimed leg to a terminal state. Only the claim-holding
    /// fingerprint resolves, and only once: a stale or losing resolver gets
    /// `false`, never a second terminal write.
    pub async fn resolve_delivery(
        pool: &SqlitePool,
        message_id: &str,
        session_key: &str,
        fingerprint: &str,
        state: &str,
        detail: Option<&str>,
        resolved_at: i64,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE fleet_message_delivery SET state = ?, detail = ?, resolved_at = ? \
             WHERE message_id = ? AND session_key = ? AND fingerprint = ? \
               AND state = 'PENDING'",
        )
        .bind(state)
        .bind(detail)
        .bind(resolved_at)
        .bind(message_id)
        .bind(session_key)
        .bind(fingerprint)
        .execute(pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// List every delivery leg of one message (the per-recipient receipt view).
    pub async fn deliveries_for_message(
        pool: &SqlitePool,
        message_id: &str,
    ) -> Result<Vec<FleetMessageDeliveryRow>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {DELIVERY_COLUMNS} FROM fleet_message_delivery \
             WHERE message_id = ? ORDER BY session_key ASC"
        ))
        .bind(message_id)
        .fetch_all(pool)
        .await?;
        rows.iter().map(delivery_from).collect()
    }

    /// List one session's still-`PENDING` legs (the boot and runtime
    /// convergence input).
    pub async fn pending_deliveries_for_session(
        pool: &SqlitePool,
        session_key: &str,
    ) -> Result<Vec<FleetMessageDeliveryRow>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {DELIVERY_COLUMNS} FROM fleet_message_delivery \
             WHERE session_key = ? AND state = 'PENDING' ORDER BY message_id ASC"
        ))
        .bind(session_key)
        .fetch_all(pool)
        .await?;
        rows.iter().map(delivery_from).collect()
    }
}

fn message_from(row: &sqlx::sqlite::SqliteRow) -> Result<FleetMessageRow, sqlx::Error> {
    Ok(FleetMessageRow {
        seq: row.try_get("seq")?,
        id: row.try_get("id")?,
        request_id: row.try_get("request_id")?,
        request_fingerprint: row.try_get("request_fingerprint")?,
        scope_key: row.try_get("scope_key")?,
        origin_message_id: row.try_get("origin_message_id")?,
        sender: row.try_get("sender")?,
        kind: row.try_get("kind")?,
        body: row.try_get("body")?,
        created_at: row.try_get("created_at")?,
    })
}

fn delivery_from(row: &sqlx::sqlite::SqliteRow) -> Result<FleetMessageDeliveryRow, sqlx::Error> {
    Ok(FleetMessageDeliveryRow {
        message_id: row.try_get("message_id")?,
        session_key: row.try_get("session_key")?,
        state: row.try_get("state")?,
        fingerprint: row.try_get("fingerprint")?,
        detail: row.try_get("detail")?,
        resolved_at: row.try_get("resolved_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn message(id: &str, scope: &str) -> NewFleetMessage {
        NewFleetMessage {
            id: id.to_string(),
            request_id: None,
            request_fingerprint: None,
            scope_key: scope.to_string(),
            origin_message_id: None,
            sender: "operator".to_string(),
            kind: "user".to_string(),
            body: format!("body of {id}"),
            created_at: 100,
        }
    }

    async fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn request_id_replay_with_matching_fingerprint_returns_original_row() {
        let (_dir, store) = store().await;
        let mut input = message("msg-1", "session:s-1");
        input.request_id = Some("req-1".to_string());
        input.request_fingerprint = Some("fp-1".to_string());
        let first = FleetMessageRepo::insert_message(store.pool(), &input).await.unwrap();

        let mut replay = input.clone();
        replay.id = "msg-2".to_string(); // a retry mints a fresh ULID
        let second = FleetMessageRepo::insert_message(store.pool(), &replay).await.unwrap();

        assert_eq!(first, second, "replay returns the ORIGINAL row");
        assert_eq!(second.id, "msg-1");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_message")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(count, 1, "no second row on replay");
    }

    #[tokio::test]
    async fn request_id_replay_with_differing_fingerprint_is_rejected() {
        let (_dir, store) = store().await;
        let mut input = message("msg-1", "session:s-1");
        input.request_id = Some("req-1".to_string());
        input.request_fingerprint = Some("fp-1".to_string());
        FleetMessageRepo::insert_message(store.pool(), &input).await.unwrap();

        let mut forged = input.clone();
        forged.id = "msg-2".to_string();
        forged.request_fingerprint = Some("fp-DIFFERENT".to_string());
        forged.body = "different body".to_string();

        assert!(matches!(
            FleetMessageRepo::insert_message(store.pool(), &forged).await,
            Err(FleetMessageError::RequestFingerprintMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn message_and_its_legs_land_in_one_transaction() {
        let (_dir, store) = store().await;
        let mut input = message("msg-1", "broadcast:b-1");
        input.request_id = Some("req-1".to_string());
        input.request_fingerprint = Some("fp-1".to_string());
        let targets = vec!["sess-1".to_string(), "sess-2".to_string()];

        let first =
            FleetMessageRepo::insert_message_with_deliveries(store.pool(), &input, &targets)
                .await
                .unwrap();
        let legs = FleetMessageRepo::deliveries_for_message(store.pool(), &first.id).await.unwrap();
        assert_eq!(
            legs.len(),
            2,
            "the legs commit WITH the message, never after"
        );

        // A replay observes the complete leg set and adds nothing.
        let mut replay = input.clone();
        replay.id = "msg-2".to_string();
        let second =
            FleetMessageRepo::insert_message_with_deliveries(store.pool(), &replay, &targets)
                .await
                .unwrap();
        assert_eq!(first, second);
        assert_eq!(
            FleetMessageRepo::deliveries_for_message(store.pool(), &first.id)
                .await
                .unwrap()
                .len(),
            2,
            "a replay never duplicates a leg"
        );
    }

    #[tokio::test]
    async fn daemon_authored_rows_carry_no_request_id_and_never_collide() {
        let (_dir, store) = store().await;
        FleetMessageRepo::insert_message(store.pool(), &message("msg-1", "session:s-1"))
            .await
            .unwrap();
        // A second NULL request_id must not be treated as a replay of the first.
        let second =
            FleetMessageRepo::insert_message(store.pool(), &message("msg-2", "session:s-1"))
                .await
                .unwrap();
        assert_eq!(second.id, "msg-2");
    }

    #[tokio::test]
    async fn lists_page_by_seq_and_scope() {
        let (_dir, store) = store().await;
        for (id, scope) in [
            ("msg-1", "session:s-1"),
            ("msg-2", "session:s-2"),
            ("msg-3", "session:s-1"),
        ] {
            FleetMessageRepo::insert_message(store.pool(), &message(id, scope))
                .await
                .unwrap();
        }

        let all = FleetMessageRepo::list_all(store.pool(), 0, 10).await.unwrap();
        assert_eq!(
            all.iter().map(|m| m.seq).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "seq is assigned in commit order"
        );

        let scoped = FleetMessageRepo::list_by_scope(store.pool(), "session:s-1", 0, 10)
            .await
            .unwrap();
        assert_eq!(
            scoped.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["msg-1", "msg-3"]
        );

        let after = FleetMessageRepo::list_by_scope(store.pool(), "session:s-1", 1, 10)
            .await
            .unwrap();
        assert_eq!(after.len(), 1, "the cursor pages by seq, not by id");
        assert_eq!(after[0].id, "msg-3");

        assert_eq!(
            FleetMessageRepo::seq_for_id(store.pool(), "msg-3").await.unwrap(),
            Some(3)
        );
        assert_eq!(
            FleetMessageRepo::seq_for_id(store.pool(), "missing").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn list_by_origin_returns_exactly_the_threaded_replies() {
        let (_dir, store) = store().await;
        FleetMessageRepo::insert_message(store.pool(), &message("bcast-1", "broadcast:b-1"))
            .await
            .unwrap();
        FleetMessageRepo::insert_message(store.pool(), &message("noise", "session:s-9"))
            .await
            .unwrap();
        for (id, scope) in [("reply-1", "session:s-1"), ("reply-2", "session:s-2")] {
            let mut reply = message(id, scope);
            reply.origin_message_id = Some("bcast-1".to_string());
            reply.sender = scope.trim_start_matches("session:").to_string();
            reply.kind = "agent".to_string();
            FleetMessageRepo::insert_message(store.pool(), &reply).await.unwrap();
        }

        let thread =
            FleetMessageRepo::list_by_origin(store.pool(), "bcast-1", 0, 10).await.unwrap();
        assert_eq!(
            thread.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["reply-1", "reply-2"],
            "the origin join returns the reply set and nothing else"
        );
    }

    #[tokio::test]
    async fn list_for_session_is_the_delivery_join_not_a_scope_filter() {
        let (_dir, store) = store().await;
        // A broadcast delivered to s-1: no scope match, only a delivery row.
        FleetMessageRepo::insert_message(store.pool(), &message("bcast-1", "broadcast:b-1"))
            .await
            .unwrap();
        FleetMessageRepo::insert_delivery(store.pool(), "bcast-1", "acp:s-1")
            .await
            .unwrap();
        // The session's own reply, joined via sender.
        let mut reply = message("reply-1", "session:acp:s-1");
        reply.sender = "acp:s-1".to_string();
        reply.kind = "agent".to_string();
        FleetMessageRepo::insert_message(store.pool(), &reply).await.unwrap();
        // An unrelated scope with its own delivery to a DIFFERENT session.
        FleetMessageRepo::insert_message(store.pool(), &message("other-1", "session:acp:s-2"))
            .await
            .unwrap();
        FleetMessageRepo::insert_delivery(store.pool(), "other-1", "acp:s-2")
            .await
            .unwrap();

        let corpus = FleetMessageRepo::list_for_session(store.pool(), "acp:s-1", i64::MAX, 20)
            .await
            .unwrap();
        assert_eq!(
            corpus.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["bcast-1", "reply-1"],
            "broadcast-delivered inbound + own outbound, in seq order, nothing else"
        );

        // The window keeps the NEWEST rows when the limit bites.
        let windowed = FleetMessageRepo::list_for_session(store.pool(), "acp:s-1", i64::MAX, 1)
            .await
            .unwrap();
        assert_eq!(windowed.len(), 1);
        assert_eq!(windowed[0].id, "reply-1", "oldest dropped first");
    }

    /// The corpus is HISTORY, and a delivery row exists from the moment a
    /// message is QUEUED. Without the seq bound a rebuild hands the agent the
    /// prompt it is about to be asked, plus everything still queued behind it,
    /// under a header saying all of it already happened.
    #[tokio::test]
    async fn list_for_session_stops_below_the_in_flight_prompt() {
        let (_dir, store) = store().await;
        for id in ["old-1", "in-flight", "queued"] {
            FleetMessageRepo::insert_message(store.pool(), &message(id, "session:acp:s-1"))
                .await
                .unwrap();
            FleetMessageRepo::insert_delivery(store.pool(), id, "acp:s-1").await.unwrap();
        }
        let in_flight = FleetMessageRepo::seq_for_id(store.pool(), "in-flight")
            .await
            .unwrap()
            .expect("the in-flight prompt has a row");

        let corpus = FleetMessageRepo::list_for_session(store.pool(), "acp:s-1", in_flight, 20)
            .await
            .unwrap();
        assert_eq!(
            corpus.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["old-1"],
            "history only: neither the in-flight prompt nor the one queued behind it"
        );
    }

    #[tokio::test]
    async fn delivery_claim_is_single_winner_under_concurrent_claimers() {
        let (_dir, store) = store().await;
        FleetMessageRepo::insert_message(store.pool(), &message("msg-1", "session:s-1"))
            .await
            .unwrap();
        FleetMessageRepo::insert_delivery(store.pool(), "msg-1", "sess-1")
            .await
            .unwrap();

        let mut claims = Vec::new();
        for i in 0..8 {
            let pool = store.pool().clone();
            claims.push(tokio::spawn(async move {
                FleetMessageRepo::claim_delivery(&pool, "msg-1", "sess-1", &format!("fp-{i}"))
                    .await
                    .unwrap()
            }));
        }
        let mut winners = 0;
        for claim in claims {
            if claim.await.unwrap() {
                winners += 1;
            }
        }
        assert_eq!(winners, 1, "exactly one concurrent claimer wins");
    }

    #[tokio::test]
    async fn resolve_requires_the_claim_fingerprint_and_fires_once() {
        let (_dir, store) = store().await;
        FleetMessageRepo::insert_message(store.pool(), &message("msg-1", "session:s-1"))
            .await
            .unwrap();
        FleetMessageRepo::insert_delivery(store.pool(), "msg-1", "sess-1")
            .await
            .unwrap();
        assert!(
            FleetMessageRepo::claim_delivery(store.pool(), "msg-1", "sess-1", "fp-owner")
                .await
                .unwrap()
        );

        // A resolver that never won the claim cannot resolve.
        assert!(
            !FleetMessageRepo::resolve_delivery(
                store.pool(),
                "msg-1",
                "sess-1",
                "fp-loser",
                "FAILED",
                None,
                200,
            )
            .await
            .unwrap()
        );
        // The claim holder resolves exactly once.
        assert!(
            FleetMessageRepo::resolve_delivery(
                store.pool(),
                "msg-1",
                "sess-1",
                "fp-owner",
                "DELIVERED",
                Some("loaded"),
                201,
            )
            .await
            .unwrap()
        );
        assert!(
            !FleetMessageRepo::resolve_delivery(
                store.pool(),
                "msg-1",
                "sess-1",
                "fp-owner",
                "FAILED",
                None,
                202,
            )
            .await
            .unwrap(),
            "a second terminal write is refused"
        );

        let legs = FleetMessageRepo::deliveries_for_message(store.pool(), "msg-1").await.unwrap();
        assert_eq!(legs.len(), 1);
        assert_eq!(legs[0].state, "DELIVERED");
        assert_eq!(legs[0].detail.as_deref(), Some("loaded"));
        assert_eq!(legs[0].resolved_at, Some(201));
        assert!(
            FleetMessageRepo::pending_deliveries_for_session(store.pool(), "sess-1")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn pending_deliveries_scope_to_one_session() {
        let (_dir, store) = store().await;
        FleetMessageRepo::insert_message(store.pool(), &message("msg-1", "broadcast:b-1"))
            .await
            .unwrap();
        for session in ["sess-1", "sess-2"] {
            FleetMessageRepo::insert_delivery(store.pool(), "msg-1", session).await.unwrap();
        }

        let pending = FleetMessageRepo::pending_deliveries_for_session(store.pool(), "sess-1")
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].session_key, "sess-1");
    }
}
