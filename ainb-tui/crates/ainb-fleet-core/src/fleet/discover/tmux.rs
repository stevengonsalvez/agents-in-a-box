// ABOUTME: Discover every tmux pane with exact target and process metadata.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::fleet::types::{
    AttentionState, Capabilities, Confidence, FleetSession, LifecycleState, ManagementState,
    Provenance, Provider, SessionKey, TransportHealth,
};

const LIST_FORMAT: &str = concat!(
    "#{session_name}\t#{window_index}\t#{pane_index}\t#{pane_id}\t",
    "#{pane_pid}\t#{pane_current_path}\t#{pane_current_command}\t",
    "#{pane_start_command}\t#{session_created}"
);

#[derive(Debug, Clone, PartialEq, Eq)]
struct TmuxPaneRow {
    session_name: String,
    window_index: String,
    pane_index: String,
    pane_id: String,
    pane_pid: u32,
    cwd: String,
    current_command: String,
    start_command: String,
    session_created: i64,
}

impl TmuxPaneRow {
    fn exact_target(&self) -> String {
        format!(
            "{}:{}.{}",
            self.session_name, self.window_index, self.pane_index
        )
    }

    fn provider(&self) -> Provider {
        infer_provider(&format!(
            "{} {} {}",
            self.session_name, self.current_command, self.start_command
        ))
    }

    fn process_start_fingerprint(&self) -> String {
        format!(
            "pane={};pid={};session_started={}",
            self.pane_id, self.pane_pid, self.session_created
        )
    }

    fn into_fleet_session(self) -> FleetSession {
        let provider = self.provider();
        let exact_tmux_target = self.exact_target();
        let fingerprint = self.process_start_fingerprint();
        FleetSession {
            session_key: SessionKey::legacy(provider, &exact_tmux_target, &fingerprint),
            provider,
            provider_session_id: None,
            cwd: self.cwd,
            exact_tmux_target: Some(exact_tmux_target),
            pane_pid: Some(self.pane_pid),
            process_start_fingerprint: Some(fingerprint),
            lifecycle: LifecycleState::Unknown,
            attention: AttentionState::None,
            management: ManagementState::Degraded,
            capabilities: Capabilities::degraded_tmux(),
            provenance: BTreeSet::from([Provenance::Tmux]),
            confidence: Confidence::Inferred,
            transport_health: TransportHealth::Healthy,
            first_seen_ms: self.session_created.checked_mul(1000),
            last_seen_ms: None,
            version: 0,
        }
    }
}

/// Enumerate all tmux panes. One exact pane target produces one Fleet row.
pub async fn discover_from_tmux() -> Result<Vec<FleetSession>> {
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", LIST_FORMAT])
        .output()
        .await
        .context("failed to invoke `tmux list-panes -a`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if tmux_has_no_sessions(&stderr) {
            return Ok(Vec::new());
        }
        anyhow::bail!("tmux list-panes exited non-zero: {stderr}");
    }

    let stdout = String::from_utf8(output.stdout).context("tmux list-panes returned non-UTF8")?;
    parse_rows(&stdout).map(|rows| rows.into_iter().map(TmuxPaneRow::into_fleet_session).collect())
}

fn parse_rows(stdout: &str) -> Result<Vec<TmuxPaneRow>> {
    stdout.lines().filter(|line| !line.is_empty()).map(parse_row).collect()
}

fn parse_row(line: &str) -> Result<TmuxPaneRow> {
    let fields: Vec<&str> = line.splitn(9, '\t').collect();
    if fields.len() != 9 {
        anyhow::bail!(
            "tmux list-panes row has {} fields, expected 9",
            fields.len()
        );
    }
    Ok(TmuxPaneRow {
        session_name: fields[0].to_string(),
        window_index: fields[1].to_string(),
        pane_index: fields[2].to_string(),
        pane_id: fields[3].to_string(),
        pane_pid: fields[4]
            .parse()
            .with_context(|| format!("invalid pane pid in tmux row: {line}"))?,
        cwd: fields[5].to_string(),
        current_command: fields[6].to_string(),
        start_command: fields[7].to_string(),
        session_created: fields[8]
            .parse()
            .with_context(|| format!("invalid session creation time in tmux row: {line}"))?,
    })
}

fn infer_provider(command_text: &str) -> Provider {
    let text = command_text.to_ascii_lowercase();
    if text.contains("codex") {
        Provider::Codex
    } else if text.contains("claude") {
        Provider::Claude
    } else {
        Provider::Unknown
    }
}

fn tmux_has_no_sessions(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    text.contains("no server running") || text.contains("no sessions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::types::{Capability, ManagementState};

    #[test]
    fn parser_preserves_same_cwd_as_distinct_exact_targets() {
        let rows = parse_rows(concat!(
            "claude-a\t0\t0\t%1\t101\t/repo\tclaude\tclaude\t1700000000\n",
            "codex-b\t2\t1\t%2\t202\t/repo\tcodex\tcodex\t1700000001\n"
        ))
        .expect("parse tmux rows");

        let sessions: Vec<_> = rows.into_iter().map(TmuxPaneRow::into_fleet_session).collect();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].cwd, sessions[1].cwd);
        assert_ne!(sessions[0].session_key, sessions[1].session_key);
        assert_eq!(
            sessions[0].exact_tmux_target.as_deref(),
            Some("claude-a:0.0")
        );
        assert_eq!(
            sessions[1].exact_tmux_target.as_deref(),
            Some("codex-b:2.1")
        );
        assert_eq!(sessions[0].provider, Provider::Claude);
        assert_eq!(sessions[1].provider, Provider::Codex);
    }

    #[test]
    fn tmux_rows_are_degraded_and_only_allow_safe_fallbacks() {
        let row = parse_row("plain\t0\t0\t%9\t909\t/tmp\tzsh\tzsh\t1700000000").expect("parse row");
        let session = row.into_fleet_session();

        assert_eq!(session.management, ManagementState::Degraded);
        assert!(session.capabilities.contains(Capability::TextSend));
        assert!(session.capabilities.contains(Capability::TmuxAttach));
        assert!(!session.capabilities.contains(Capability::StructuredAnswer));
        assert!(
            session
                .process_start_fingerprint
                .as_deref()
                .is_some_and(|value| value.contains("pid=909"))
        );
    }

    #[test]
    fn no_server_errors_are_empty_fleet_not_failure() {
        assert!(tmux_has_no_sessions("no server running on /tmp/tmux.sock"));
        assert!(tmux_has_no_sessions("no sessions"));
        assert!(!tmux_has_no_sessions("permission denied"));
    }
}
