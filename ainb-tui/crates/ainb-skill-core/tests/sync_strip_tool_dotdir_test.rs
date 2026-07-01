//! Regression: `apply_to_home` / `apply_to_repo` must strip the leading
//! `.<tool>/` segment from the layout's `home` before joining the
//! per-tool `install_root`, since `install_root_for(tool)` already
//! embeds the tool's dotdir (production behavior — and the only
//! behavior the sandbox `AINB_TOOL_HOME_<TOOL>` override produces).
//!
//! Pre-fix: `tool_home.join(home_rel)` with `tool_home =
//! /tmp/sandbox/.claude` and `home_rel = .claude/skills/foo/SKILL.md`
//! produced `/tmp/sandbox/.claude/.claude/skills/foo/SKILL.md` —
//! file-not-found at runtime. Surfaced by sandbox testing:
//! `AINB_TOOL_HOME_CLAUDE=/tmp/sandbox/.claude` + `ainb skill sync
//! --to-repo`.
//!
//! Bead `agents-in-a-box-bsi`.

use std::path::{Path, PathBuf};
use std::process::Command;

use ainb_skill_core::manifest::{SourceEntry, TargetMapping};
use ainb_skill_core::sync::{
    ApplyToRepoOpts, ContentFetcher, FetchError, SyncAction, SyncDirection, apply_to_home,
    apply_to_repo,
};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn git(args: &[&str], cwd: &Path) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "ainb-test")
        .env("GIT_AUTHOR_EMAIL", "ainb-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "ainb-test")
        .env("GIT_COMMITTER_EMAIL", "ainb-test@example.invalid")
        .output()
        .expect("git")
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(&["init", "-b", "main"], dir);
    git(&["config", "user.email", "ainb-test@example.invalid"], dir);
    git(&["config", "user.name", "ainb-test"], dir);
    std::fs::write(dir.join("README.md"), "seed\n").unwrap();
    git(&["add", "README.md"], dir);
    let out = git(&["commit", "-m", "seed"], dir);
    assert!(out.status.success(), "seed commit failed: {out:?}");
}

fn fake_source_with_dotclaude_layout() -> SourceEntry {
    // Mirrors production: `target_layout.home` carries the tool's
    // dotdir prefix (`.claude/skills`) — the convention authors use
    // because the manifest is "relative to $HOME" from a human
    // reading perspective. The executors must strip it.
    SourceEntry {
        name: "src-with-dotdir-layout".into(),
        kind: Some("gh".into()),
        uri: "gh:owner/repo".into(),
        r#ref: "main".into(),
        enabled: true,
        read_only: false,
        target_layout: vec![TargetMapping {
            glob: "skills/*/SKILL.md".into(),
            home: PathBuf::from(".claude/skills"),
            repo: PathBuf::from("skills"),
        }],
    }
}

struct StubFetcher {
    bytes: Vec<u8>,
}

impl ContentFetcher for StubFetcher {
    fn fetch_content(
        &self,
        _ref_name: &str,
        _repo_path: &Path,
    ) -> std::result::Result<Vec<u8>, FetchError> {
        Ok(self.bytes.clone())
    }
}

#[test]
fn apply_to_home_strips_dot_claude_prefix_from_layout_home() {
    // Sandbox-style install_root = /tmp/.../.claude (embeds .claude),
    // matching what `install_root_for("claude")` would return when
    // AINB_TOOL_HOME_CLAUDE is set. The layout's `home: .claude/skills`
    // joined naively against this base would land at
    // `<install_root>/.claude/skills/foo/SKILL.md` — wrong. The
    // executor must strip, producing `<install_root>/skills/foo/SKILL.md`.
    let parent = tempfile::tempdir().expect("tempdir");
    let install_root = parent.path().join(".claude");
    std::fs::create_dir_all(&install_root).expect("mkdir install_root");

    let action = SyncAction {
        unit_name: "commit".into(),
        direction: SyncDirection::ToHome,
        reason: "test".into(),
    };
    let source = fake_source_with_dotclaude_layout();
    let unit_path = PathBuf::from("skills/commit/SKILL.md");
    let fetcher = StubFetcher {
        bytes: b"---\nname: commit\nkind: skill\n---\nbody\n".to_vec(),
    };

    apply_to_home(
        &action,
        &install_root,
        "claude",
        &source,
        &unit_path,
        &fetcher,
    )
    .expect("apply_to_home must succeed when install_root already embeds .claude");

    let expected = install_root.join("skills/commit/SKILL.md");
    let wrong = install_root.join(".claude/skills/commit/SKILL.md");
    assert!(
        expected.exists(),
        "file must land at install_root/skills/commit/SKILL.md (got missing); doubled-prefix bug regressed"
    );
    assert!(
        !wrong.exists(),
        "file must NOT land at install_root/.claude/skills/commit/SKILL.md (doubled prefix)"
    );
    let bytes = std::fs::read(&expected).expect("read landed");
    assert_eq!(bytes, fetcher.bytes, "round-trip bytes");
}

