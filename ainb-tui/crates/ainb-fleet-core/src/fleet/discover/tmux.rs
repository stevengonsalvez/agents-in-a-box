// ABOUTME: Discover every tmux pane with exact target and process metadata.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::fleet::discover::process_tree::ProcessTable;
use crate::fleet::types::{
    AttentionState, Capabilities, Confidence, FleetSession, LifecycleState, ManagementState,
    Provenance, Provider, SessionKey, TransportHealth,
};

const LIST_FORMAT: &str = concat!(
    "#{session_name}\t#{window_index}\t#{pane_index}\t#{pane_id}\t",
    "#{pane_pid}\t#{pane_current_path}\t#{pane_current_command}\t",
    "#{pane_start_command}\t#{session_created}\t#{pane_dead}\t#{window_activity}"
);

/// Silence after which a live pane counts as between turns rather than working.
///
/// Sized above a slow tool call (a build, a full test suite) so a quiet-but-busy
/// agent is not mislabelled idle, and well under the multi-hour gap that
/// separates a working session from an abandoned one.
const IDLE_AFTER_SECS: i64 = 120;

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
    pane_dead: bool,
    /// Epoch seconds of the last output in this pane's window. `None` when tmux
    /// does not report it, which keeps the lifecycle honestly `Unknown` instead
    /// of inventing an idle age from a missing value.
    window_activity: Option<i64>,
}

impl TmuxPaneRow {
    fn exact_target(&self) -> String {
        format!(
            "{}:{}.{}",
            self.session_name, self.window_index, self.pane_index
        )
    }

    fn process_start_fingerprint(&self) -> String {
        format!(
            "pane={};pid={};session_started={}",
            self.pane_id, self.pane_pid, self.session_created
        )
    }

    /// Derive lifecycle from what tmux already knows about the pane.
    ///
    /// This is an INFERRED observation, so a provider hook always outranks it
    /// (see `should_replace` in the fleet repo) — it only has to be right for
    /// sessions that emit no hooks at all, which is most of them.
    const fn lifecycle(&self, now_secs: i64) -> LifecycleState {
        if self.pane_dead {
            return LifecycleState::Exited;
        }
        match self.window_activity {
            Some(activity) if now_secs.saturating_sub(activity) > IDLE_AFTER_SECS => {
                LifecycleState::Idle
            }
            Some(_) => LifecycleState::Running,
            None => LifecycleState::Unknown,
        }
    }

