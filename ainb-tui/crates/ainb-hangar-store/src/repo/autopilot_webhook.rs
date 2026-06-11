//! Typed repository wrapper over the webhook columns of `autopilot` plus the
//! `autopilot_webhook_delivery` audit table (e38.18).
//!
//! Webhook-triggered autopilots add a third firing surface to the cron + manual
//! pair: an authenticated local HTTP ingress fires an autopilot on a signed
//! `POST /hangar/webhook/<autopilot_id>`. This module owns the *persistence* side
//! of that feature — the per-autopilot webhook configuration (digest of the HMAC
//! secret, the enabled flag, the optional event filter) and the delivery log the
//! `ainb hangar autopilot deliveries <id>` verb reads.
//!
//! # Secret-at-rest discipline (mirrors e38.1)
//!
//! The `webhook_secret_sha256` column stores only the SHA-256 **digest** of the
//! HMAC signing secret, never the secret itself — exactly the
//! `daemon_socket_token` model (digest in the database, recoverable plaintext
//! only in a 0600 file). The recoverable plaintext the HTTP ingress needs to
//! recompute the body HMAC lives in [`WebhookSecretStore`]'s 0600
//! `webhook_secrets.json`. An attacker who reads `hangar.db` learns the digest,
//! which cannot forge a signature.
//!
//! # Workspace scoping
//!
//! Every mutation/read that takes an autopilot id is scoped
//! `WHERE id = ? AND workspace_id = ?` (or joins through `autopilot` for the
//! delivery reads), so a caller holding an [`AutopilotId`] from workspace A can
//! never configure, read the secret digest of, or list the deliveries of
//! workspace B's autopilot — the same tenant-isolation contract `AutopilotRepo`
//! established.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
use sqlx::SqlitePool;

/// The per-autopilot webhook configuration (the three `autopilot` columns).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebhookConfig {
    /// Whether the HTTP ingress accepts a POST for this autopilot at all. A row
    /// with `enabled = false` (or no secret digest) rejects every webhook
    /// request, so a half-configured autopilot is never firable over HTTP.
    pub enabled: bool,
    /// SHA-256 hex digest of the HMAC signing secret; `None` when no secret is
    /// set. The recoverable plaintext lives in [`WebhookSecretStore`].
    pub secret_sha256: Option<String>,
    /// Optional exact-match event filter; `None` fires on every signed request.
    pub event_filter: Option<String>,
}

/// One processed webhook request (an `autopilot_webhook_delivery` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDelivery {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning autopilot (`autopilot.id`).
    pub autopilot_id: String,
    /// When the request was processed (epoch-ms).
    pub received_at: i64,
    /// `fired` / `filtered` / `rejected`.
    pub outcome: String,
    /// The event name seen on the request (`None` when absent).
    pub event: Option<String>,
    /// The HTTP status the ingress returned.
    pub http_status: i64,
    /// The `autopilot_run` this delivery fired (`None` unless `outcome = fired`).
    pub run_id: Option<String>,
    /// A short human detail (e.g. the rejection reason); never the secret/body.
    pub detail: Option<String>,
}

/// The inputs to [`AutopilotWebhookRepo::record_delivery`].
#[derive(Debug, Clone)]
pub struct NewDelivery {
    /// Owning autopilot.
    pub autopilot_id: AutopilotId,
    /// When the request was processed (epoch-ms).
    pub received_at: i64,
    /// `fired` / `filtered` / `rejected`.
    pub outcome: DeliveryOutcome,
    /// The event name seen on the request.
    pub event: Option<String>,
    /// The HTTP status the ingress returned.
    pub http_status: u16,
    /// The fired run id (only when `outcome == Fired`).
    pub run_id: Option<String>,
    /// A short human detail; never the secret/body.
    pub detail: Option<String>,
}

/// The three delivery outcomes the `outcome` CHECK constrains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The request was signed + passed the filter; the autopilot fired.
    Fired,
    /// The request was signed but the event filter rejected it (nothing fired).
    Filtered,
    /// The request was rejected (bad/absent signature, disabled, unknown id).
    Rejected,
}

impl DeliveryOutcome {
    /// The literal stored in the `outcome` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fired => "fired",
            Self::Filtered => "filtered",
            Self::Rejected => "rejected",
        }
    }
}

/// Stateless typed wrapper over the autopilot webhook columns + delivery table.
pub struct AutopilotWebhookRepo;

