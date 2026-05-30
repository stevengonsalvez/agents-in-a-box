// ABOUTME: tmux capture-pane wrapper + signal detection from buffer text.

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::fleet::types::Signal;

pub async fn capture_pane(tmux_session: &str, lines: u32) -> Result<String> {
    let scroll_arg = format!("-{lines}");
    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-t",
            tmux_session,
            "-p",
            "-S",
            scroll_arg.as_str(),
        ])
        .output()
        .await
        .context("invoking tmux capture-pane")?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn detect_signals_from_pane(pane: &str, at_ms: i64) -> Vec<Signal> {
    let mut out = Vec::new();
    // AskUserQuestion UI has a recognisable header band; refine as samples accrue.
    if pane.contains('?')
        && pane.contains('│')
        && pane.contains('┌')
        && pane.contains('└')
    {
        let snippet_start = pane.len().saturating_sub(400);
        out.push(Signal::AskUserQuestion {
            at: at_ms,
            raw: pane[snippet_start..].to_string(),
        });
    }
    out
}
