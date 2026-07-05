//! `ainb skill library {list,add,new}` integration tests — bead ai-lgk.
//!
//! The own-skill library is YAML-backed (`library.yaml`), no SQLite.
//! Every test isolates the tool home via `AINB_TOOL_HOME_CLAUDE`
//! pointed at a per-test tempdir, so `library add` / `new` never touch
//! the user's real `~/.claude`. Env mutation is serialised through
//! `ENV_LOCK` because cargo runs tests in parallel within a binary.

use std::fs;
use std::path::{Path, PathBuf};

use ainb_cli::{
    AddArgs, Command as CliCommand, InstallArgs, LibraryCmd, SkillCommand, SourceCommand, dispatch,
};
use ainb_skill_core::library::{Library, library_path_in};
use ainb_skill_core::manifest::{Manifest, SourceEntry, TargetMapping};
use ainb_skill_core::paths::manifest_path_in;

/// Serialises `AINB_TOOL_HOME_CLAUDE` mutation across these tests so a
/// parallel run doesn't trample another test's tool home.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Sandbox {
    _tmp: tempfile::TempDir,
    /// `$AINB_HOME` — where `library.yaml` lands.
    home: PathBuf,
    /// The isolated claude tool home (pointed at by AINB_TOOL_HOME_CLAUDE).
    claude_home: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = tempfile::Builder::new().prefix("ainb-library-cli-").tempdir().expect("tempdir");
        let home = tmp.path().join("ainb");
        fs::create_dir_all(&home).unwrap();
        let claude_home = tmp.path().join("claude-home");
        fs::create_dir_all(&claude_home).unwrap();
        Self {
            _tmp: tmp,
            home,
            claude_home,
        }
    }

    fn library(&self) -> Library {
        Library::load_from(&library_path_in(&self.home)).expect("load library")
    }
}

/// Run any `skill` subcommand against the sandbox with the claude tool
/// home isolated to `sandbox.claude_home`. Returns the captured stdout.
/// Holds `ENV_LOCK` for the duration so the env override is race-free.
fn run_skill(sandbox: &Sandbox, action: SkillCommand) -> (anyhow::Result<()>, String) {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_TOOL_HOME_CLAUDE", &sandbox.claude_home);
    let mut buf: Vec<u8> = Vec::new();
    let res = dispatch(&sandbox.home, CliCommand::Skill { action }, &mut buf);
    std::env::remove_var("AINB_TOOL_HOME_CLAUDE");
    (res, String::from_utf8(buf).expect("utf8 stdout"))
}

/// Run a `skill library` subcommand — thin wrapper over [`run_skill`].
fn run_library(sandbox: &Sandbox, cmd: LibraryCmd) -> (anyhow::Result<()>, String) {
    run_skill(sandbox, SkillCommand::Library { cmd })
}

/// `ainb source add` against the sandbox — no tool-home isolation
/// needed (source add never touches a tool home).
fn add_source(sandbox: &Sandbox, action: SourceCommand) -> anyhow::Result<()> {
    let mut buf = Vec::new();
    dispatch(&sandbox.home, CliCommand::Source { action }, &mut buf)
}

/// Seed a `local:` source fixture with one skill and register it in the
/// sandbox's manifest via `source add`. Returns the fixture dir (kept
/// alive for the caller) and the full unit URI.
fn add_local_skill_source(
    sandbox: &Sandbox,
    source_name: &str,
    skill_name: &str,
) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(format!("skills/{skill_name}/SKILL.md"));
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(
        &p,
        format!("---\nname: {skill_name}\nkind: skill\n---\nHand-authored {skill_name}.\n"),
    )
    .unwrap();
    let local_uri = format!("local:{}", dir.path().display());

    add_source(
        sandbox,
        SourceCommand::Add(AddArgs {
            uri: local_uri.clone(),
            name: Some(source_name.to_string()),
            kind: None,
        }),
    )
    .expect("add local source");

    let unit_uri = format!("{local_uri}@main/skills/{skill_name}");
    (dir, unit_uri)
}

/// Manually append a bare `SourceEntry` to the manifest — used by tests
/// that only need `mark-source`/`unmark-source`'s manifest-membership
/// check, not a real fetchable source.
fn seed_manifest_source(home: &Path, name: &str) {
    let mut manifest = Manifest::load_from(&manifest_path_in(home)).unwrap();
    manifest.sources.push(SourceEntry {
        name: name.to_string(),
        kind: None,
        uri: format!("local:/nonexistent/{name}"),
        r#ref: "main".to_string(),
        enabled: true,
        read_only: false,
        target_layout: Vec::new(),
    });
    manifest.save_to(&manifest_path_in(home)).unwrap();
}

