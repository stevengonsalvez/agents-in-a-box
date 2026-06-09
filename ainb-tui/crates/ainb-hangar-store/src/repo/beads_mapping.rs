//! Typed repository wrapper over the `beads_mapping` table (P2.1).
//!
//! `beads_mapping` correlates a Hangar entity (an `issue` or `task`) with its
//! mirrored beads (`bd`) id, and records:
//!
//! - `source` — whether the row originated from Hangar (mirror outbound) or
//!   from a swarm leader's own `bd` lifecycle (skip outbound; per
//!   `feedback_swarm_leader_owns_bd_tracking`).
//! - `last_synced` — the wall-clock instant the sync loop last reconciled the
//!   pair. Stored as ISO-8601 UTC TEXT at second precision; the P2 last-writer-
//!   wins logic compares it as a [`DateTime<Utc>`].
//!
//! The repo is a thin, stateless sqlx layer. Each id side is independently
//! UNIQUE in the schema (one hangar id maps to exactly one bd id and vice
//! versa), so [`insert`](BeadsMappingRepo::insert) of a second row reusing
//! either id surfaces a `UniqueViolation`.

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, Row, SqlitePool};

/// Which Hangar / bd entity kind a mapping row correlates.
///
/// Stored as the TEXT tokens `issue` / `task`, matching the schema's
/// `CHECK (… IN ('issue','task'))` constraints on both `hangar_kind` and
/// `bd_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingKind {
    /// The mapping correlates an `issue` row.
    Issue,
    /// The mapping correlates an `agent_task_queue` (task) row.
    Task,
}

impl MappingKind {
    /// The stable TEXT token persisted for this kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Task => "task",
        }
    }

    /// Parse a stored TEXT token back into a [`MappingKind`].
    fn parse(s: &str) -> Result<Self, sqlx::Error> {
        match s {
            "issue" => Ok(Self::Issue),
            "task" => Ok(Self::Task),
            other => Err(decode_err(
                "hangar_kind/bd_kind",
                &format!("unknown kind '{other}'"),
            )),
        }
    }
}

/// Where a mapping row originated, gating the outbound mirror.
///
/// `Hangar`-sourced rows mirror their lifecycle out to `bd`; `Swarm`-sourced
/// rows are owned by a swarm leader's `bd` workflow and are skipped by the
/// outbound sync (per `feedback_swarm_leader_owns_bd_tracking`). Stored as the
/// TEXT tokens `hangar` / `swarm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingSource {
    /// Created by Hangar — eligible for outbound mirroring to `bd`.
    Hangar,
    /// Created by a swarm leader — outbound mirror is skipped.
    Swarm,
}

impl MappingSource {
    /// The stable TEXT token persisted for this source.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hangar => "hangar",
            Self::Swarm => "swarm",
        }
    }

    /// Parse a stored TEXT token back into a [`MappingSource`].
    fn parse(s: &str) -> Result<Self, sqlx::Error> {
        match s {
            "hangar" => Ok(Self::Hangar),
            "swarm" => Ok(Self::Swarm),
            other => Err(decode_err("source", &format!("unknown source '{other}'"))),
        }
    }
}

/// A fully-materialised `beads_mapping` row.
///
/// `last_synced` is a [`DateTime<Utc>`]; it is stored as ISO-8601 TEXT at second
/// precision and so round-trips at second precision only (sub-second components
/// are dropped on write).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeadsMappingRow {
    /// Hangar-side entity id (the PK; `issue.id` or `agent_task_queue.id`).
    pub hangar_id: String,
    /// beads-side entity id.
    pub bd_id: String,
    /// Which kind the Hangar id refers to.
    pub hangar_kind: MappingKind,
    /// Which kind the bd id refers to.
    pub bd_kind: MappingKind,
    /// Origin of the row (gates the outbound mirror).
    pub source: MappingSource,
    /// Wall-clock instant of the last reconcile (UTC, second precision).
    pub last_synced: DateTime<Utc>,
}

/// Stateless typed wrapper over the `beads_mapping` table.
///
/// Holds a borrowed [`SqlitePool`] so callers compose it with the rest of the
/// store (`Store::pool()`).
pub struct BeadsMappingRepo<'a> {
    pool: &'a SqlitePool,
}