#[test]
fn apply_to_repo_strips_dot_claude_prefix_from_layout_home() {
    // Same sandbox shape as the ToHome test, but on the publish path.
    // The user edited a file at install_root/skills/foo/SKILL.md (the
    // single-prefix path install put it at); apply_to_repo must read
    // from there, NOT from install_root/.claude/skills/foo/SKILL.md.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_SYNC_SKIP_PUSH", "1");

    let parent = tempfile::tempdir().expect("tempdir parent");
    let install_root = parent.path().join(".claude");
    std::fs::create_dir_all(&install_root).expect("mkdir install_root");

    let repo_dir = tempfile::tempdir().expect("tempdir repo");
    init_repo(repo_dir.path());

    // Seed the home-side file at install-root-relative single-prefix path.
    let body = b"---\nname: commit\nkind: skill\n---\nedited\n";
    let home_file = install_root.join("skills/commit/SKILL.md");
    std::fs::create_dir_all(home_file.parent().unwrap()).expect("mkdir home parent");
    std::fs::write(&home_file, body).expect("seed home file");

    // Sanity: the doubled path must NOT exist on disk — if the
    // executor reads from there, it would find nothing and surface
    // the original bug's error message.
    let doubled = install_root.join(".claude/skills/commit/SKILL.md");
    assert!(
        !doubled.exists(),
        "test fixture must not pre-create the doubled path"
    );

    let action = SyncAction {
        unit_name: "commit".into(),
        direction: SyncDirection::ToRepo,
        reason: "test".into(),
    };
    let source = fake_source_with_dotclaude_layout();
    let unit_path = PathBuf::from("skills/commit/SKILL.md");
    let opts = ApplyToRepoOpts {
        repo_cache_dir: repo_dir.path().to_path_buf(),
    };

    let result = apply_to_repo(&action, &install_root, "claude", &source, &unit_path, &opts);
    std::env::remove_var("AINB_SYNC_SKIP_PUSH");

    result.expect("apply_to_repo must read the home file the executor wrote");

    // The repo-side file landed under skills/<name>/ in the cache (repo_rel).
    let landed = repo_dir.path().join("skills/commit/SKILL.md");
    assert!(
        landed.exists(),
        "repo-side file must land under cache_dir/skills/commit/SKILL.md"
    );
    assert_eq!(std::fs::read(&landed).unwrap(), body, "bytes must transit");
}

#[test]
fn apply_to_home_does_not_strip_when_layout_home_has_no_dotdir_prefix() {
    // Negative case: if the manifest author writes `home: skills`
    // (no leading dotdir), nothing should be stripped — strip is
    // shape-driven, not tool-name-driven.
    let install_root = tempfile::tempdir().expect("tempdir");

    let source = SourceEntry {
        name: "dotdir-less".into(),
        kind: Some("gh".into()),
        uri: "gh:owner/repo".into(),
        r#ref: "main".into(),
        enabled: true,
        read_only: false,
        target_layout: vec![TargetMapping {
            glob: "skills/*/SKILL.md".into(),
            home: PathBuf::from("skills"), // no leading dot
            repo: PathBuf::from("skills"),
        }],
    };
    let action = SyncAction {
        unit_name: "commit".into(),
        direction: SyncDirection::ToHome,
        reason: "test".into(),
    };
    let unit_path = PathBuf::from("skills/commit/SKILL.md");
    let fetcher = StubFetcher {
        bytes: b"body".to_vec(),
    };

    apply_to_home(
        &action,
        install_root.path(),
        "claude",
        &source,
        &unit_path,
        &fetcher,
    )
    .expect("apply_to_home must succeed for no-dotdir layout");

    let landed = install_root.path().join("skills/commit/SKILL.md");
    assert!(
        landed.exists(),
        "no-strip case lands at the raw layout home"
    );
}