/// Seed an on-disk skill folder under the claude tool home so
/// `library add <path>` has something to ingest.
fn seed_skill_folder(claude_home: &Path, name: &str) -> PathBuf {
    let dir = claude_home.join("skills").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\nkind: skill\n---\nHand-authored {name}.\n"),
    )
    .unwrap();
    dir
}

#[test]
fn library_new_scaffolds_valid_skill_md() {
    let sb = Sandbox::new();
    let (res, out) = run_library(
        &sb,
        LibraryCmd::New {
            name: "my-new-skill".into(),
            tool: None,
        },
    );
    res.expect("library new ok");
    assert!(out.contains("my-new-skill"), "out names the skill: {out}");

    // The scaffolded SKILL.md exists, has frontmatter, and is parseable.
    let skill_md = sb.claude_home.join("skills").join("my-new-skill").join("SKILL.md");
    assert!(skill_md.is_file(), "SKILL.md scaffolded at {skill_md:?}");
    let text = fs::read_to_string(&skill_md).unwrap();
    assert!(text.starts_with("---"), "has frontmatter open: {text}");
    assert!(
        text.contains("name: my-new-skill"),
        "frontmatter name: {text}"
    );
    assert!(
        text.contains("kind: skill"),
        "frontmatter declares kind: {text}"
    );
    // Frontmatter is closed by a second `---`.
    assert_eq!(
        text.matches("---").count(),
        2,
        "frontmatter opened and closed: {text}"
    );

    // It was registered in library.yaml with a tool-home-relative path.
    let lib = sb.library();
    let owned = lib.get("my-new-skill").expect("registered");
    assert_eq!(owned.path, ".claude/skills/my-new-skill");
    assert!(owned.promoted_uri.is_none());
}

#[test]
fn library_add_existing_folder() {
    let sb = Sandbox::new();
    let dir = seed_skill_folder(&sb.claude_home, "adopt-me");

    let (res, out) = run_library(
        &sb,
        LibraryCmd::Add {
            path: dir.clone(),
            tool: None,
        },
    );
    res.expect("library add ok");
    assert!(out.contains("adopt-me"), "out names the skill: {out}");

    let lib = sb.library();
    let owned = lib.get("adopt-me").expect("registered after add");
    assert_eq!(
        owned.path, ".claude/skills/adopt-me",
        "registers the tool-home-relative path"
    );
}

#[test]
fn library_list_shows_owned() {
    let sb = Sandbox::new();
    // Register two via `new` so we have rows to list.
    run_library(
        &sb,
        LibraryCmd::New {
            name: "alpha".into(),
            tool: None,
        },
    )
    .0
    .expect("new alpha");
    run_library(
        &sb,
        LibraryCmd::New {
            name: "beta".into(),
            tool: None,
        },
    )
    .0
    .expect("new beta");

    // Plain list.
    let (res, out) = run_library(&sb, LibraryCmd::List { json: false });
    res.expect("list ok");
    assert!(out.contains("alpha"), "lists alpha: {out}");
    assert!(out.contains("beta"), "lists beta: {out}");

    // JSON list.
    let (res, json) = run_library(&sb, LibraryCmd::List { json: true });
    res.expect("list --json ok");
    let parsed: serde_json::Value = serde_json::from_str(json.trim()).expect("valid json");
    let arr = parsed.as_array().expect("json array");
    assert_eq!(arr.len(), 2, "two owned units: {json}");
    let names: Vec<&str> =
        arr.iter().filter_map(|v| v.get("name").and_then(|n| n.as_str())).collect();
    assert!(
        names.contains(&"alpha") && names.contains(&"beta"),
        "names: {names:?}"
    );
}

