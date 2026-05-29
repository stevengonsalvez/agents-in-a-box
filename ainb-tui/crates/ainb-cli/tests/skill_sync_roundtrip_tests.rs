//! `ainb skill sync` bidirectional content-sync tripwire — bead v12.D.4.
//!
//! Seeds a git source (file:// bare repo + working clone) with a
//! skill on disk + an installed deployment under a sandboxed tool
//! home. Edits the home file, runs `ainb skill sync --to-repo`, asserts
//! the cache clone has a new `sync: <unit>` commit + bytes match.
//! Then edits the cache clone (the upstream side), runs
//! `ainb skill sync --to-home`, asserts the home file matches the
//! upstream bytes. The roundtrip ships in one test so the directional
//! flags are exercised end-to-end.

use std::path::{Path, PathBuf};
use std::process::Command;

use ainb_cli::{
    AddArgs, Command as Cmd, InstallArgs, SkillCommand, SourceCommand, SyncArgs, dispatch,
};
use ainb_skill_core::manifest::{Manifest, TargetMapping, UnitEntry};
use ainb_skill_core::paths::manifest_path_in;

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

fn init_bare_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let out = git(&["init", "--bare", "-b", "main"], dir);
    assert!(out.status.success(), "git init --bare: {out:?}");
}

fn init_working_clone(remote: &str, dir: &Path) {
    let out = Command::new("git")
        .args(["clone", remote])
        .arg(dir)
        .env("GIT_AUTHOR_NAME", "ainb-test")
        .env("GIT_AUTHOR_EMAIL", "ainb-test@example.invalid")
        .output()
        .expect("git clone");
    assert!(out.status.success(), "git clone: {out:?}");
    git(&["config", "user.email", "ainb-test@example.invalid"], dir);
    git(&["config", "user.name", "ainb-test"], dir);
}

fn seed_skill(working_clone: &Path, body: &[u8]) {
    let p = working_clone.join("skills/commit/SKILL.md");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    let out = git(&["add", "skills/commit/SKILL.md"], working_clone);
    assert!(out.status.success(), "git add: {out:?}");
    let out = git(&["commit", "-m", "seed commit skill"], working_clone);
    assert!(out.status.success(), "seed commit: {out:?}");
    let out = git(&["push", "origin", "main"], working_clone);
    assert!(out.status.success(), "push: {out:?}");
}

fn run(home: &Path, action: SkillCommand) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(home, Cmd::Skill { action }, &mut buf);
    (String::from_utf8(buf).expect("utf8"), res)
}

fn with_skip_push_and_homes<R>(base: &Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    const VARS: &[&str] = &[
        "AINB_TOOL_HOME_CLAUDE",
        "AINB_TOOL_HOME_CODEX",
        "AINB_TOOL_HOME_COPILOT",
        "AINB_TOOL_HOME_GEMINI",
        "AINB_TOOL_HOME_CURSOR",
        "AINB_TOOL_HOME_AMAZONQ",
        "AINB_TOOL_HOME_CLAUDE_DESKTOP",
        "AINB_TOOL_HOME_CLINE",
        "AINB_TOOL_HOME_ROO",
    ];
    for var in VARS {
        let tool = var.trim_start_matches("AINB_TOOL_HOME_").to_lowercase();
        let dir = base.join(&tool);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var(var, &dir);
    }
    std::env::set_var("AINB_SYNC_SKIP_PUSH", "1");
    let r = body();
    std::env::remove_var("AINB_SYNC_SKIP_PUSH");
    for var in VARS {
        std::env::remove_var(var);
    }
    r
}

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("ainb-skill-sync-rt-").tempdir().expect("tempdir")
}

