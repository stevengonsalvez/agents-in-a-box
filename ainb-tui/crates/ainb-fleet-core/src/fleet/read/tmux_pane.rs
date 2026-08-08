// ABOUTME: tmux capture-pane wrapper + signal detection from buffer text.

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::fleet::types::Signal;

pub async fn capture_pane(tmux_session: &str, lines: u32) -> Result<String> {
    capture(tmux_session, lines, Ansi::Stripped).await
}

/// Capture the pane KEEPING its ANSI attributes (`capture-pane -e`).
///
/// The DIM attribute (`ESC [ 2 m`) is the only thing that distinguishes an
/// EMPTY composer showing Claude Code's dim ghost of a previous prompt from a
/// composer holding real unsubmitted text: in plain `-p` output the two are
/// byte-identical in shape. `Press up to edit queued messages` (a BUSY session
/// that has already accepted the send) is dim as well. The send path's
/// verification has to tell those apart, so it captures with `-e`; every other
/// reader keeps the plain [`capture_pane`], whose output feeds substring
/// matching that escape sequences would only disturb.
pub async fn capture_pane_ansi(tmux_session: &str, lines: u32) -> Result<String> {
    capture(tmux_session, lines, Ansi::Kept).await
}

/// Whether a capture keeps the pane's ANSI attributes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ansi {
    Stripped,
    Kept,
}

async fn capture(tmux_session: &str, lines: u32, ansi: Ansi) -> Result<String> {
    let scroll_arg = format!("-{lines}");
    let mut args = vec![
        "capture-pane",
        "-t",
        tmux_session,
        "-p",
        "-S",
        scroll_arg.as_str(),
    ];
    if ansi == Ansi::Kept {
        args.push("-e");
    }
    let output = Command::new("tmux")
        .args(args)
        .output()
        .await
        .context("invoking tmux capture-pane")?;
    // A missing/ambiguous pane exits 1 with an EMPTY stdout and the diagnostic
    // on stderr ("can't find pane: <name>"). Returning that empty string as
    // `Ok` made every capture failure look like a clean, empty pane, which is
    // how the send path concluded "composer emptied, submitted" for a session
    // that had died mid-send. Fail loudly instead.
    if !output.status.success() {
        anyhow::bail!(
            "tmux capture-pane -t {tmux_session} exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn detect_signals_from_pane(pane: &str, at_ms: i64) -> Vec<Signal> {
    let mut out = Vec::new();
    // AskUserQuestion UI has a recognisable header band; refine as samples accrue.
    if pane.contains('?') && pane.contains('│') && pane.contains('┌') && pane.contains('└') {
        let snippet_start = pane.len().saturating_sub(400);
        out.push(Signal::AskUserQuestion {
            at: at_ms,
            raw: pane[snippet_start..].to_string(),
        });
    }
    out
}