#[test]
fn library_add_rejects_outside_tool_home() {
    let sb = Sandbox::new();
    // A folder OUTSIDE the claude tool home — sibling of it, not under it.
    let outside = sb._tmp.path().join("outside-skills").join("rogue");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join("SKILL.md"),
        "---\nname: rogue\nkind: skill\n---\nNot under any tool home.\n",
    )
    .unwrap();

    let (res, _out) = run_library(
        &sb,
        LibraryCmd::Add {
            path: outside.clone(),
            tool: None,
        },
    );
    assert!(
        res.is_err(),
        "add must refuse a path outside the sandbox tool homes"
    );
    let msg = format!("{}", res.unwrap_err());
    assert!(
        msg.to_lowercase().contains("outside") || msg.to_lowercase().contains("tool home"),
        "error names the safety reason: {msg}"
    );

    // Nothing was registered.
    let lib = sb.library();
    assert!(
        lib.get("rogue").is_none(),
        "rejected path is not registered"
    );
}

// ─────────────────────────────────────────────────────────────────────
// `library copy` — resolves ANY configured source's unit and deploys +
// registers it, reusing `install`'s fetch/adapter machinery.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn library_copy_deploys_from_any_source_and_registers() {
    let sb = Sandbox::new();
    let (_src, unit_uri) = add_local_skill_source(&sb, "ext", "borrowed");

    let (res, out) = run_library(
        &sb,
        LibraryCmd::Copy {
            uri: unit_uri.clone(),
            tool: None,
        },
    );
    res.expect("library copy ok");
    assert!(out.contains("borrowed"), "out names the skill: {out}");

    let deployed = sb.claude_home.join("skills").join("borrowed").join("SKILL.md");
    assert!(
        deployed.is_file(),
        "copy deploys under tool home: {deployed:?}"
    );

    let lib = sb.library();
    let owned = lib.get("borrowed").expect("registered after copy");
    assert_eq!(owned.path, ".claude/skills/borrowed");
}

#[test]
fn library_copy_is_idempotent_when_already_deployed() {
    let sb = Sandbox::new();
    let (_src, unit_uri) = add_local_skill_source(&sb, "ext2", "already-there");

    // Pre-place the destination with different bytes — copy must not
    // clobber an existing deployment, only register it.
    let dest = sb.claude_home.join("skills").join("already-there");
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("SKILL.md"), "hand-edited, not the source copy\n").unwrap();

    let (res, out) = run_library(
        &sb,
        LibraryCmd::Copy {
            uri: unit_uri.clone(),
            tool: None,
        },
    );
    res.expect("library copy ok");
    assert!(out.contains("skipping copy"), "out: {out}");
    assert_eq!(
        fs::read_to_string(dest.join("SKILL.md")).unwrap(),
        "hand-edited, not the source copy\n",
        "already-deployed content must be preserved"
    );

    let lib = sb.library();
    assert!(lib.get("already-there").is_some(), "still registered");
}

// ─────────────────────────────────────────────────────────────────────
// `library mark-source` / `unmark-source` — opt a manifest source into
// git-native two-way library sync.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn mark_source_then_unmark_round_trips() {
    let sb = Sandbox::new();
    seed_manifest_source(&sb.home, "my-source");

    let (res, out) = run_library(
        &sb,
        LibraryCmd::MarkSource {
            name: "my-source".into(),
        },
    );
    res.expect("mark-source ok");
    assert!(out.contains("marked"), "out: {out}");
    assert!(sb.library().is_library_source("my-source"));

    // Reload-through-dispatch: a second mark is a no-op, not an error.
    let (res, out) = run_library(
        &sb,
        LibraryCmd::MarkSource {
            name: "my-source".into(),
        },
    );
    res.expect("repeat mark-source ok");
    assert!(out.contains("already"), "out: {out}");

    let (res, out) = run_library(
        &sb,
        LibraryCmd::UnmarkSource {
            name: "my-source".into(),
        },
    );
    res.expect("unmark-source ok");
    assert!(out.contains("unmarked"), "out: {out}");
    assert!(!sb.library().is_library_source("my-source"));
}

#[test]
fn mark_source_rejects_unknown_source() {
    let sb = Sandbox::new();
    let (res, _out) = run_library(
        &sb,
        LibraryCmd::MarkSource {
            name: "ghost".into(),
        },
    );
    assert!(res.is_err(), "unknown source name must bail");
    let msg = format!("{}", res.unwrap_err());
    assert!(
        msg.contains("no source named"),
        "error names the reason: {msg}"
    );
    assert!(!sb.library().is_library_source("ghost"));
}

