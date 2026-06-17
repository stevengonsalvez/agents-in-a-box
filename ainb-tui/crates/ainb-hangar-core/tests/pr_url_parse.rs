//! P9.1 unit tests for [`ainb_hangar_core::pr_url::parse_gh_pr_create_stdout`].
//!
//! `gh pr create` prints the created PR's URL on its own line to stdout. The
//! parser scans an agent's captured stdout for a *canonical* PR URL line and
//! returns it so the daemon can stamp `agent_task_queue.result.pr_url`.
//!
//! Contract under test:
//! - canonical `https://github.com/owner/repo/pull/123` line → `Some(url)`
//! - the same URL surrounded by unrelated log lines → `Some(url)`
//! - non-PR `gh` output (e.g. a `gh pr list` table, an issue URL) → `None`
//! - multiple PR URLs → the **last** one (most-recent intent)
//! - empty / whitespace-only input → `None`

use ainb_hangar_core::pr_url::parse_gh_pr_create_stdout;

/// Canonical single-line output: exactly what `gh pr create` prints on success.
#[test]
fn canonical_single_url_line() {
    let stdout = "https://github.com/owner/repo/pull/123\n";
    assert_eq!(
        parse_gh_pr_create_stdout(stdout),
        Some("https://github.com/owner/repo/pull/123".to_string())
    );
}

/// The URL surrounded by the chatty log lines a real agent run interleaves
/// (progress, the `gh` "Creating pull request…" banner, a trailing summary).
#[test]
fn url_surrounded_by_log_lines() {
    let stdout = "\
Creating pull request for feat/x into main in owner/repo
https://github.com/owner/repo/pull/123
done in 1.2s
";
    assert_eq!(
        parse_gh_pr_create_stdout(stdout),
        Some("https://github.com/owner/repo/pull/123".to_string())
    );
}

/// A `gh pr list` table must NOT match: its rows contain a PR number and title
/// but never a bare canonical `…/pull/N` URL line, so the anchored line-regex
/// rejects every row.
#[test]
fn gh_pr_list_table_does_not_match() {
    let stdout = "\
Showing 3 of 3 open pull requests in owner/repo

ID   TITLE                BRANCH        CREATED AT
#123  Add the thing       feat/x        about 1 hour ago
#124  Fix the other       fix/y         about 2 hours ago
#125  Tidy up             chore/z       about 3 hours ago
";
    assert_eq!(parse_gh_pr_create_stdout(stdout), None);
}

/// An *issue* URL (different path segment) must not be mistaken for a PR URL.
#[test]
fn issue_url_does_not_match() {
    let stdout = "https://github.com/owner/repo/issues/123\n";
    assert_eq!(parse_gh_pr_create_stdout(stdout), None);
}

/// A URL embedded mid-line (not on its own line) must not match: the regex is
/// anchored to the whole trimmed line so a sentence quoting the URL is rejected.
/// This keeps prose / error messages from being misread as the created PR.
#[test]
fn url_embedded_in_prose_does_not_match() {
    let stdout = "see https://github.com/owner/repo/pull/123 for details\n";
    assert_eq!(parse_gh_pr_create_stdout(stdout), None);
}

/// Empty and whitespace-only inputs yield `None` (no URL, never `Some("")`).
#[test]
fn empty_input_yields_none() {
    assert_eq!(parse_gh_pr_create_stdout(""), None);
    assert_eq!(parse_gh_pr_create_stdout("   \n\t\n"), None);
}

/// Multi-PR run: when several canonical PR URL lines appear, the parser returns
/// the **last** one (most-recent intent), per the REFACTOR decision.
#[test]
fn multiple_urls_returns_last() {
    let stdout = "\
https://github.com/owner/repo/pull/1
intermediate log
https://github.com/owner/repo/pull/2
https://github.com/owner/repo/pull/3
";
    assert_eq!(
        parse_gh_pr_create_stdout(stdout),
        Some("https://github.com/owner/repo/pull/3".to_string())
    );
}

/// Leading/trailing whitespace around an otherwise-canonical line is tolerated:
/// `gh` / the shell may pad the line, but the URL itself is still the match.
#[test]
fn surrounding_whitespace_on_url_line_is_trimmed() {
    let stdout = "   https://github.com/owner/repo/pull/77   \n";
    assert_eq!(
        parse_gh_pr_create_stdout(stdout),
        Some("https://github.com/owner/repo/pull/77".to_string())
    );
}

/// A hyphen/dot-bearing owner and repo (`my-org`, `my.repo`) are valid GitHub
/// slugs and must match.
#[test]
fn hyphen_and_dot_in_slugs_match() {
    let stdout = "https://github.com/my-org/my.repo/pull/9\n";
    assert_eq!(
        parse_gh_pr_create_stdout(stdout),
        Some("https://github.com/my-org/my.repo/pull/9".to_string())
    );
}

/// A trailing path segment after the PR number (`/files`, `/commits`) is NOT
/// the canonical create output and must be rejected — only the bare PR URL is a
/// "created PR" signal.
#[test]
fn url_with_trailing_segment_does_not_match() {
    let stdout = "https://github.com/owner/repo/pull/123/files\n";
    assert_eq!(parse_gh_pr_create_stdout(stdout), None);
}
