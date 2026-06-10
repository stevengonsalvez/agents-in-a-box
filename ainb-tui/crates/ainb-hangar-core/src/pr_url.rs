//! Parse the PR URL `gh pr create` prints to stdout (P9.1).
//!
//! v1 GitHub integration is `gh`-CLI-only (no App, no webhook): an agent that
//! shells out to `gh pr create` inside its worktree prints the created PR's URL
//! on its own stdout line. The daemon captures the agent's stdout (a bounded
//! ring buffer) and runs [`parse_gh_pr_create_stdout`] over it on completion to
//! stamp `agent_task_queue.result.pr_url` (see [`crate::result::TaskResult`]),
//! so the TUI can surface the PR.
//!
//! # What counts as a match
//!
//! Exactly a *canonical* PR URL on its own (whitespace-trimmed) line:
//! `https://github.com/<owner>/<repo>/pull/<n>`. The pattern is **anchored to
//! the whole line** (`^…$`) on purpose:
//! - a `gh pr list` table row, an issue URL (`/issues/N`), a sub-path
//!   (`/pull/N/files`), or the URL quoted mid-sentence all fail to match, so
//!   prose / error text / other `gh` verbs are never misread as a created PR.
//!
//! # Multi-PR runs
//!
//! When several canonical PR URL lines appear (an agent that opened more than
//! one PR), the **last** one is returned — most-recent intent.

use std::sync::LazyLock;

use regex::Regex;

/// Compiled once: a single canonical `gh pr create` PR-URL line.
///
/// Anchored to the whole line (the caller trims each line before matching).
/// `owner` / `repo` allow GitHub's slug alphabet (alnum plus `-`, `_`, `.`) but
/// never `/`, so the segment count is exactly `owner/repo/pull/<digits>` and a
/// trailing `/files` or `/commits` cannot sneak in.
static PR_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://github\.com/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+/pull/\d+$")
        .expect("PR URL regex is a valid pattern")
});

/// Scan agent stdout for the canonical `gh pr create` PR-URL line, returning the
/// **last** match (most-recent intent on a multi-PR run), or `None` when no line
/// is a canonical PR URL.
///
/// Each line is whitespace-trimmed before matching, so leading/trailing padding
/// `gh` or the shell may add is tolerated; the returned string is the trimmed
/// URL (never an empty string).
#[must_use]
pub fn parse_gh_pr_create_stdout(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .map(str::trim)
        .rfind(|line| PR_URL_RE.is_match(line))
        .map(ToString::to_string)
}