// ─────────────────────────────────────────────────────────────────────
// `library push` / `pull` — git-native two-way sync, gated on
// `mark-source`.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn push_bails_when_source_not_marked_library() {
    let sb = Sandbox::new();
    seed_manifest_source(&sb.home, "unmarked");

    let (res, _out) = run_library(
        &sb,
        LibraryCmd::Push {
            source: "unmarked".into(),
        },
    );
    assert!(res.is_err(), "push must refuse a non-library source");
    let msg = format!("{}", res.unwrap_err());
    assert!(msg.contains("not a library source"), "error: {msg}");
}

#[test]
fn pull_bails_when_source_not_marked_library() {
    let sb = Sandbox::new();
    seed_manifest_source(&sb.home, "unmarked2");

    let (res, _out) = run_library(
        &sb,
        LibraryCmd::Pull {
            source: "unmarked2".into(),
        },
    );
    assert!(res.is_err(), "pull must refuse a non-library source");
    let msg = format!("{}", res.unwrap_err());
    assert!(msg.contains("not a library source"), "error: {msg}");
}

/// Run `git <args>` in `cwd`, panicking with the captured output on any
/// invocation failure the caller doesn't explicitly check.
fn git(args: &[&str], cwd: &Path) -> std::process::Output {
    std::process::Command::new("git")
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
    fs::create_dir_all(dir).unwrap();
    let out = git(&["init", "--bare", "-b", "main"], dir);
    assert!(out.status.success(), "git init --bare: {out:?}");
}

fn init_working_clone(remote: &str, dir: &Path) {
    let out = std::process::Command::new("git")
        .args(["clone", remote])
        .arg(dir)
        .output()
        .expect("git clone");
    assert!(out.status.success(), "git clone: {out:?}");
    git(&["config", "user.email", "ainb-test@example.invalid"], dir);
    git(&["config", "user.name", "ainb-test"], dir);
}

fn seed_skill_and_push(working_clone: &Path, body: &[u8]) {
    let p = working_clone.join("skills/commit/SKILL.md");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
    assert!(git(&["add", "skills/commit/SKILL.md"], working_clone).status.success());
    assert!(git(&["commit", "-m", "seed commit skill"], working_clone).status.success());
    assert!(git(&["push", "origin", "main"], working_clone).status.success());
}

/// End-to-end: install a unit from a real (local `file://` bare) git
/// source, mark that source as a library source, edit the deployed
/// file, `push` — the edit must land in the source's fetched checkout
/// (same `promote-cache` clone `ainb skill sync` uses) with a commit
/// covering it. Exercises the reused `bidirectional_content_sync`
/// machinery end-to-end, not just the bail path.
#[test]
fn push_publishes_deployed_edit_to_checkout_and_commits() {
    let sb = Sandbox::new();
    let bare_repo = tempfile::tempdir().unwrap();
    let working = tempfile::tempdir().unwrap();

    init_bare_repo(bare_repo.path());
    let bare_url = format!("file://{}", bare_repo.path().display());
    init_working_clone(&bare_url, working.path());
    seed_skill_and_push(working.path(), b"---\nname: commit\n---\ninitial body\n");

    let source_uri = format!("git:{bare_url}");
    add_source(
        &sb,
        SourceCommand::Add(AddArgs {
            uri: source_uri.clone(),
            name: Some("upstream".into()),
            kind: None,
        }),
    )
    .expect("add git source");

    // Declare `target_layout` so the content-sync planner can map the
    // skill's path — mirrors `skill_sync_roundtrip_tests.rs` (the CLI's
    // `source add` doesn't populate this for non-marketplace sources).
    let mut manifest = Manifest::load_from(&manifest_path_in(&sb.home)).unwrap();
    manifest
        .sources
        .iter_mut()
        .find(|s| s.name == "upstream")
        .unwrap()
        .target_layout = vec![TargetMapping {
        glob: "skills/*/SKILL.md".into(),
        home: PathBuf::from("skills"),
        repo: PathBuf::from("skills"),
    }];
    manifest.save_to(&manifest_path_in(&sb.home)).unwrap();

    let unit_uri = format!("{source_uri}@main/skills/commit");
    let (res, _out) = run_skill(
        &sb,
        SkillCommand::Install(InstallArgs {
            uri: unit_uri.clone(),
            targets: Some("claude".into()),
            dry_run: false,
            yes: true,
        }),
    );
    res.expect("install ok");

    let (res, _out) = run_library(
        &sb,
        LibraryCmd::MarkSource {
            name: "upstream".into(),
        },
    );
    res.expect("mark-source ok");

    let home_file = sb.claude_home.join("skills/commit/SKILL.md");
    assert!(
        home_file.is_file(),
        "install must place file: {home_file:?}"
    );
    let edited = b"---\nname: commit\n---\nlibrary-edited\n";
    fs::write(&home_file, edited).unwrap();

    let (res, _out) = run_library(
        &sb,
        LibraryCmd::Push {
            source: "upstream".into(),
        },
    );
    res.expect("push ok");

    // The edit reached the cache checkout (`<AINB_HOME>/promote-cache/<key>/`
    // — the same clone `ainb skill sync` maintains for this source).
    let cache_root = sb.home.join("promote-cache");
    assert!(cache_root.exists(), "promote-cache dir must exist");
    let clones: Vec<_> = fs::read_dir(&cache_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(clones.len(), 1, "expected one cache clone, got: {clones:?}");
    let clone_dir = clones[0].path();
    let cache_file = clone_dir.join("skills/commit/SKILL.md");
    assert_eq!(
        fs::read(&cache_file).unwrap(),
        edited,
        "cache repo bytes must match the library edit"
    );

    // A commit covering the edit exists in the checkout's log — made by
    // the reused content-sync machinery's `apply_to_repo` (subject
    // `sync: commit`), or by `push`'s own catch-all commit otherwise.
    let log = git(&["log", "--oneline"], &clone_dir);
    let log_text = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_text.contains("sync:") || log_text.contains("ainb: sync library edits"),
        "expected a sync commit in the checkout log: {log_text}"
    );
}

