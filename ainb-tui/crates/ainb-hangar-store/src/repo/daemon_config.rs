//! Typed repository over the `daemon_config` table (migration 0028) — the
//! daemon's global key/value config.
//!
//! Host-wide daemon knobs that belong to no workspace live here as string
//! key/value pairs. At P9 the only consumer is auto-standup's global toggle +
//! thresholds (D13), but the table is deliberately generic so a future global
//! setting reuses it rather than growing a bespoke column.
//!
//! **Missing key = coded default.** [`DaemonConfigRepo::get`] returns `None` for
//! an unset key; every caller supplies its own default so an upgrading daemon
//! behaves identically until a value is explicitly written. Values are stored as
//! opaque strings; typed accessors ([`DaemonConfigRepo::get_bool`] /
//! [`DaemonConfigRepo::get_i64`]) parse on read and fall back to the default on a
//! malformed value (never a hard error — a corrupt config knob must not down the
//! watcher).

use sqlx::SqlitePool;

/// Stateless typed wrapper over the `daemon_config` table.
pub struct DaemonConfigRepo;

impl DaemonConfigRepo {
    /// Read one config value, or `None` when the key is unset.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn get(pool: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar::<_, String>("SELECT value FROM daemon_config WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await
    }

    /// Upsert one config value (last write wins).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails.
    pub async fn set(pool: &SqlitePool, key: &str, value: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO daemon_config (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Read a boolean config value, falling back to `default` when the key is
    /// unset OR its stored value is not a recognised boolean token
    /// (`1`/`0`/`true`/`false`, case-insensitive). A malformed value never errors
    /// — the coded default holds so a corrupt knob cannot wedge the daemon.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] only if the underlying query fails.
    pub async fn get_bool(
        pool: &SqlitePool,
        key: &str,
        default: bool,
    ) -> Result<bool, sqlx::Error> {
        Ok(Self::get(pool, key).await?.and_then(|v| parse_bool(&v)).unwrap_or(default))
    }

    /// Read an integer config value, falling back to `default` when the key is
    /// unset OR its stored value does not parse as an `i64`. Never errors on a
    /// malformed value — the coded default holds.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] only if the underlying query fails.
    pub async fn get_i64(pool: &SqlitePool, key: &str, default: i64) -> Result<i64, sqlx::Error> {
        Ok(Self::get(pool, key)
            .await?
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(default))
    }
}

/// Parse a stored boolean token, tolerating the common spellings; `None` for an
/// unrecognised value so the caller's default holds.
fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    #[tokio::test]
    async fn missing_key_returns_none_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        assert!(DaemonConfigRepo::get(store.pool(), "nope").await.unwrap().is_none());
        // Typed accessors fall back to the coded default for an unset key.
        assert!(DaemonConfigRepo::get_bool(store.pool(), "nope", true).await.unwrap());
        assert_eq!(DaemonConfigRepo::get_i64(store.pool(), "nope", 15).await.unwrap(), 15);
    }

    #[tokio::test]
    async fn set_then_get_roundtrips_and_upserts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        DaemonConfigRepo::set(store.pool(), "autostandup.enabled", "false").await.unwrap();
        assert_eq!(
            DaemonConfigRepo::get(store.pool(), "autostandup.enabled").await.unwrap().as_deref(),
            Some("false")
        );
        // A second write to the same key overwrites (upsert, not a duplicate-row error).
        DaemonConfigRepo::set(store.pool(), "autostandup.enabled", "true").await.unwrap();
        assert!(DaemonConfigRepo::get_bool(store.pool(), "autostandup.enabled", false).await.unwrap());
    }

    #[tokio::test]
    async fn typed_accessors_parse_and_tolerate_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        DaemonConfigRepo::set(store.pool(), "b", "ON").await.unwrap();
        assert!(DaemonConfigRepo::get_bool(store.pool(), "b", false).await.unwrap());
        DaemonConfigRepo::set(store.pool(), "n", "42").await.unwrap();
        assert_eq!(DaemonConfigRepo::get_i64(store.pool(), "n", 0).await.unwrap(), 42);
        // A malformed value falls back to the default rather than erroring.
        DaemonConfigRepo::set(store.pool(), "bad", "not-a-number").await.unwrap();
        assert_eq!(DaemonConfigRepo::get_i64(store.pool(), "bad", 7).await.unwrap(), 7);
        DaemonConfigRepo::set(store.pool(), "badb", "maybe").await.unwrap();
        assert!(DaemonConfigRepo::get_bool(store.pool(), "badb", true).await.unwrap());
    }
}