impl<'a> BeadsMappingRepo<'a> {
    /// Wrap a borrowed pool.
    #[must_use]
    pub const fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert one mapping row and return it as persisted.
    ///
    /// Returns the inserted row (so callers see the exact `last_synced` that
    /// landed, already normalised to second precision).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] with a `UniqueViolation` database error if a
    /// row reusing either `hangar_id` (PK) or `bd_id` (UNIQUE) already exists,
    /// or any other database error on failure.
    pub async fn insert(&self, mapping: &BeadsMappingRow) -> Result<BeadsMappingRow, sqlx::Error> {
        sqlx::query(
            "INSERT INTO beads_mapping \
             (hangar_id, bd_id, hangar_kind, bd_kind, source, last_synced) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&mapping.hangar_id)
        .bind(&mapping.bd_id)
        .bind(mapping.hangar_kind.as_str())
        .bind(mapping.bd_kind.as_str())
        .bind(mapping.source.as_str())
        .bind(encode_ts(mapping.last_synced))
        .execute(self.pool)
        .await?;

        self.find_by_hangar(&mapping.hangar_id)
            .await?
            .ok_or_else(|| decode_err("hangar_id", "row vanished immediately after insert"))
    }

    /// Fetch the mapping for a Hangar id, or `None` if absent.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a stored row is malformed.
    pub async fn find_by_hangar(
        &self,
        hangar_id: &str,
    ) -> Result<Option<BeadsMappingRow>, sqlx::Error> {
        let row = sqlx::query(SELECT_ALL_WHERE_HANGAR)
            .bind(hangar_id)
            .fetch_optional(self.pool)
            .await?;
        row.as_ref().map(BeadsMappingRow::from_row).transpose()
    }

    /// Fetch the mapping for a bd id, or `None` if absent.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a stored row is malformed.
    pub async fn find_by_bd(&self, bd_id: &str) -> Result<Option<BeadsMappingRow>, sqlx::Error> {
        let row = sqlx::query(SELECT_ALL_WHERE_BD).bind(bd_id).fetch_optional(self.pool).await?;
        row.as_ref().map(BeadsMappingRow::from_row).transpose()
    }

    /// Stamp a new `last_synced` on the row identified by `hangar_id`.
    ///
    /// A no-op (zero rows affected) if no such row exists — callers that need to
    /// distinguish should check the return.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the update fails.
    pub async fn update_last_synced(
        &self,
        hangar_id: &str,
        last_synced: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE beads_mapping SET last_synced = ? WHERE hangar_id = ?")
            .bind(encode_ts(last_synced))
            .bind(hangar_id)
            .execute(self.pool)
            .await?;
        Ok(())
    }

    /// List every mapping with `last_synced >= since`, ordered ascending.
    ///
    /// The reconcile delta query: walks only rows touched at or after the
    /// supplied cutoff. The comparison is inclusive of `since` itself.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a row is malformed.
    pub async fn list_since(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<BeadsMappingRow>, sqlx::Error> {
        let rows = sqlx::query(concat!(
            "SELECT hangar_id, bd_id, hangar_kind, bd_kind, source, last_synced ",
            "FROM beads_mapping WHERE last_synced >= ? ORDER BY last_synced"
        ))
        .bind(encode_ts(since))
        .fetch_all(self.pool)
        .await?;
        rows.iter().map(BeadsMappingRow::from_row).collect()
    }

    /// Delete the mapping row identified by `hangar_id`.
    ///
    /// Idempotent: deleting an absent row is not an error.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the delete fails.
    pub async fn delete_by_hangar(&self, hangar_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM beads_mapping WHERE hangar_id = ?")
            .bind(hangar_id)
            .execute(self.pool)
            .await?;
        Ok(())
    }
}

/// Strongly-typed correlation key, pairing the two id sides so a caller cannot
/// silently swap argument order between a hangar id and a bd id.
///
/// Encodes the P2.1 invariant at compile time per the CLAUDE.md type-design
/// rule. The repo methods take `&str` for the common single-side lookup path;
/// callers composing both ids (the sync adapter) thread a [`MappingKey`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingKey {
    /// The Hangar-side id.
    pub hangar: String,
    /// The beads-side id.
    pub bd: String,
}

impl MappingKey {
    /// Build a key from both sides.
    #[must_use]
    pub fn new(hangar: impl Into<String>, bd: impl Into<String>) -> Self {
        Self {
            hangar: hangar.into(),
            bd: bd.into(),
        }
    }
}

/// `SELECT` projecting all columns, filtered on the hangar id.
const SELECT_ALL_WHERE_HANGAR: &str = "SELECT hangar_id, bd_id, hangar_kind, bd_kind, source, \
     last_synced FROM beads_mapping WHERE hangar_id = ?";

/// `SELECT` projecting all columns, filtered on the bd id.
const SELECT_ALL_WHERE_BD: &str = "SELECT hangar_id, bd_id, hangar_kind, bd_kind, source, \
     last_synced FROM beads_mapping WHERE bd_id = ?";

impl FromRow<'_, SqliteRow> for BeadsMappingRow {
    fn from_row(row: &SqliteRow) -> Result<Self, sqlx::Error> {
        let hangar_kind: String = row.try_get("hangar_kind")?;
        let bd_kind: String = row.try_get("bd_kind")?;
        let source: String = row.try_get("source")?;
        let last_synced: String = row.try_get("last_synced")?;
        Ok(Self {
            hangar_id: row.try_get("hangar_id")?,
            bd_id: row.try_get("bd_id")?,
            hangar_kind: MappingKind::parse(&hangar_kind)?,
            bd_kind: MappingKind::parse(&bd_kind)?,
            source: MappingSource::parse(&source)?,
            last_synced: decode_ts(&last_synced)?,
        })
    }
}

/// Encode a UTC instant as the canonical ISO-8601 second-precision TEXT the
/// column stores (e.g. `2023-11-14T22:13:20Z`).
fn encode_ts(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Decode a stored ISO-8601 TEXT timestamp back into a UTC instant.
fn decode_ts(s: &str) -> Result<DateTime<Utc>, sqlx::Error> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| Utc.from_utc_datetime(&dt.naive_utc()))
        .map_err(|e| decode_err("last_synced", &e.to_string()))
}

/// Build a `sqlx` decode error for a malformed `beads_mapping` column.
fn decode_err(column: &str, detail: &str) -> sqlx::Error {
    sqlx::Error::ColumnDecode {
        index: column.to_string(),
        source: format!("malformed beads_mapping '{column}': {detail}").into(),
    }
}
