// ABOUTME: Discover sessions by direct SQLite read of `~/.claude-peers.db`.
//
// Faster than the broker HTTP API and avoids a network round-trip per
// discovery pass. Writes still go through the broker HTTP client.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

use crate::fleet::types::{BrokerPeer, Session, SessionSource};

/// Default DB path (override via `CLAUDE_PEERS_DB`).
fn db_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("CLAUDE_PEERS_DB") {
        return std::path::PathBuf::from(p);
    }
    let mut home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    home.push(".claude-peers.db");
    home
}

pub fn list_broker_peers() -> Result<Vec<BrokerPeer>> {
    let path = db_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", path.display()))?;

    let mut stmt = conn.prepare(
        "SELECT id, pid, cwd, git_root, tty, summary, registered_at, last_seen FROM peers",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(BrokerPeer {
            id: row.get(0)?,
            pid: row.get::<_, i64>(1)? as u32,
            cwd: row.get(2)?,
            git_root: row.get(3)?,
            tty: row.get(4)?,
            summary: row.get(5)?,
            registered_at: row.get(6)?,
            last_seen: row.get(7)?,
        })
    })?;

    let mut out = Vec::new();
    for r in rows {
        let peer = r?;
        if is_pid_alive(peer.pid) {
            out.push(peer);
        }
    }
    Ok(out)
}

#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    // signal 0 — checks existence without affecting the process.
    // `pid` is unsigned; the cast saturates at i32::MAX which is fine
    // because real PIDs on macOS/Linux never reach that range.
    let nix_pid = nix::unistd::Pid::from_raw(pid as i32);
    nix::sys::signal::kill(nix_pid, None).is_ok()
}

#[cfg(not(unix))]
fn is_pid_alive(_pid: u32) -> bool {
    true
}

pub fn discover_from_peers() -> Result<Vec<Session>> {
    let peers = list_broker_peers()?;
    Ok(peers
        .into_iter()
        .map(|p| Session {
            id: p.id.clone(),
            cwd: p.cwd,
            pid: Some(p.pid),
            git_root: p.git_root,
            tmux_session: None,
            workspace_name: None,
            worktree_path: None,
            peer_id: Some(p.id),
            bg_job_id: None,
            transcript_path: None,
            sources: vec![SessionSource::Peers],
            // Broker summary is unreliable — most peers never call set_summary.
            // Standup synthesises the field from JSONL instead. Leave None here
            // so the JSONL value isn't shadowed by a stale broker write.
            summary: None,
            last_seen_ms: parse_iso8601_ms(&p.last_seen),
        })
        .collect())
}

fn parse_iso8601_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.timestamp_millis())
}