impl AutopilotWebhookRepo {
    /// Read one autopilot's webhook configuration, scoped to `workspace`.
    ///
    /// Returns `None` when the id resolves to no autopilot in this tenant (a
    /// foreign id), so the HTTP ingress cannot leak the existence of another
    /// workspace's autopilot.
    ///
    /// # Errors
    ///
    /// Returns [`sqlx::Error`] on a SQL failure.
    pub async fn get_config(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        id: &AutopilotId,
    ) -> Result<Option<WebhookConfig>, sqlx::Error> {
        let row: Option<(i64, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT webhook_enabled, webhook_secret_sha256, webhook_event_filter \
             FROM autopilot WHERE id = ? AND workspace_id = ?",
        )
        .bind(id.as_str())
        .bind(workspace.as_str())
        .fetch_optional(pool)
        .await?;
        Ok(
            row.map(|(enabled, secret_sha256, event_filter)| WebhookConfig {
                enabled: enabled != 0,
                secret_sha256,
                event_filter,
            }),
        )
    }

    /// Resolve a full autopilot row **plus** its webhook config by id alone — the
    /// shape the HTTP ingress needs, since the webhook URL carries only the
    /// autopilot id (a ULID, not workspace-qualified). The autopilot's owning
    /// workspace is intrinsic to the row, so this is the deliberate one place
    /// without a workspace argument: the id itself is the unguessable credential,
    /// and the per-autopilot HMAC secret is what actually authorises a fire.
    ///
    /// Returns `None` for an unknown id (the ingress answers 404 and fires
    /// nothing).
    ///
    /// # Errors
    ///
    /// Returns [`sqlx::Error`] on a SQL failure.
    pub async fn get_for_webhook(
        pool: &SqlitePool,
        id: &AutopilotId,
    ) -> Result<Option<(super::autopilot::Autopilot, WebhookConfig)>, sqlx::Error> {
        let autopilot =
            super::autopilot::AutopilotRepo::get_by_id(pool, id)
                .await
                .map_err(|e| match e {
                    super::autopilot::AutopilotRepoError::Db(db) => db,
                    other => sqlx::Error::Protocol(other.to_string()),
                })?;
        let Some(autopilot) = autopilot else {
            return Ok(None);
        };
        let config = WebhookConfig {
            enabled: false,
            secret_sha256: None,
            event_filter: None,
        };
        // Re-read the webhook columns scoped to the row's own workspace — the
        // single source of truth for the config flags.
        let ws = WorkspaceId::from_str(autopilot.workspace_id.clone())
            .map_err(|_| sqlx::Error::RowNotFound)?;
        let config = Self::get_config(pool, &ws, id).await?.unwrap_or(config);
        Ok(Some((autopilot, config)))
    }

