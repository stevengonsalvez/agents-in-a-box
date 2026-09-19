//! One-time idempotent boot import of `~/.agents-in-a-box/sessions.json` into
//! the daemon-owned `sessions` table (spec P6d, #1166).

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_store::repo::sessions::{SessionRow, SessionsRepo};
use anyhow::Result;
use sqlx::SqlitePool;
use std::path::Path;

/// One-time idempotent import from `sessions.json` into the `sessions` table.
///
/// Leaves `sessions.json` in place. Skips records that are already present
/// in the database (by session_id or tmux_session_name), so repeated boots
/// import nothing further. Returns the count of newly imported sessions.
pub async fn import_sessions_if_needed(pool: &SqlitePool, sessions_path: &Path) -> Result<usize> {
    if !sessions_path.exists() {
        return Ok(0);
    }
    let content = match std::fs::read_to_string(sessions_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %sessions_path.display(), error = %e, "could not read sessions.json");
            return Ok(0);
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(path = %sessions_path.display(), error = %e, "could not parse sessions.json");
            return Ok(0);
        }
    };
    let sessions_map = match value.get("sessions").and_then(|s| s.as_object()) {
        Some(m) => m,
        None => return Ok(0),
    };

    let mut imported = 0;
    for (tmux_key, entry) in sessions_map {
        let tmux_session_name = entry
            .get("tmux_session_name")
            .and_then(|v| v.as_str())
            .unwrap_or(tmux_key)
            .to_string();
        let session_id = entry
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| SystemIdGen.new_ulid());

        let existing_tmux = SessionsRepo::get_by_tmux_name(pool, &tmux_session_name).await?;
        let existing_id = SessionsRepo::get_by_id(pool, &session_id).await?;
        if existing_tmux.is_some() || existing_id.is_some() {
            continue;
        }

        let worktree_path =
            entry.get("worktree_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let workspace_name = entry
            .get("workspace_name")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();
        let created_at = match entry.get("created_at") {
            Some(serde_json::Value::Number(n)) => {
                n.as_i64().unwrap_or_else(|| SystemClock.now_ms())
            }
            Some(serde_json::Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.timestamp_millis())
                .unwrap_or_else(|_| SystemClock.now_ms()),
            _ => SystemClock.now_ms(),
        };
        let agent_type =
            entry.get("agent_type").and_then(|v| v.as_str()).unwrap_or("Claude").to_string();
        let headroom_enabled =
            entry.get("headroom_enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        let rtk_enabled = entry.get("rtk_enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        let skip_permissions = entry.get("skip_permissions").and_then(|v| v.as_bool());
        let model = entry.get("model").and_then(|v| v.as_str()).map(|s| s.to_string());
        let model_source = entry
            .get("model_source")
            .and_then(|v| v.as_str())
            .unwrap_or("LegacyTyped")
            .to_string();
        let codex_model = entry.get("codex_model").and_then(|v| v.as_str()).map(|s| s.to_string());
        let codex_thread_id =
            entry.get("codex_thread_id").and_then(|v| v.as_str()).map(|s| s.to_string());

        let row = SessionRow {
            session_id,
            tmux_session_name,
            worktree_path,
            workspace_name,
            created_at,
            agent_type,
            headroom_enabled,
            rtk_enabled,
            skip_permissions,
            model,
            model_source,
            codex_model,
            codex_thread_id,
        };

        SessionsRepo::upsert(pool, &row).await?;
        imported += 1;
    }

    Ok(imported)
}