/// Finding 1 regression: a `library new` skill is registered in
/// `library.yaml` and deployed under the tool home, but is NEVER
/// installed through the lockfile — `bidirectional_content_sync` only
/// walks `lockfile.units`, so before the fix `push` reported success
/// while publishing nothing for it. Assert it actually lands in the
/// checkout, gets committed, and reaches the bare remote.
#[test]
fn push_stages_owned_library_units_never_installed_through_lockfile() {
    let sb = Sandbox::new();
    let bare_repo = tempfile::tempdir().unwrap();
    let working = tempfile::tempdir().unwrap();

    init_bare_repo(bare_repo.path());
    let bare_url = format!("file://{}", bare_repo.path().display());
    init_working_clone(&bare_url, working.path());
    seed_skill_and_push(working.path(), b"---\nname: commit\n---\ninitial body\n");

    let source_uri = format!("git:{bare_url}");
    add_source(
        &sb,
        SourceCommand::Add(AddArgs {
            uri: source_uri.clone(),
            name: Some("upstream".into()),
            kind: None,
        }),
    )
    .expect("add git source");

    let mut manifest = Manifest::load_from(&manifest_path_in(&sb.home)).unwrap();
    manifest
        .sources
        .iter_mut()
        .find(|s| s.name == "upstream")
        .unwrap()
        .target_layout = vec![TargetMapping {
        glob: "skills/*/SKILL.md".into(),
        home: PathBuf::from("skills"),
        repo: PathBuf::from("skills"),
    }];
    manifest.save_to(&manifest_path_in(&sb.home)).unwrap();

    // `library new` — owned unit, no lockfile entry at all.
    let (res, _out) = run_library(
        &sb,
        LibraryCmd::New {
            name: "home-grown".into(),
            tool: None,
        },
    );
    res.expect("library new ok");
    let owned_content = "---\nname: home-grown\nkind: skill\n---\nauthored locally.\n";
    fs::write(
        sb.claude_home.join("skills/home-grown/SKILL.md"),
        owned_content,
    )
    .unwrap();

    run_library(
        &sb,
        LibraryCmd::MarkSource {
            name: "upstream".into(),
        },
    )
    .0
    .expect("mark-source ok");

    let (res, out) = run_library(
        &sb,
        LibraryCmd::Push {
            source: "upstream".into(),
        },
    );
    res.expect("push ok");
    assert!(
        out.contains("staged 1 owned-library unit"),
        "push reports the staged unit: {out}"
    );

    let cache_root = sb.home.join("promote-cache");
    let clones: Vec<_> = fs::read_dir(&cache_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(clones.len(), 1, "expected one cache clone, got: {clones:?}");
    let clone_dir = clones[0].path();
    let cache_file = clone_dir.join("skills/home-grown/SKILL.md");
    assert!(
        cache_file.is_file(),
        "owned unit staged into checkout at {cache_file:?}"
    );
    assert_eq!(
        fs::read_to_string(&cache_file).unwrap(),
        owned_content,
        "staged bytes match the owned unit's on-disk content"
    );

    let log = git(&["log", "--oneline"], &clone_dir);
    let log_text = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_text.contains("ainb: sync library edits"),
        "expected push's catch-all commit in the checkout log: {log_text}"
    );

    // Reached the bare remote, not just the local cache checkout.
    let verify = tempfile::tempdir().unwrap();
    let out = std::process::Command::new("git")
        .args(["clone", &bare_url])
        .arg(verify.path())
        .output()
        .expect("git clone verify");
    assert!(out.status.success(), "clone verify: {out:?}");
    assert!(
        verify.path().join("skills/home-grown/SKILL.md").is_file(),
        "owned unit pushed all the way to the bare remote"
    );
}

