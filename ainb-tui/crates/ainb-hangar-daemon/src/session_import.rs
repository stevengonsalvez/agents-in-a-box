//! One-time boot import of `~/.agents-in-a-box/sessions.json` into the
//! daemon-owned `sessions` table (spec P6d, #1166).
//!
//! ```text
//! boot ──▶ marker for this path? ──yes──▶ AlreadyCompleted (nothing read)
//!                │ no
//!                ▼
//!          file missing ──▶ marker, 0 rows (fresh home)
//!          over cap / unreadable / unparseable ──▶ Err, NO marker
//!          parsed ──▶ validate each record ──▶ rows + marker, one tx
//! ```
//!
//! The file is only ever read, never written, so a user who downgrades keeps
//! every session it held. The marker (migration 0102) is what makes the import
//! one-time: a row deleted after the import is not brought back by the next
//! boot. Until a marker exists, `workspace/session_list` answers
//! `import_complete: false` and the CLI keeps reading the file, so a failed
//! import can never make a populated file look empty.
//!
//! # Dark in P6d, and what P6e must reconcile
//!
//! P6d ships the table dark: no daemon advertises the capability, so every
//! reader and writer stays on the file, and the table is written only by this
//! import and by the daemon's own interactive registration (as a shadow of
//! its file write). Sessions created after the import therefore exist in the
//! file but not the table. Before P6e flips the capability it needs a second
//! reconciliation: file rows missing from the table, keyed by session id,
//! that never resurrects a row the table deleted after the flip. That is
//! P6e's first open question; this marker alone does not answer it.

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_proto::sessions::WorkspaceSessionEntry;
use ainb_hangar_store::repo::sessions::{ImportOutcome, SessionRow, SessionsRepo};
use anyhow::{Context, Result, bail};
use sqlx::SqlitePool;
use std::path::Path;

pub use ainb_hangar_store::repo::sessions::ImportMarker;

/// Largest `sessions.json` the import reads. A real store is a few KB per
/// session; anything past this is not a session store.
pub const SESSIONS_JSON_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// What one boot's import did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportReport {
    /// This boot imported and wrote the marker.
    Completed(ImportMarker),
    /// An earlier boot finished the import of this file; nothing was read.
    AlreadyCompleted,
}

/// Import `sessions_path` once, capped at [`SESSIONS_JSON_MAX_BYTES`].
///
/// # Errors
///
/// Returns an error, and writes no marker, when the file is over the cap,
/// unreadable or unparseable, or the store write fails. The caller logs it;
/// clients see `import_complete: false` and keep reading the file.
pub async fn import_sessions_if_needed(
    pool: &SqlitePool,
    sessions_path: &Path,
) -> Result<ImportReport> {
    import_sessions_from(pool, sessions_path, SESSIONS_JSON_MAX_BYTES).await
}

/// [`import_sessions_if_needed`] with an explicit size cap (the test seam).
///
/// # Errors
///
/// As [`import_sessions_if_needed`].
pub async fn import_sessions_from(
    pool: &SqlitePool,
    sessions_path: &Path,
    max_bytes: u64,
) -> Result<ImportReport> {
    let source = sessions_path.to_string_lossy().into_owned();
    if SessionsRepo::import_marker(pool, &source).await?.is_some() {
        return Ok(ImportReport::AlreadyCompleted);
    }

    let path = sessions_path.to_path_buf();
    let content = tokio::task::spawn_blocking(move || read_capped(&path, max_bytes))
        .await
        .context("sessions.json read task")??;

    let (rows, rejected) = match content {
        None => (Vec::new(), 0),
        Some(content) => parse_records(&content)
            .with_context(|| format!("could not parse {}", sessions_path.display()))?,
    };

    let outcome =
        SessionsRepo::complete_import(pool, &source, &rows, rejected, SystemClock.now_ms()).await?;
    Ok(match outcome {
        ImportOutcome::Completed(marker) => ImportReport::Completed(marker),
        ImportOutcome::AlreadyCompleted => ImportReport::AlreadyCompleted,
    })
}

