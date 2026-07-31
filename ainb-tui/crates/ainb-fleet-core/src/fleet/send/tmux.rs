// ABOUTME: tmux send-keys + has-session wrappers, with submit verification
// for multi-line payloads.
//
// Uses `-l` literal mode for the text to prevent prompt-injection via
// shell metacharacters; sends Enter as a key event afterwards.
//
// The payload is passed AFTER a `--` end-of-options terminator: without it a
// `text` that begins with `-` (e.g. an interview option label `-y` / `--no`, or
// a `fleet broadcast` prompt) is parsed by tmux as a flag rather than literal
// keys, silently dropping (or corrupting) the send.
//
// SUBMIT RELIABILITY: a multi-line payload arrives in Claude Code as a
// bracketed paste — the composer shows "[Pasted text #N +X lines]" and needs a
// beat to ingest it before an Enter actually submits. The original
// fire-and-forget (literal send + immediate Enter, no verification) left
// pastes parked in the composer whenever the session was busy; successive
// sends then STACKED (the ATC heartbeat incident: 8 unsubmitted pastes).
// Multi-line sends now settle, press Enter, then verify via capture-pane that
// the composer emptied, retrying Enter a few times before reporting failure.
// Single-line sends keep the fast path — they render as plain typed text and
// have never mis-submitted.

use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command;
use tokio::time::sleep;

use crate::fleet::read::tmux_pane::capture_pane;

/// Give Claude Code's composer time to ingest a bracketed paste before Enter.
const PASTE_SETTLE: Duration = Duration::from_millis(1000);
/// Wait after each Enter before checking whether the composer emptied.
const SUBMIT_CHECK_DELAY: Duration = Duration::from_millis(700);
/// Extra Enter attempts when the paste is still parked in the composer.
const ENTER_RETRIES: u32 = 3;
/// Backoff between Enter retries.
const RETRY_BACKOFF: Duration = Duration::from_millis(1500);

/// Build the `tmux send-keys` argv for a literal payload. The `--` terminator
/// sits between the flags and `text` so a `-`-prefixed payload (e.g. an
/// interview option label like `-y`/`--no`, or a broadcast prompt) is parsed as
/// literal keys, not a tmux flag.
fn send_keys_literal_args<'a>(tmux_session: &'a str, text: &'a str) -> [&'a str; 6] {
    ["send-keys", "-t", tmux_session, "-l", "--", text]
}

fn picker_key_args<'a>(tmux_session: &'a str, key: &'a str) -> Option<[&'a str; 4]> {
    let allowed = matches!(
        key,
        "0" | "1"
            | "2"
            | "3"
            | "4"
            | "5"
            | "6"
            | "7"
            | "8"
            | "9"
            | "Enter"
            | "Up"
            | "Down"
            | "Left"
            | "Right"
            | "Space"
            | "Tab"
            | "BTab"
    );
    allowed.then_some(["send-keys", "-t", tmux_session, key])
}

pub async fn tmux_send(tmux_session: &str, text: &str) -> Result<()> {
    let status = Command::new(crate::fleet::tmux_bin())
        .args(send_keys_literal_args(tmux_session, text))
        .status()
        .await
        .context("invoking tmux send-keys (literal)")?;
    if !status.success() {
        anyhow::bail!("tmux send-keys -l exited {}", status);
    }

    // Fast path: single-line text renders as plain typed input and the
    // immediate Enter submits reliably — keep the historical behavior.
    if !text.contains('\n') {
        return tmux_press_enter(tmux_session).await;
    }

    // Multi-line text arrives as a bracketed paste: settle, submit, verify.
    sleep(PASTE_SETTLE).await;
    for attempt in 0..=ENTER_RETRIES {
        tmux_press_enter(tmux_session).await?;
        sleep(SUBMIT_CHECK_DELAY).await;
        if !pane_has_unsubmitted_input(tmux_session).await {
            return Ok(());
        }
        if attempt < ENTER_RETRIES {
            sleep(RETRY_BACKOFF).await;
        }
    }
    anyhow::bail!(
        "multi-line send to {tmux_session} not submitted after {} Enter attempts \
(composer still shows the pending paste)",
        ENTER_RETRIES + 1
    )
}

/// Press Enter in the target session (used both to submit a send and to flush
/// a previously-parked paste).
pub async fn tmux_press_enter(tmux_session: &str) -> Result<()> {
    let enter = Command::new(crate::fleet::tmux_bin())
        .args(["send-keys", "-t", tmux_session, "Enter"])
        .status()
        .await
        .context("invoking tmux send-keys Enter")?;
    if !enter.success() {
        anyhow::bail!("tmux send-keys Enter exited {}", enter);
    }
    Ok(())
}