#[test]
fn sync_roundtrip_home_to_repo_and_back() {
    let ainb_home = tmp_home();
    let bare_repo = tempfile::tempdir().unwrap();
    let working = tempfile::tempdir().unwrap();
    let tool_homes = tempfile::tempdir().unwrap();

    init_bare_repo(bare_repo.path());
    let bare_url = format!("file://{}", bare_repo.path().display());
    init_working_clone(&bare_url, working.path());

    let initial = b"---\nname: commit\n---\ninitial body\n";
    seed_skill(working.path(), initial);

    let source_uri = format!("git:{}", bare_url);

    with_skip_push_and_homes(tool_homes.path(), || {
        // 1. Add source + install unit.
        let mut out = Vec::new();
        dispatch(
            ainb_home.path(),
            Cmd::Source {
                action: SourceCommand::Add(AddArgs {
                    uri: source_uri.clone(),
                    name: Some("upstream".into()),
                    kind: None,
                }),
            },
            &mut out,
        )
        .expect("add source");

        // Declare target_layout on the freshly-added source so the
        // SyncPlanner's eligibility check + executors can resolve the
        // skill path. The CLI `source add` flow doesn't populate
        // target_layout for non-marketplace sources yet (bead C.5),
        // so we patch the manifest here.
        let mut manifest = Manifest::load_from(&manifest_path_in(ainb_home.path())).unwrap();
        manifest
            .sources
            .iter_mut()
            .find(|s| s.name == "upstream")
            .unwrap()
            .target_layout = vec![TargetMapping {
            glob: "skills/*/SKILL.md".into(),
            // `home` is relative to the per-tool install root
            // (e.g. ~/.claude); the test wires
            // `AINB_TOOL_HOME_CLAUDE=<tool_homes>/claude` so the file
            // ends up at `<claude>/skills/commit/SKILL.md`.
            home: PathBuf::from("skills"),
            repo: PathBuf::from("skills"),
        }];
        manifest.save_to(&manifest_path_in(ainb_home.path())).unwrap();

        let unit_uri = format!("{source_uri}@main/skills/commit");
        let (_o, res) = run(
            ainb_home.path(),
            SkillCommand::Install(InstallArgs {
                uri: unit_uri.clone(),
                targets: Some("claude".into()),
                dry_run: false,
                yes: true,
            }),
        );
        res.expect("install");

        // Declare the unit in the manifest so the SyncPlanner sees it.
        let mut manifest = Manifest::load_from(&manifest_path_in(ainb_home.path())).unwrap();
        manifest.units.push(UnitEntry {
            uri: unit_uri.clone(),
            targets: Some(vec!["claude".into()]),
            shadowed_by: None,
        });
        manifest.save_to(&manifest_path_in(ainb_home.path())).unwrap();

        // ── 2. Edit home → sync --to-repo → verify commit lands. ──
        let claude_install_root = tool_homes.path().join("claude");
        let home_file = claude_install_root.join("skills/commit/SKILL.md");
        assert!(home_file.exists(), "install must place file: {}", home_file.display());

        let edited = b"---\nname: commit\n---\nhome-edited\n";
        std::fs::write(&home_file, edited).unwrap();

        let (_out, res) = run(
            ainb_home.path(),
            SkillCommand::Sync(SyncArgs {
                yes: true,
                dry_run: false,
                to_repo: true,
                to_home: false,
                source_or_unit: None,
            }),
        );
        res.expect("sync --to-repo");

        // The CLI keeps a clone under
        // `<ainb-home>/promote-cache/<sanitised-remote>/`. Find any
        // single-entry directory + verify HEAD commit message.
        let cache_root = ainb_home.path().join("promote-cache");
        assert!(cache_root.exists(), "promote-cache dir must exist");
        let clones: Vec<_> = std::fs::read_dir(&cache_root)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(clones.len(), 1, "expected one cache clone, got: {clones:?}");
        let clone_dir = clones[0].path();

        let msg_out = git(&["log", "-1", "--pretty=%s"], &clone_dir);
        let msg = String::from_utf8_lossy(&msg_out.stdout).trim().to_string();
        assert_eq!(msg, "sync: commit", "subject must be `sync: commit`");

        let cache_file = clone_dir.join("skills/commit/SKILL.md");
        assert_eq!(
            std::fs::read(&cache_file).unwrap(),
            edited,
            "cache repo bytes must match home edit"
        );

        // ── 3. Edit cache (upstream side) → sync --to-home → verify
        //    home file matches. ──────────────────────────────────
        //
        // Sleep 1.1s so the upstream file's mtime is unambiguously
        // newer than the home file's mtime. Without it, the host
        // filesystem may resolve mtimes at second granularity and
        // the two writes land in the same tick, making the planner
        // tie-break to ToRepo (its "home wins on indeterminate"
        // default) and skipping the ToHome update.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let upstream_edited = b"---\nname: commit\n---\nupstream-edited\n";
        std::fs::write(&cache_file, upstream_edited).unwrap();
        let out = git(&["add", "skills/commit/SKILL.md"], &clone_dir);
        assert!(out.status.success(), "git add upstream edit");
        let out = git(&["commit", "-m", "upstream patch"], &clone_dir);
        assert!(out.status.success(), "git commit upstream patch");
        let out = git(&["push", "origin", "main"], &clone_dir);
        assert!(out.status.success(), "git push upstream patch");

        // Sanity-check the cache state before sync — the test wrote
        // upstream-edited into the cache and committed it, so the
        // file at the cache path should already hold upstream bytes.
        let cache_state = std::fs::read(&cache_file).unwrap();
        assert_eq!(
            cache_state, upstream_edited,
            "cache must already hold upstream bytes before --to-home sync"
        );
        // Home is now older than upstream; sync --to-home should pull.
        let (_sync_out, res) = run(
            ainb_home.path(),
            SkillCommand::Sync(SyncArgs {
                yes: true,
                dry_run: false,
                to_repo: false,
                to_home: true,
                source_or_unit: None,
            }),
        );
        res.expect("sync --to-home");

        let home_after = std::fs::read(&home_file).unwrap();
        assert_eq!(
            home_after, upstream_edited,
            "home must match upstream after --to-home"
        );
    });
}

