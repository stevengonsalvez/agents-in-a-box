// ABOUTME: tmux send-keys + has-session wrappers.
//
// Uses `-l` literal mode for the text to prevent prompt-injection via
// shell metacharacters; sends Enter as a key event afterwards.

use anyhow::{Context, Result};
use tokio::process::Command;

pub async fn tmux_send(tmux_session: &str, text: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", tmux_session, "-l", text])
        .status()
        .await
        .context("invoking tmux send-keys (literal)")?;
    if !status.success() {
        anyhow::bail!("tmux send-keys -l exited {}", status);
    }
    let enter = Command::new("tmux")
        .args(["send-keys", "-t", tmux_session, "Enter"])
        .status()
        .await
        .context("invoking tmux send-keys Enter")?;
    if !enter.success() {
        anyhow::bail!("tmux send-keys Enter exited {}", enter);
    }
    Ok(())
}

pub async fn tmux_session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .status()
        .await
        .is_ok_and(|s| s.success())
}
