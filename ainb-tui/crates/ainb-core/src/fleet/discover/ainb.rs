// ABOUTME: Discover sessions by shelling out to `ainb list --format json`.

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::fleet::types::{AinbSession, Session, SessionSource};

/// Override the binary used (test seam).
fn ainb_bin() -> String {
    std::env::var("AINB_BIN").unwrap_or_else(|_| "ainb".to_string())
}

pub async fn list_ainb_sessions() -> Result<Vec<AinbSession>> {
    let output = Command::new(ainb_bin())
        .args(["list", "--format", "json"])
        .output()
        .await
        .context("failed to invoke `ainb list --format json`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ainb list exited non-zero: {stderr}");
    }

    let parsed: Vec<AinbSession> = serde_json::from_slice(&output.stdout)
        .context("`ainb list` produced invalid JSON")?;
    Ok(parsed)
}

/// Map ainb rows to [`Session`] records. Filters to running sessions.
pub async fn discover_from_ainb() -> Result<Vec<Session>> {
    let rows = list_ainb_sessions().await?;
    Ok(rows
        .into_iter()
        .filter(|r| r.is_running)
        .map(|r| Session {
            id: r.session_id,
            cwd: r.worktree_path.clone(),
            pid: None,
            git_root: None,
            tmux_session: Some(r.tmux_session_name),
            workspace_name: Some(r.workspace_name),
            worktree_path: Some(r.worktree_path),
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![SessionSource::Ainb],
            summary: None,
            last_seen_ms: parse_iso8601_ms(&r.created_at),
        })
        .collect())
}

fn parse_iso8601_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}