    /// Set (or clear) the webhook configuration for an autopilot, scoped to
    /// `workspace`. A foreign id touches no row.
    ///
    /// The `secret_sha256` digest is written as given (`None` clears the secret);
    /// callers persist the recoverable plaintext separately via
    /// [`WebhookSecretStore`].
    ///
    /// # Errors
    ///
    /// Returns [`sqlx::Error`] on a SQL failure.
    pub async fn set_config(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        id: &AutopilotId,
        config: &WebhookConfig,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE autopilot \
             SET webhook_enabled = ?, webhook_secret_sha256 = ?, webhook_event_filter = ? \
             WHERE id = ? AND workspace_id = ?",
        )
        .bind(i64::from(config.enabled))
        .bind(&config.secret_sha256)
        .bind(&config.event_filter)
        .bind(id.as_str())
        .bind(workspace.as_str())
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Record one processed webhook request in the delivery audit log.
    ///
    /// Not workspace-scoped on its own (the `autopilot_id` is resolved+scoped by
    /// the caller before a delivery is recorded), but the read-back
    /// [`list_deliveries`](Self::list_deliveries) joins through `autopilot` to
    /// enforce the tenant boundary.
    ///
    /// # Errors
    ///
    /// Returns [`sqlx::Error`] on a SQL failure (e.g. an `autopilot_id` FK
    /// violation).
    pub async fn record_delivery(
        pool: &SqlitePool,
        delivery: &NewDelivery,
    ) -> Result<String, sqlx::Error> {
        let id = SystemIdGen.new_ulid();
        sqlx::query(
            "INSERT INTO autopilot_webhook_delivery \
             (id, autopilot_id, received_at, outcome, event, http_status, run_id, detail) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(delivery.autopilot_id.as_str())
        .bind(delivery.received_at)
        .bind(delivery.outcome.as_str())
        .bind(&delivery.event)
        .bind(i64::from(delivery.http_status))
        .bind(&delivery.run_id)
        .bind(&delivery.detail)
        .execute(pool)
        .await?;
        Ok(id)
    }

    /// List an autopilot's deliveries, latest-first, capped at `limit`. Scoped to
    /// `workspace` by joining through `autopilot`: a foreign autopilot id yields
    /// an empty set rather than leaking another tenant's delivery history.
    ///
    /// # Errors
    ///
    /// Returns [`sqlx::Error`] on a SQL failure.
    pub async fn list_deliveries(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        autopilot_id: &AutopilotId,
        limit: u32,
    ) -> Result<Vec<WebhookDelivery>, sqlx::Error> {
        let rows = sqlx::query_as::<_, WebhookDelivery>(
            "SELECT d.id, d.autopilot_id, d.received_at, d.outcome, d.event, \
                    d.http_status, d.run_id, d.detail \
             FROM autopilot_webhook_delivery d \
             JOIN autopilot a ON a.id = d.autopilot_id \
             WHERE d.autopilot_id = ? AND a.workspace_id = ? \
             ORDER BY d.received_at DESC \
             LIMIT ?",
        )
        .bind(autopilot_id.as_str())
        .bind(workspace.as_str())
        .bind(i64::from(limit))
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }
}

impl sqlx::FromRow<'_, sqlx::sqlite::SqliteRow> for WebhookDelivery {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            autopilot_id: row.try_get("autopilot_id")?,
            received_at: row.try_get("received_at")?,
            outcome: row.try_get("outcome")?,
            event: row.try_get("event")?,
            http_status: row.try_get("http_status")?,
            run_id: row.try_get("run_id")?,
            detail: row.try_get("detail")?,
        })
    }
}

/// The 0600 file mapping autopilot id → recoverable HMAC secret plaintext.
///
/// The database stores only the secret's SHA-256 digest; the HTTP ingress needs
/// the *recoverable* plaintext to recompute the body HMAC at verify time. That
/// plaintext lives here, in `{hangar_home}/hangar/webhook_secrets.json`, created
/// with `0600` permissions so only the daemon's uid can read it — the same
/// discipline `daemon.token` (e38.1) uses, generalised to N secrets.
///
/// The file is a flat JSON object `{ "<autopilot_id>": "<secret>" }`. A missing
/// file is an empty map (no secrets set yet). Writes are atomic-enough for a
/// single daemon: the whole map is rewritten on each set/clear.
pub struct WebhookSecretStore {
    path: PathBuf,
}

impl WebhookSecretStore {
    /// Resolve the secret file under `hangar_home`:
    /// `{hangar_home}/hangar/webhook_secrets.json`.
    #[must_use]
    pub fn in_home(hangar_home: &Path) -> Self {
        Self {
            path: hangar_home.join("hangar").join("webhook_secrets.json"),
        }
    }

    /// The resolved secret-file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the recoverable secret for `autopilot_id`, or `None` when unset.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] on a read failure or a corrupt (non-JSON) file.
    pub fn get(&self, autopilot_id: &str) -> io::Result<Option<String>> {
        Ok(self.load()?.remove(autopilot_id))
    }

    /// Set (overwrite) the recoverable secret for `autopilot_id`, rewriting the
    /// 0600 file.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] on a read/write failure.
    pub fn set(&self, autopilot_id: &str, secret: &str) -> io::Result<()> {
        let mut map = self.load()?;
        map.insert(autopilot_id.to_string(), secret.to_string());
        self.store(&map)
    }

    /// Remove the recoverable secret for `autopilot_id` (a no-op when absent).
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] on a read/write failure.
    pub fn remove(&self, autopilot_id: &str) -> io::Result<()> {
        let mut map = self.load()?;
        if map.remove(autopilot_id).is_some() {
            self.store(&map)?;
        }
        Ok(())
    }

    /// Load the secret map (missing file → empty map).
    fn load(&self) -> io::Result<BTreeMap<String, String>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e),
        }
    }

    /// Rewrite the secret map to the 0600 file (created fresh so the mode is
    /// applied at create time and never widened by a pre-existing file).
    fn store(&self, map: &BTreeMap<String, String>) -> io::Result<()> {
        use std::io::Write as _;

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(map)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let _ = std::fs::remove_file(&self.path);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&self.path)?;
        f.write_all(&json)?;
        Ok(())
    }
}