/// Read `path` if it exists and is at most `max_bytes`. `Ok(None)` means no
/// file (a fresh home).
fn read_capped(path: &Path, max_bytes: u64) -> Result<Option<String>> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("could not stat {}", path.display())),
    };
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    if meta.len() > max_bytes {
        bail!(
            "{} is {} bytes, over the {max_bytes} byte import limit",
            path.display(),
            meta.len()
        );
    }
    std::fs::read_to_string(path)
        .map(Some)
        .with_context(|| format!("could not read {}", path.display()))
}

/// Parse the store into valid rows plus a count of rejected records.
fn parse_records(content: &str) -> Result<(Vec<SessionRow>, i64)> {
    let value: serde_json::Value = serde_json::from_str(content).context("parse sessions.json")?;
    let Some(sessions) = value.get("sessions").and_then(serde_json::Value::as_object) else {
        bail!("parse sessions.json: no `sessions` object");
    };

    let mut rows = Vec::with_capacity(sessions.len());
    let mut rejected = 0_i64;
    for (tmux_key, record) in sessions {
        match record_to_entry(tmux_key, record).and_then(|entry| {
            entry.validate()?;
            Ok(entry)
        }) {
            Ok(entry) => rows.push(entry_to_row(entry)),
            Err(why) => {
                rejected += 1;
                tracing::warn!(
                    tmux_key = %tmux_key.escape_debug(),
                    %why,
                    "sessions.json record not imported; it stays in the file"
                );
            }
        }
    }
    Ok((rows, rejected))
}

/// Map one file record onto the wire entry.
///
/// A record with no `session_id` gets a fresh UUID, minted once here and then
/// stored, so every later read sees the same id. A record whose `session_id`
/// is present keeps it verbatim; if it is not a UUID, validation rejects the
/// record rather than inventing a replacement.
fn record_to_entry(
    tmux_key: &str,
    record: &serde_json::Value,
) -> Result<WorkspaceSessionEntry, String> {
    let text = |key: &str| record.get(key).and_then(serde_json::Value::as_str);
    let flag = |key: &str| record.get(key).and_then(serde_json::Value::as_bool);

    let session_id = match record.get("session_id") {
        None | Some(serde_json::Value::Null) => uuid::Uuid::new_v4().to_string(),
        Some(serde_json::Value::String(id)) => id.clone(),
        Some(_) => return Err("session_id is not a string".to_string()),
    };
    let created_at = match record.get("created_at") {
        Some(serde_json::Value::Number(n)) => {
            n.as_i64().ok_or_else(|| "created_at is not an integer".to_string())?
        }
        Some(serde_json::Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.timestamp_millis())
            .map_err(|e| format!("created_at: {e}"))?,
        _ => return Err("created_at is missing".to_string()),
    };

    Ok(WorkspaceSessionEntry {
        session_id,
        tmux_session_name: text("tmux_session_name").unwrap_or(tmux_key).to_string(),
        worktree_path: text("worktree_path").unwrap_or_default().to_string(),
        workspace_name: text("workspace_name").unwrap_or("default").to_string(),
        created_at,
        agent_type: text("agent_type").unwrap_or("Claude").to_string(),
        headroom_enabled: flag("headroom_enabled").unwrap_or(false),
        rtk_enabled: flag("rtk_enabled").unwrap_or(false),
        skip_permissions: flag("skip_permissions"),
        model: text("model").map(str::to_string),
        model_source: text("model_source").unwrap_or("LegacyTyped").to_string(),
        codex_model: text("codex_model").map(str::to_string),
        codex_thread_id: text("codex_thread_id").map(str::to_string),
    })
}

fn entry_to_row(entry: WorkspaceSessionEntry) -> SessionRow {
    SessionRow {
        session_id: entry.session_id,
        tmux_session_name: entry.tmux_session_name,
        worktree_path: entry.worktree_path,
        workspace_name: entry.workspace_name,
        created_at: entry.created_at,
        agent_type: entry.agent_type,
        headroom_enabled: entry.headroom_enabled,
        rtk_enabled: entry.rtk_enabled,
        skip_permissions: entry.skip_permissions,
        model: entry.model,
        model_source: entry.model_source,
        codex_model: entry.codex_model,
        codex_thread_id: entry.codex_thread_id,
    }
}
