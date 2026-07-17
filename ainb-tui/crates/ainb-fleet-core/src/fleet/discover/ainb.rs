// ABOUTME: Discover sessions by shelling out to `ainb list --format json`.

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::fleet::types::{AinbSession, Session, SessionSource};

/// Resolve the `ainb` binary to shell out to: `$AINB_BIN` if set (tests point it
/// at a fake / the test binary), else THIS executable, else the literal `ainb`
/// on `$PATH`.
///
/// The `current_exe()` step is load-bearing, not cosmetic: `cargo run` and a
/// stale `brew`-installed `ainb` can coexist on one machine, and a bare `"ainb"`
/// resolved against `$PATH` picks up the stale binary even when the running
/// process is the fresh one — yielding a different `list --format json` shape or
/// a different session store, i.e. silently wrong discovery. Mirrors
/// `atc_bin()` / `resolve_ainb_bin()` so every self-spawn prefers self.
fn ainb_bin() -> String {
    resolve_ainb_bin(std::env::var("AINB_BIN").ok(), std::env::current_exe().ok())
}

/// Pure resolver behind [`ainb_bin`]: pick the `$AINB_BIN` override (when
/// non-empty), else `current_exe`, else the bare `"ainb"` name. Split out so the
/// three-way precedence is testable without mutating process env (the crate
/// denies `unsafe`, and edition-2024 `set_var` is unsafe).
fn resolve_ainb_bin(
    override_var: Option<String>,
    current_exe: Option<std::path::PathBuf>,
) -> String {
    if let Some(v) = override_var.filter(|s| !s.is_empty()) {
        return v;
    }
    current_exe
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "ainb".to_string())
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

    let parsed: Vec<AinbSession> =
        serde_json::from_slice(&output.stdout).context("`ainb list` produced invalid JSON")?;
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
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::resolve_ainb_bin;
    use std::path::PathBuf;

    /// A non-empty `$AINB_BIN` wins over everything.
    #[test]
    fn override_wins() {
        assert_eq!(
            resolve_ainb_bin(
                Some("/tmp/fake-ainb".into()),
                Some(PathBuf::from("/usr/bin/ainb"))
            ),
            "/tmp/fake-ainb"
        );
    }

    /// With no (or empty) override, resolve to `current_exe` — NOT the bare
    /// `"ainb"` name that `$PATH` could resolve to a stale sibling binary. This
    /// is the whole point of the fix: the fresh running process discovers via
    /// itself, never a stale brew `ainb`.
    #[test]
    fn falls_back_to_current_exe_not_bare_name() {
        let resolved = resolve_ainb_bin(None, Some(PathBuf::from("/repo/target/debug/ainb")));
        assert_eq!(resolved, "/repo/target/debug/ainb");
        assert_ne!(resolved, "ainb");

        // An empty override string is treated as unset.
        let empty = resolve_ainb_bin(
            Some(String::new()),
            Some(PathBuf::from("/repo/target/debug/ainb")),
        );
        assert_eq!(empty, "/repo/target/debug/ainb");
    }

    /// Only when BOTH override and current_exe are unavailable does it degrade
    /// to the bare name (effectively unreachable in practice).
    #[test]
    fn last_resort_bare_name() {
        assert_eq!(resolve_ainb_bin(None, None), "ainb");
    }
}
