// ABOUTME: One `ps` snapshot of the process table so pane→agent attribution is cheap.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use tokio::process::Command;

/// Levels walked below a pane pid. A pane's agent sits one to three levels
/// down (login shell → optional wrapper → agent). The cap exists to bound a
/// pathological or self-referential table, not to describe real nesting.
const MAX_DEPTH: usize = 8;

/// A `pid → (ppid, command)` snapshot of the whole process table.
///
/// The tmux reconciler samples every three seconds and needs to know which
/// panes actually hold an agent. Probing each pane separately would cost one
/// process spawn per pane per tick, so the table is read ONCE per pass and
/// walked in memory.
#[derive(Debug, Default, Clone)]
pub struct ProcessTable {
    children: HashMap<u32, Vec<u32>>,
    commands: HashMap<u32, String>,
}

impl ProcessTable {
    /// Read the live process table with a single `ps` invocation.
    ///
    /// # Errors
    /// Returns an error when `ps` cannot be spawned or exits non-zero. The
    /// caller treats that as "discovery unavailable" and leaves durable state
    /// untouched, rather than concluding that no pane holds an agent.
    pub async fn snapshot() -> Result<Self> {
        // `-ww` forces unlimited column width. macOS inherits the BSD habit of
        // truncating to the detected terminal width even when stdout is a pipe,
        // and `comm` is the LAST column — a truncated full path loses exactly the
        // basename the agent match depends on, so a real agent pane would look
        // agentless. Repeating `w` is understood by both BSD and procps.
        let output = Command::new("ps")
            .args(["-axww", "-o", "pid=,ppid=,comm="])
            .output()
            .await
            .context("failed to invoke `ps -axww -o pid=,ppid=,comm=`")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("ps exited non-zero: {stderr}");
        }

        Ok(Self::parse(&String::from_utf8_lossy(&output.stdout)))
    }

    /// Parse `pid ppid comm` rows.
    ///
    /// `comm` may contain spaces — macOS reports a full executable path, which
    /// can read `.../Browser Helper (Renderer)` — so only the two leading
    /// numeric columns are tokenised and the remainder is taken whole.
    #[must_use]
    pub fn parse(stdout: &str) -> Self {
        let mut table = Self::default();
        for line in stdout.lines() {
            let Some((pid, parent, command)) = parse_row(line) else {
                continue;
            };
            table.children.entry(parent).or_default().push(pid);
            table.commands.insert(pid, command);
        }
        table
    }

    /// Normalised command names of `root` and every descendant, nearest first.
    ///
    /// Breadth-first so the shallowest match wins: a Claude session that spawns
    /// a nested agent still reports as the outer process that owns the pane.
    #[must_use]
    pub fn tree_commands(&self, root: u32) -> Vec<&str> {
        let mut seen = HashSet::from([root]);
        let mut frontier = vec![root];
        let mut names = Vec::new();

        for _ in 0..=MAX_DEPTH {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for pid in frontier {
                if let Some(command) = self.commands.get(&pid) {
                    names.push(command.as_str());
                }
                for child in self.children.get(&pid).into_iter().flatten() {
                    if seen.insert(*child) {
                        next.push(*child);
                    }
                }
            }
            frontier = next;
        }

        names
    }
}

fn parse_row(line: &str) -> Option<(u32, u32, String)> {
    let rest = line.trim_start();
    let (pid, rest) = split_leading_token(rest)?;
    let (parent, rest) = split_leading_token(rest.trim_start())?;
    let command = rest.trim();
    if command.is_empty() {
        return None;
    }
    Some((pid.parse().ok()?, parent.parse().ok()?, normalize(command)))
}

fn split_leading_token(text: &str) -> Option<(&str, &str)> {
    let end = text.find(char::is_whitespace)?;
    Some((&text[..end], &text[end..]))
}

/// `comm` is a full path on macOS and a bare (15-char truncated) name on Linux.
/// Reducing both to a lowercase file name lets callers match exact names, which
/// keeps `/Applications/Claude.app/…/Claude` from passing as the `claude` CLI.
fn normalize(command: &str) -> String {
    command.rsplit('/').next().unwrap_or(command).trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "    1     0 /sbin/launchd\n",
        "  100     1 /bin/zsh\n",
        "  200   100 /Users/me/.local/bin/claude\n",
        "  300   200 /usr/bin/rg\n",
        "  400     1 /bin/zsh\n",
        "  500     1 /Applications/Arc.app/Helpers/Browser Helper (Renderer)\n",
    );

    #[test]
    fn walks_descendants_breadth_first_and_normalises_names() {
        let table = ProcessTable::parse(SAMPLE);

        assert_eq!(table.tree_commands(100), vec!["zsh", "claude", "rg"]);
        assert_eq!(table.tree_commands(200), vec!["claude", "rg"]);
    }

    #[test]
    fn a_bare_shell_tree_holds_no_agent() {
        let table = ProcessTable::parse(SAMPLE);

        // Pane 400 is a login shell with no children — the exact shape of a
        // leftover tmux pane whose agent already exited.
        assert_eq!(table.tree_commands(400), vec!["zsh"]);
    }

    #[test]
    fn command_paths_containing_spaces_survive_parsing() {
        let table = ProcessTable::parse(SAMPLE);

        assert_eq!(table.tree_commands(500), vec!["browser helper (renderer)"]);
    }

    #[test]
    fn unknown_pid_yields_no_commands() {
        assert!(ProcessTable::parse(SAMPLE).tree_commands(9999).is_empty());
    }

    #[test]
    fn a_self_parenting_row_cannot_loop_forever() {
        // A malformed table must terminate rather than spin the reconciler.
        let table = ProcessTable::parse("  7   7 /bin/loop\n");

        assert_eq!(table.tree_commands(7), vec!["loop"]);
    }
}
