// ABOUTME: `ainb fleet broadcast` — send one prompt to selected sessions.
//
// Default targeting: requires `--all` or `--filter <regex>` / `--cwd <substring>`
// to fan out. Standup-style picker is not in v1 (orchestration is invoked from
// within an LLM session that already drives selection).

use anyhow::{bail, Result};
use regex::Regex;

use crate::cli::OutputFormat;
use crate::fleet::discover::{discover_from_ainb, discover_from_peers, merge_sessions};
use crate::fleet::send::send;
use crate::fleet::types::{SendOutcome, Session};

pub async fn execute(matches: &clap::ArgMatches, _format: OutputFormat) -> Result<()> {
    let prompt = matches
        .get_one::<String>("prompt")
        .ok_or_else(|| anyhow::anyhow!("missing <prompt>"))?
        .clone();
    let all = matches.get_flag("all");
    let filter = matches.get_one::<String>("filter").cloned();
    let cwd = matches.get_one::<String>("cwd").cloned();

    if !all && filter.is_none() && cwd.is_none() {
        bail!("broadcast requires --all, --filter <regex>, or --cwd <substring>");
    }

    let (ainb, peers) = tokio::join!(discover_from_ainb(), async { discover_from_peers() });
    let merged = merge_sessions(vec![
        ainb.unwrap_or_default(),
        peers.unwrap_or_default(),
    ]);

    let targets = filter_targets(&merged, all, filter.as_deref(), cwd.as_deref())?;
    if targets.is_empty() {
        bail!("no targets matched");
    }

    let mut results = Vec::with_capacity(targets.len());
    for t in &targets {
        let outcome = send(t, &prompt).await?;
        results.push((t.clone(), outcome));
    }

    print_results(&results);
    Ok(())
}

fn filter_targets(
    all_sessions: &[Session],
    all: bool,
    filter: Option<&str>,
    cwd: Option<&str>,
) -> Result<Vec<Session>> {
    let re = filter.map(Regex::new).transpose()?;
    let out = all_sessions
        .iter()
        .filter(|s| {
            if !all {
                let mut keep = false;
                if let Some(ref re) = re {
                    if re.is_match(s.tmux_session.as_deref().unwrap_or(""))
                        || re.is_match(s.workspace_name.as_deref().unwrap_or(""))
                    {
                        keep = true;
                    }
                }
                if let Some(needle) = cwd {
                    if s.cwd.contains(needle) {
                        keep = true;
                    }
                }
                if !keep {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect();
    Ok(out)
}

fn print_results(results: &[(Session, SendOutcome)]) {
    let total = results.len();
    println!("ainb fleet broadcast — sent to {total} target(s)");
    for (t, outcome) in results {
        let name = t
            .tmux_session
            .as_deref()
            .or(t.workspace_name.as_deref())
            .unwrap_or(&t.id);
        match outcome {
            SendOutcome::Broker { peer_id } => {
                println!("  ✓ {name} via broker peer {peer_id}");
            }
            SendOutcome::Tmux { tmux_session } => {
                println!("  ✓ {name} via tmux {tmux_session}");
            }
            SendOutcome::Failed { reason } => {
                println!("  ✗ {name}: {reason}");
            }
        }
    }
}
