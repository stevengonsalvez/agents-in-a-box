// ABOUTME: tmux send-keys + has-session wrappers.
//
// Uses `-l` literal mode for the text to prevent prompt-injection via
// shell metacharacters; sends Enter as a key event afterwards.
//
// The payload is passed AFTER a `--` end-of-options terminator: without it a
// `text` that begins with `-` (e.g. an interview option label `-y` / `--no`, or
// a `fleet broadcast` prompt) is parsed by tmux as a flag rather than literal
// keys, silently dropping (or corrupting) the send.

use anyhow::{Context, Result};
use tokio::process::Command;

/// Build the `tmux send-keys` argv for a literal payload. The `--` terminator
/// sits between the flags and `text` so a `-`-prefixed payload (e.g. an
/// interview option label like `-y`/`--no`, or a broadcast prompt) is parsed as
/// literal keys, not a tmux flag.
fn send_keys_literal_args<'a>(tmux_session: &'a str, text: &'a str) -> [&'a str; 6] {
    ["send-keys", "-t", tmux_session, "-l", "--", text]
}

pub async fn tmux_send(tmux_session: &str, text: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(send_keys_literal_args(tmux_session, text))
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

#[cfg(test)]
mod tests {
    use super::send_keys_literal_args;

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
}