#[test]
fn sync_dry_run_does_not_mutate() {
    let ainb_home = tmp_home();
    let bare_repo = tempfile::tempdir().unwrap();
    let working = tempfile::tempdir().unwrap();
    let tool_homes = tempfile::tempdir().unwrap();

    init_bare_repo(bare_repo.path());
    let bare_url = format!("file://{}", bare_repo.path().display());
    init_working_clone(&bare_url, working.path());

    let initial = b"body\n";
    seed_skill(working.path(), initial);

    let source_uri = format!("git:{}", bare_url);

    with_skip_push_and_homes(tool_homes.path(), || {
        let mut out = Vec::new();
        dispatch(
            ainb_home.path(),
            Cmd::Source {
                action: SourceCommand::Add(AddArgs {
                    uri: source_uri.clone(),
                    name: Some("dry-upstream".into()),
                    kind: None,
                }),
            },
            &mut out,
        )
        .expect("add source");
        let mut manifest = Manifest::load_from(&manifest_path_in(ainb_home.path())).unwrap();
        manifest
            .sources
            .iter_mut()
            .find(|s| s.name == "dry-upstream")
            .unwrap()
            .target_layout = vec![TargetMapping {
            glob: "skills/*/SKILL.md".into(),
            // `home` is relative to the per-tool install root
            // (e.g. ~/.claude); the test wires
            // `AINB_TOOL_HOME_CLAUDE=<tool_homes>/claude` so the file
            // ends up at `<claude>/skills/commit/SKILL.md`.
            home: PathBuf::from("skills"),
            repo: PathBuf::from("skills"),
        }];
        manifest.save_to(&manifest_path_in(ainb_home.path())).unwrap();
        let unit_uri = format!("{source_uri}@main/skills/commit");
        let (_o, res) = run(
            ainb_home.path(),
            SkillCommand::Install(InstallArgs {
                uri: unit_uri.clone(),
                targets: Some("claude".into()),
                dry_run: false,
                yes: true,
            }),
        );
        res.expect("install");
        let mut manifest = Manifest::load_from(&manifest_path_in(ainb_home.path())).unwrap();
        manifest.units.push(UnitEntry {
            uri: unit_uri.clone(),
            targets: Some(vec!["claude".into()]),
            shadowed_by: None,
        });
        manifest.save_to(&manifest_path_in(ainb_home.path())).unwrap();

        let claude_install_root = tool_homes.path().join("claude");
        let home_file = claude_install_root.join("skills/commit/SKILL.md");
        let edited = b"edited\n";
        std::fs::write(&home_file, edited).unwrap();

        let (out, res) = run(
            ainb_home.path(),
            SkillCommand::Sync(SyncArgs {
                yes: false,
                dry_run: true,
                to_repo: true,
                to_home: false,
                source_or_unit: None,
            }),
        );
        res.expect("dry-run ok");
        assert!(out.contains("dry-run"), "got: {out}");

        // Cache should not exist (nothing committed).
        let cache_root = ainb_home.path().join("promote-cache");
        if cache_root.exists() {
            for entry in std::fs::read_dir(&cache_root).unwrap() {
                let p = entry.unwrap().path();
                if p.is_dir() {
                    let log = git(&["log", "--oneline"], &p);
                    let txt = String::from_utf8_lossy(&log.stdout);
                    assert!(
                        !txt.contains("sync: commit"),
                        "dry-run must not commit; got log:\n{txt}"
                    );
                }
            }
        }
    });
}