    /// Build a Fleet row for this pane.
    ///
    /// The provider comes from the pane's process tree, not its command string:
    /// a running Claude renames itself to its version (`pane_current_command`
    /// reads `2.1.220`), while a leftover pane whose agent exited still reports
    /// a perfectly ordinary `zsh`. Only the tree distinguishes the two. A pane
    /// with no agent in its tree yields `Provider::Unknown`.
    fn into_session(self, processes: &ProcessTable, now_secs: i64) -> FleetSession {
        let provider = agent_provider(processes, self.pane_pid).unwrap_or(Provider::Unknown);
        let exact_tmux_target = self.exact_target();
        let fingerprint = self.process_start_fingerprint();
        let lifecycle = self.lifecycle(now_secs);
        FleetSession {
            session_key: SessionKey::legacy(provider, &exact_tmux_target, &fingerprint),
            provider,
            provider_session_id: None,
            cwd: self.cwd,
            exact_tmux_target: Some(exact_tmux_target),
            pane_pid: Some(self.pane_pid),
            process_start_fingerprint: Some(fingerprint),
            lifecycle,
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

/// Panes that actually hold an agent — the Fleet roster.
///
/// Panes without an agent process are NOT Fleet sessions: a host accumulates
/// plenty of ordinary shells, and admitting them buries the real sessions in
/// rows that can never carry a lifecycle.
///
/// Use [`discover_all_tmux_panes`] instead for identity or liveness checks. A
/// control action must not fail merely because a pane's agent is momentarily
/// between processes.
pub async fn discover_from_tmux() -> Result<Vec<FleetSession>> {
    Ok(discover_all_tmux_panes()
        .await?
        .into_iter()
        // The provider is derived from the very tree the gate asks about, so
        // "has a known provider" and "holds an agent" are the same predicate.
        .filter(|session| session.provider != Provider::Unknown)
        .collect())
}

/// Every tmux pane, agent-bearing or not. One exact pane target produces one
/// row.
///
/// This is the roster for identity and liveness checks — "is pane X with
/// fingerprint Y still there" — which must stay true to tmux rather than to
/// Fleet's view of what deserves a row.
pub async fn discover_all_tmux_panes() -> Result<Vec<FleetSession>> {
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
    let rows = parse_rows(&stdout)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    // A `ps` failure is discovery being unavailable, not proof that no pane
    // holds an agent — propagate so the caller leaves durable state alone
    // instead of retiring every session.
    let processes = ProcessTable::snapshot().await?;
    let now_secs = chrono::Utc::now().timestamp();
    Ok(rows.into_iter().map(|row| row.into_session(&processes, now_secs)).collect())
}

fn parse_rows(stdout: &str) -> Result<Vec<TmuxPaneRow>> {
    stdout.lines().filter(|line| !line.is_empty()).map(parse_row).collect()
}

fn parse_row(line: &str) -> Result<TmuxPaneRow> {
    let fields: Vec<&str> = line.splitn(11, '\t').collect();
    if fields.len() != 11 {
        anyhow::bail!(
            "tmux list-panes row has {} fields, expected 11",
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
        // Both activity fields degrade rather than fail the whole discovery
        // pass: an older tmux that reports neither still yields usable rows,
        // just with an `Unknown` lifecycle.
        pane_dead: fields[9].trim() == "1",
        window_activity: fields[10].trim().parse().ok(),
    })
}

/// The agent that owns a pane, or `None` when the pane holds none.
///
/// Matched on exact process names rather than substrings, so neither the Claude
/// desktop app (`…/Claude.app/Contents/MacOS/Claude`) nor a branch called
/// `f/claude-resume` can pass as a session.
///
/// Copilot is admitted now that `Provider::Copilot` exists on the wire AND the
/// Swift client decodes unknown enum values tolerantly. Emitting a provider token
/// an older client has never seen used to fail its whole snapshot decode, so this
/// name could not land before those two.
fn agent_provider(processes: &ProcessTable, pane_pid: u32) -> Option<Provider> {
    processes.tree_commands(pane_pid).into_iter().find_map(|command| match command {
        "claude" => Some(Provider::Claude),
        "codex" => Some(Provider::Codex),
        _ => None,
    })
}

fn tmux_has_no_sessions(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    text.contains("no server running") || text.contains("no sessions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::types::{Capability, ManagementState};

    const NOW: i64 = 1_700_000_500;

    /// Pane 101 runs claude, pane 202 runs codex, pane 909 is a bare shell.
    fn processes() -> ProcessTable {
        ProcessTable::parse(concat!(
            "  101     1 /bin/zsh\n",
            "  111   101 /Users/me/.local/bin/claude\n",
            "  202     1 /bin/zsh\n",
            "  222   202 /opt/homebrew/bin/codex\n",
            "  909     1 /bin/zsh\n",
        ))
    }

    #[test]
    fn parser_preserves_same_cwd_as_distinct_exact_targets() {
        let rows = parse_rows(concat!(
            "claude-a\t0\t0\t%1\t101\t/repo\t2.1.220\tclaude\t1700000000\t0\t1700000499\n",
            "codex-b\t2\t1\t%2\t202\t/repo\tcodex\tcodex\t1700000001\t0\t1700000499\n"
        ))
        .expect("parse tmux rows");

        let table = processes();
        let sessions: Vec<_> = rows.into_iter().map(|row| row.into_session(&table, NOW)).collect();
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
    fn a_renamed_claude_process_is_still_detected_as_claude() {
        // Claude reports its VERSION as `pane_current_command`, so the pane
        // command string says `2.1.220` and only the process tree says claude.
        let row = parse_row("build\t0\t0\t%1\t101\t/repo\t2.1.220\tzsh\t1700000000\t0\t1700000499")
            .expect("parse row");

        let session = row.into_session(&processes(), NOW);

        assert_eq!(session.provider, Provider::Claude);
    }

    #[test]
    fn a_branch_named_after_claude_is_not_a_session() {
        // The old string heuristic matched the tmux SESSION NAME, so a branch
        // called `f/claude-resume` minted a bogus CLAUDE row.
        let row = parse_row(
            "tmux_repo--f-claude-resume\t0\t0\t%9\t909\t/tmp\tzsh\tzsh\t1700000000\t0\t1700000499",
        )
        .expect("parse row");

        assert_eq!(
            row.into_session(&processes(), NOW).provider,
            Provider::Unknown
        );
    }

    #[test]
    fn a_bare_shell_pane_is_not_a_fleet_session() {
        let row = parse_row("plain\t0\t0\t%9\t909\t/tmp\tzsh\tzsh\t1700000000\t0\t1700000499")
            .expect("parse row");

        // `Unknown` is exactly what the roster filter drops, so a bare shell is
        // still enumerated for identity checks but never becomes a Fleet row.
        assert_eq!(
            row.into_session(&processes(), NOW).provider,
            Provider::Unknown
        );
    }

    #[test]
    fn the_roster_filter_drops_only_the_agentless_panes() {
        let rows = parse_rows(concat!(
            "claude-a\t0\t0\t%1\t101\t/repo\t2.1.220\tzsh\t1700000000\t0\t1700000499\n",
            "bare\t0\t0\t%9\t909\t/tmp\tzsh\tzsh\t1700000000\t0\t1700000499\n",
            "codex-b\t2\t1\t%2\t202\t/repo\tcodex\tcodex\t1700000001\t0\t1700000499\n"
        ))
        .expect("parse tmux rows");
        let table = processes();

        let all: Vec<_> = rows.into_iter().map(|row| row.into_session(&table, NOW)).collect();
        let roster = all.iter().filter(|session| session.provider != Provider::Unknown).count();

        // Identity and liveness checks keep every pane; the Fleet roster keeps
        // only the two that hold an agent.
        assert_eq!(all.len(), 3);
        assert_eq!(roster, 2);
        assert!(
            all.iter()
                .any(|session| session.exact_tmux_target.as_deref() == Some("bare:0.0")),
            "the agentless pane must still be enumerated for liveness checks"
        );
    }

    #[test]
    fn tmux_rows_are_degraded_and_only_allow_safe_fallbacks() {
        let row = parse_row("plain\t0\t0\t%1\t101\t/tmp\tzsh\tclaude\t1700000000\t0\t1700000499")
            .expect("parse row");
        let session = row.into_session(&processes(), NOW);

        assert_eq!(session.management, ManagementState::Degraded);
        assert!(session.capabilities.contains(Capability::TextSend));
        assert!(session.capabilities.contains(Capability::TmuxAttach));
        assert!(!session.capabilities.contains(Capability::StructuredAnswer));
        assert!(
            session
                .process_start_fingerprint
                .as_deref()
                .is_some_and(|value| value.contains("pid=101"))
        );
    }

    #[test]
    fn lifecycle_follows_pane_activity_age() {
        let live = |activity: &str, dead: &str| {
            parse_row(&format!(
                "s\t0\t0\t%1\t101\t/repo\tzsh\tclaude\t1700000000\t{dead}\t{activity}"
            ))
            .expect("parse row")
            .into_session(&processes(), NOW)
            .lifecycle
        };

        // NOW is 1_700_000_500; the threshold is 120s.
        assert_eq!(live("1700000499", "0"), LifecycleState::Running);
        assert_eq!(live("1700000380", "0"), LifecycleState::Running);
        assert_eq!(live("1700000379", "0"), LifecycleState::Idle);
        assert_eq!(live("1699990000", "0"), LifecycleState::Idle);
        assert_eq!(live("1700000499", "1"), LifecycleState::Exited);
    }

    #[test]
    fn a_tmux_without_activity_reporting_stays_unknown_not_idle() {
        // Inventing an idle age from a missing field would mark a whole fleet
        // idle on any tmux that does not report `window_activity`.
        let row =
            parse_row("s\t0\t0\t%1\t101\t/repo\tzsh\tclaude\t1700000000\t0\t").expect("parse row");

        let session = row.into_session(&processes(), NOW);

        assert_eq!(session.lifecycle, LifecycleState::Unknown);
    }

    #[test]
    fn no_server_errors_are_empty_fleet_not_failure() {
        assert!(tmux_has_no_sessions("no server running on /tmp/tmux.sock"));
        assert!(tmux_has_no_sessions("no sessions"));
        assert!(!tmux_has_no_sessions("permission denied"));
    }
}
