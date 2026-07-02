// ABOUTME: Scan ~/.claude/jobs/<id>/ for background-session state dirs.

use anyhow::Result;

use crate::fleet::types::{Session, SessionSource};

fn jobs_root() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("AINB_FLEET_JOBS_DIR") {
        return std::path::PathBuf::from(p);
    }
    let mut home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    home.push(".claude");
    home.push("jobs");
    home
}

pub fn discover_from_jobs() -> Result<Vec<Session>> {
    let root = jobs_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let cwd = entry.path().to_string_lossy().into_owned();
        out.push(Session {
            id: name.clone(),
            cwd,
            pid: None,
            git_root: None,
            tmux_session: None,
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: Some(name),
            transcript_path: None,
            sources: vec![SessionSource::Jobs],
            summary: None,
            last_seen_ms: None,
        });
    }
    Ok(out)
}
