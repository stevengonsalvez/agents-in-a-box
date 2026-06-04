//! GitLsRemoteBackend integration tests against a real local bare repo.
//!
//! All in-tree drift coverage used MockBackend; the production
//! `GitLsRemoteBackend` had zero end-to-end exercise. This file
//! mirrors the M4 push-integration pattern from
//! `sync_to_repo_tests::apply_to_repo_pushes_to_real_local_bare_remote`:
//! spin up a real bare git repo, point a SourceEntry at it via a
//! `file://` URI, and round-trip `git ls-remote` through the
//! production code path.
//!
//! Bead v12.1.T4 / agents-in-a-box-i2m. Verifies four behaviours
//! the MockBackend cannot prove:
//!
//!   - InSync round-trip — deployed SHA equal to upstream tip
//!     resolves to `DriftStatus::InSync`.
//!   - Outdated round-trip — deployed SHA different from tip
//!     resolves to `DriftStatus::Outdated`.
//!   - argv-smuggle reject — a source URI starting with `-`
//!     short-circuits before invoking git.
//!   - GIT_TERMINAL_PROMPT propagation — an unreachable / bad
//!     URL must NOT hang waiting for a terminal credential prompt;
//!     it must surface a typed error within a hard time budget.
//!
//! Skips when `git` is unavailable on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ainb_skill_core::manifest::{SourceEntry, TargetMapping};
use ainb_skill_core::{
    build_skill_manager_sandbox, DriftBackend, DriftStatus, GitLsRemoteBackend, SandboxTier,
};

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Resolve the bare-remote tip SHA via `git rev-parse main`. Used after
/// the sandbox fixture seeds the bare repo so each drift test knows
/// what SHA counts as "in sync".
fn bare_remote_tip(bare: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "main"])
        .current_dir(bare)
        .output()
        .expect("git rev-parse");
    assert!(out.status.success(), "git rev-parse main on bare: {out:?}");
    let tip = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(tip.len(), 40, "tip SHA expected 40 chars, got `{tip}`");
    tip
}

fn source_with_uri(uri: String) -> SourceEntry {
    SourceEntry {
        name: "drift-int-src".to_string(),
        kind: Some("git".to_string()),
        uri,
        r#ref: "main".to_string(),
        enabled: true,
        read_only: false,
        target_layout: vec![TargetMapping {
            glob: "skills/*/SKILL.md".to_string(),
            home: PathBuf::from(".claude/skills"),
            repo: PathBuf::from("skills"),
        }],
    }
}

#[test]
fn git_ls_remote_backend_reports_in_sync_when_deployed_sha_equals_tip() {
    if !git_available() {
        eprintln!("SKIP: git not on PATH");
        return;
    }

    let scratch = tempfile::tempdir().expect("scratch tempdir");
    let layout = build_skill_manager_sandbox(scratch.path(), SandboxTier::Minimal)
        .expect("sandbox fixture");
    let bare = layout.bare_remote.clone();
    let tip = bare_remote_tip(&bare);
    let source = source_with_uri(format!("git:file://{}", bare.display()));
    let backend = GitLsRemoteBackend::new();

    let status = backend.compare(&source, &tip).expect("compare must succeed");
    assert_eq!(status, DriftStatus::InSync, "deployed == tip => InSync");
}

#[test]
fn git_ls_remote_backend_reports_outdated_when_deployed_sha_differs() {
    if !git_available() {
        eprintln!("SKIP: git not on PATH");
        return;
    }

    let scratch = tempfile::tempdir().expect("scratch tempdir");
    let layout = build_skill_manager_sandbox(scratch.path(), SandboxTier::Minimal)
        .expect("sandbox fixture");
    let bare = layout.bare_remote.clone();
    let tip = bare_remote_tip(&bare);
    let stale = "0000000000000000000000000000000000000000";
    assert_ne!(stale, tip);

    let source = source_with_uri(format!("git:file://{}", bare.display()));
    let backend = GitLsRemoteBackend::new();
    let status = backend.compare(&source, stale).expect("compare must succeed");
    match status {
        DriftStatus::Outdated { behind: _ } => {}
        other => panic!("expected Outdated, got {other:?}"),
    }
}

#[test]
fn git_ls_remote_backend_rejects_argv_smuggled_uri() {
    // Source URI starting with `-` would, without the leading-dash
    // guard, let an attacker turn `git ls-remote -- <repo>` into
    // `git ls-remote --upload-pack=<cmd>` (because the source URL
    // would be parsed as an option after our `--`-stripped prefix).
    // The backend must refuse before invoking git.
    if !git_available() {
        eprintln!("SKIP: git not on PATH");
        return;
    }

    // `git:` strips off, leaving a URI starting with `-` which is
    // the value passed into `git ls-remote` post-`--`. The leading-
    // dash check in source_to_remote_url applies to the RESOLVED
    // URL (after the `git:` prefix is stripped), so we craft the
    // suffix accordingly.
    let source = source_with_uri("git:--upload-pack=/usr/bin/id".to_string());
    let backend = GitLsRemoteBackend::new();
    let err = backend
        .compare(&source, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
        .expect_err("argv-smuggled URI must error");
    let msg = err.to_string();
    assert!(
        msg.contains("argv-smuggled"),
        "expected argv-smuggled rejection, got: {msg}"
    );
}

#[test]
fn git_ls_remote_backend_does_not_hang_on_bad_url_with_terminal_prompt_disabled() {
    // Hard guarantee: an unreachable / bogus URL must surface an
    // error within a tight time budget rather than freeze the
    // process on a credential prompt. This is the regression that
    // bit the live TUI in commit f34d851 (drift poll hanging on
    // git ls-remote against an inaccessible repo, freezing the
    // SkillManager screen until 95s timeout).
    //
    // file:// to a nonexistent path is the simplest "bad URL" git
    // can hit without going to the network; ls-remote will fail
    // immediately. The point is the time bound — if
    // GIT_TERMINAL_PROMPT propagation ever regresses, the bound
    // catches it.
    if !git_available() {
        eprintln!("SKIP: git not on PATH");
        return;
    }

    let source =
        source_with_uri("git:file:///nonexistent/path/to/bogus-repo.git".to_string());
    let backend = GitLsRemoteBackend::new();

    let started = Instant::now();
    let result = backend.compare(&source, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    let elapsed = started.elapsed();

    // Must surface as an error (not hang), and must do so within
    // a generous 10s. A real hang would surface as a 95s+ timeout
    // (the live regression baseline).
    assert!(
        result.is_err(),
        "compare against bogus URL must return an error, got Ok({:?})",
        result.ok()
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "compare against bogus URL took {elapsed:?} — GIT_TERMINAL_PROMPT/GIT_ASKPASS may not be propagating"
    );
}