/// Route one constrained picker key without converting it to generic text.
pub async fn tmux_send_picker_key(tmux_session: &str, key: &str) -> Result<()> {
    let args = picker_key_args(tmux_session, key)
        .ok_or_else(|| anyhow::anyhow!("unsupported verified picker key: {key}"))?;
    let status = Command::new(crate::fleet::tmux_bin())
        .args(args)
        .status()
        .await
        .context("invoking tmux send-keys for verified picker")?;
    if !status.success() {
        anyhow::bail!("tmux verified picker send-keys exited {status}");
    }
    Ok(())
}

/// True when the session's visible composer still holds an unsubmitted nudge
/// (a parked bracketed paste or an unsubmitted `[HEARTBEAT` line).
///
/// Capture failure degrades to `false` (assume submitted) so a transient
/// capture-pane error never wedges the send path — worst case we fall back to
/// the historical fire-and-forget behavior.
pub async fn pane_has_unsubmitted_input(tmux_session: &str) -> bool {
    match capture_pane(tmux_session, 0).await {
        Ok(pane) => composer_pending(&pane),
        Err(_) => false,
    }
}

/// Inspect visible pane text for an unsubmitted composer.
///
/// Claude Code renders the composer as the LAST prompt-prefixed line on screen
/// (`>`, `❯`, or `›`, possibly inside a `│` box border). Transcript echoes of
/// already-submitted messages use the same prefix but sit above the composer,
/// so only the last such line is inspected. Pending markers: a bracketed-paste
/// placeholder (`[Pasted text #`) or a raw unsubmitted heartbeat (`[HEARTBEAT`).
///
/// A false positive costs one harmless extra Enter (empty-composer submit is a
/// no-op) or a skipped idempotent tick; a false negative reverts to the old
/// stacking behavior for one tick — both self-heal.
fn composer_pending(pane: &str) -> bool {
    for line in pane.lines().rev() {
        let t = line.trim_start_matches(['│', ' ', '\t']);
        for prefix in ['>', '❯', '›'] {
            if let Some(rest) = t.strip_prefix(prefix) {
                let rest = rest.trim();
                return rest.contains("[Pasted text #") || rest.contains("[HEARTBEAT");
            }
        }
    }
    false
}

pub async fn tmux_session_exists(name: &str) -> bool {
    Command::new(crate::fleet::tmux_bin())
        .args(["has-session", "-t", name])
        .status()
        .await
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::{composer_pending, picker_key_args, send_keys_literal_args};

    #[test]
    fn dash_prefixed_payload_sits_after_the_terminator() {
        // A `-`-prefixed option label must land AFTER `--` so tmux treats it as
        // literal keys, not a flag. (Regression: without the terminator `-y`
        // was parsed as an unknown tmux flag and the send silently failed.)
        let args = send_keys_literal_args("sess", "-y");
        assert_eq!(args, ["send-keys", "-t", "sess", "-l", "--", "-y"]);
        let term = args.iter().position(|a| *a == "--").expect("`--` present");
        let payload = args.iter().position(|a| *a == "-y").expect("payload present");
        assert!(term < payload, "payload must follow the `--` terminator");
    }

    #[test]
    fn plain_payload_also_terminated() {
        let args = send_keys_literal_args("sess", "ship to prod");
        assert_eq!(
            args,
            ["send-keys", "-t", "sess", "-l", "--", "ship to prod"]
        );
    }

    #[test]
    fn picker_keys_use_non_literal_constrained_routing() {
        assert_eq!(
            picker_key_args("sess:0.0", "1"),
            Some(["send-keys", "-t", "sess:0.0", "1"])
        );
        assert_eq!(
            picker_key_args("sess:0.0", "Enter"),
            Some(["send-keys", "-t", "sess:0.0", "Enter"])
        );
        assert_eq!(picker_key_args("sess:0.0", "option label"), None);
        assert_eq!(picker_key_args("sess:0.0", "C-c"), None);
    }

    #[test]
    fn parked_paste_in_composer_is_pending() {
        let pane = "some transcript text\n\
                    ● did a thing\n\
                    > [Pasted text #8 +5 lines][Pasted text #9 +12 lines]rest\n\
                    status line · tokens";
        assert!(composer_pending(pane));
    }

    #[test]
    fn unsubmitted_heartbeat_line_is_pending() {
        let pane = "transcript\n› [HEARTBEAT 1782949818090] 16 session(s) need attention\nstatus";
        assert!(composer_pending(pane));
    }

    #[test]
    fn empty_composer_is_not_pending() {
        // Transcript echo of an ALREADY-submitted paste sits above the (empty)
        // composer — the last prompt-prefixed line decides, so this is clean.
        let pane = "> [Pasted text #1 +12 lines]\n\
                    ● handled it\n\
                    > \n\
                    status line";
        assert!(!composer_pending(pane));
    }

    #[test]
    fn typed_text_in_composer_is_not_a_pending_nudge() {
        // A human mid-keystroke is not a parked nudge — do not flag it.
        let pane = "transcript\n> fix the login bug\nstatus";
        assert!(!composer_pending(pane));
    }

    #[test]
    fn no_prompt_line_is_not_pending() {
        assert!(!composer_pending("plain shell output\n$ ls\nfile"));
    }
}