/// Finding 3 regression: `ensure_repo_clone` never checks out
/// `source.ref` (it clones the remote's default branch), and a bare
/// `git push` would push whatever branch happens to be checked out —
/// silently landing on the wrong remote branch for a source whose ref
/// isn't the default. Set up a source pinned to a non-default branch
/// and assert the push lands there, not on the bare repo's `main`.
#[test]
fn push_targets_the_sources_configured_ref_not_the_default_branch() {
    let sb = Sandbox::new();
    let bare_repo = tempfile::tempdir().unwrap();
    let working = tempfile::tempdir().unwrap();

    init_bare_repo(bare_repo.path());
    let bare_url = format!("file://{}", bare_repo.path().display());
    init_working_clone(&bare_url, working.path());
    seed_skill_and_push(working.path(), b"---\nname: commit\n---\ninitial body\n");

    // A second branch, diverging from `main` at the same commit — the
    // source will be pinned to this one instead of the bare repo's
    // default (which `ensure_repo_clone`'s plain `git clone` checks out).
    assert!(git(&["checkout", "-b", "release"], working.path()).status.success());
    assert!(git(&["push", "origin", "release"], working.path()).status.success());

    let source_uri = format!("git:{bare_url}");
    add_source(
        &sb,
        SourceCommand::Add(AddArgs {
            uri: source_uri.clone(),
            name: Some("upstream".into()),
            kind: None,
        }),
    )
    .expect("add git source");

    let mut manifest = Manifest::load_from(&manifest_path_in(&sb.home)).unwrap();
    {
        let entry = manifest.sources.iter_mut().find(|s| s.name == "upstream").unwrap();
        entry.r#ref = "release".to_string();
        entry.target_layout = vec![TargetMapping {
            glob: "skills/*/SKILL.md".into(),
            home: PathBuf::from("skills"),
            repo: PathBuf::from("skills"),
        }];
    }
    manifest.save_to(&manifest_path_in(&sb.home)).unwrap();

    let (res, _out) = run_library(
        &sb,
        LibraryCmd::New {
            name: "release-only".into(),
            tool: None,
        },
    );
    res.expect("library new ok");
    fs::write(
        sb.claude_home.join("skills/release-only/SKILL.md"),
        "---\nname: release-only\nkind: skill\n---\nrelease-branch content.\n",
    )
    .unwrap();

    run_library(
        &sb,
        LibraryCmd::MarkSource {
            name: "upstream".into(),
        },
    )
    .0
    .expect("mark-source ok");
    run_library(
        &sb,
        LibraryCmd::Push {
            source: "upstream".into(),
        },
    )
    .0
    .expect("push ok");

    // Fresh clones verify the *remote* branch tips, not the local cache.
    let verify_main = tempfile::tempdir().unwrap();
    let out = std::process::Command::new("git")
        .args(["clone", "--branch", "main", &bare_url])
        .arg(verify_main.path())
        .output()
        .expect("clone main");
    assert!(out.status.success(), "clone main: {out:?}");
    assert!(
        !verify_main.path().join("skills/release-only/SKILL.md").exists(),
        "push must not touch `main` when the source ref is `release`"
    );

    let verify_release = tempfile::tempdir().unwrap();
    let out = std::process::Command::new("git")
        .args(["clone", "--branch", "release", &bare_url])
        .arg(verify_release.path())
        .output()
        .expect("clone release");
    assert!(out.status.success(), "clone release: {out:?}");
    assert!(
        verify_release.path().join("skills/release-only/SKILL.md").is_file(),
        "push must land on the source's configured ref (`release`)"
    );
}
