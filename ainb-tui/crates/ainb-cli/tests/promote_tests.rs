//! `ainb skill promote` integration tests — drive the full
//! local-orphan -> git-backed flow against a hermetic bare repo
//! served over `file://`. Per spec §`ainb skill promote — Detailed
//! Design`, the acceptance shape is:
//!
//!   * bare repo receives `skills/<name>/<files>` after promote
//!   * manifest URI is rewritten from `local:` to `git:` / `gh:`
//!   * lockfile records a `Deployed` entry pointing at the original
//!     on-disk path with sha256 `file_hashes`
//!
//! All tests are hermetic — they `git init --bare` a tempdir and use
//! `--to file://<that-tempdir>` so the suite never reaches the
//! network and never invokes the real `gh` CLI. `gh:` URIs are
//! covered by unit tests in `promote.rs::tests`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use ainb_cli::{Command as CliCommand, PromoteArgs, SkillCommand, dispatch};
use ainb_skill_core::lockfile::{DeployedRef, Lockfile};
use ainb_skill_core::manifest::{Manifest, SourceEntry, UnitEntry};
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};

/// Sandbox layout:
/// ```text
///   <tmp>/
///   ├── ainb/                      ← AINB_HOME (manifest + lockfile + cache)
///   ├── upstream.git/              ← bare repo (the "gh: remote")
///   └── sandbox/.claude/skills/    ← orphan unit source dir
///       └── my-skill/SKILL.md
/// ```
struct Sandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    upstream: PathBuf,
    claude_skills: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("ainb-promote-")
            .tempdir()
            .expect("tempdir");
        let home = tmp.path().join("ainb");
        fs::create_dir_all(&home).unwrap();
        let upstream = tmp.path().join("upstream.git");
        let claude_skills = tmp.path().join("sandbox").join(".claude").join("skills");
        fs::create_dir_all(&claude_skills).unwrap();
        Self {
            _tmp: tmp,
            home,
            upstream,
            claude_skills,
        }
    }

    fn upstream_file_uri(&self) -> String {
        format!("file://{}", self.upstream.display())
    }
}

/// Initialise a bare repo with one initial commit on `main` so the
/// promote flow can clone + pull + push against a non-empty remote.
fn init_bare_upstream_with_main(bare_path: &Path) {
    fs::create_dir_all(bare_path).unwrap();
    run_git(bare_path, &["init", "--bare", "--initial-branch=main"]).expect("git init --bare");

    // Bootstrap an initial empty commit via a throwaway working clone.
    let bootstrap_dir =
        tempfile::tempdir().expect("bootstrap tempdir");
    let work = bootstrap_dir.path();
    run_git(
        work,
        &[
            "clone",
            "--quiet",
            &format!("file://{}", bare_path.display()),
            ".",
        ],
    )
    .expect("clone bare");
    set_test_identity(work);
    fs::write(work.join("README.md"), "promote test upstream\n").unwrap();
    run_git(work, &["add", "README.md"]).unwrap();
    // Disable gpg signing for the bootstrap commit so the test does
    // not depend on the user's `commit.gpgsign=true` + a working
    // gpg-agent. The bootstrap commit only exists to seed a HEAD ref
    // on the bare repo — no real authorship is being attested.
    run_git(
        work,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "initial commit",
        ],
    )
    .unwrap();
    run_git(work, &["push", "origin", "main"]).unwrap();
}

fn set_test_identity(work: &Path) {
    run_git(work, &["config", "user.email", "test@example.invalid"]).unwrap();
    run_git(work, &["config", "user.name", "Test"]).unwrap();
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {:?} -> {} {}",
            args,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn run_promote(home: &Path, args: PromoteArgs) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(
        home,
        CliCommand::Skill {
            action: SkillCommand::Promote(args),
        },
        &mut buf,
    );
    (String::from_utf8(buf).expect("utf8"), res)
}

/// Seed `<sandbox>/my-skill/SKILL.md` + a nested file and write a
/// manifest that references it as a `local:` orphan.
fn seed_orphan(sandbox: &Sandbox, unit_name: &str) {
    let unit_dir = sandbox.claude_skills.join(unit_name);
    fs::create_dir_all(&unit_dir).unwrap();
    fs::write(
        unit_dir.join("SKILL.md"),
        format!("---\nname: {unit_name}\n---\n# {unit_name}\nbody\n"),
    )
    .unwrap();
    fs::create_dir_all(unit_dir.join("scripts")).unwrap();
    fs::write(unit_dir.join("scripts").join("helper.sh"), "#!/bin/sh\necho ok\n").unwrap();

    let locator = sandbox.claude_skills.to_string_lossy().to_string();
    let uri = format!("local:{locator}@head/{unit_name}");
    let manifest = Manifest {
        schema_version: 1,
        sources: vec![SourceEntry {
            name: "local-skills".to_string(),
            kind: Some("raw".to_string()),
            uri: format!("local:{locator}"),
            r#ref: "head".to_string(),
            enabled: true,
            read_only: false,
        }],
        units: vec![UnitEntry {
            uri,
            targets: None,
            shadowed_by: None,
        }],
        defaults: None,
        options: None,
    };
    manifest.save_to(&manifest_path_in(&sandbox.home)).unwrap();
}

fn cloned_repo_contains(bare: &Path, expected_file: &str) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let r = run_git(
        dir.path(),
        &[
            "clone",
            "--quiet",
            &format!("file://{}", bare.display()),
            "verify",
        ],
    );
    r.expect("clone bare for verification");
    dir.path().join("verify").join(expected_file).exists()
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[test]
fn promote_local_orphan_to_file_remote_full_roundtrip() {
    let sb = Sandbox::new();
    init_bare_upstream_with_main(&sb.upstream);
    seed_orphan(&sb, "my-skill");

    let (out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "my-skill".to_string(),
            to: format!("{}@main", sb.upstream_file_uri()),
            message: None,
            dry_run: false,
            yes: true,
        },
    );
    res.unwrap_or_else(|e| panic!("promote should succeed; err={e}, output: {out}"));
    assert!(out.contains("promoted my-skill"), "missing summary in: {out}");

    // Bare repo receives the unit.
    assert!(
        cloned_repo_contains(&sb.upstream, "skills/my-skill/SKILL.md"),
        "bare repo missing skills/my-skill/SKILL.md"
    );
    assert!(
        cloned_repo_contains(&sb.upstream, "skills/my-skill/scripts/helper.sh"),
        "bare repo missing nested file"
    );

    // Manifest URI is rewritten to the git: form.
    let manifest = Manifest::load_from(&manifest_path_in(&sb.home)).unwrap();
    let unit = manifest
        .units
        .iter()
        .find(|u| u.uri.contains("my-skill"))
        .expect("unit still present");
    assert!(
        unit.uri.starts_with("git:file://"),
        "expected manifest URI to start with `git:file://`, got: {}",
        unit.uri
    );
    assert!(
        unit.uri.ends_with("@main/skills/my-skill"),
        "expected manifest URI to end with `@main/skills/my-skill`, got: {}",
        unit.uri
    );

    // SourceEntry for the new remote was added.
    let new_source_uri = format!("git:{}", sb.upstream_file_uri());
    assert!(
        manifest.sources.iter().any(|s| s.uri == new_source_uri),
        "manifest missing SourceEntry for new remote `{new_source_uri}`. sources={:?}",
        manifest.sources.iter().map(|s| &s.uri).collect::<Vec<_>>()
    );

    // Lockfile records the deployed instance pointing at the original
    // on-disk path with computed sha256 hashes.
    let lockfile = Lockfile::load_from(&lockfile_path_in(&sb.home)).unwrap();
    let locked = lockfile
        .units
        .iter()
        .find(|u| u.declared_uri == unit.uri)
        .expect("lockfile missing promoted unit");
    let deployed = locked
        .deployed
        .get("claude")
        .expect("expected `claude` tool key in deployed map");
    match deployed {
        DeployedRef::Deployed { path, file_hashes } => {
            // Promote stores the *canonical* orphan dir (defense in
            // depth — see resolve_source_dir). On macOS this means
            // `/var/...` -> `/private/var/...`. Compare via
            // canonicalize so the assertion is platform-portable.
            let expected = sb
                .claude_skills
                .join("my-skill")
                .canonicalize()
                .expect("canonicalize expected path");
            let got = PathBuf::from(path).canonicalize().expect("canonicalize got path");
            assert_eq!(
                got, expected,
                "deployed.path must equal the (canonical) original orphan dir"
            );
            assert!(
                file_hashes.contains_key("SKILL.md"),
                "file_hashes missing SKILL.md; got keys: {:?}",
                file_hashes.keys().collect::<Vec<_>>()
            );
            assert!(
                file_hashes.contains_key("scripts/helper.sh"),
                "file_hashes missing nested entry"
            );
            for v in file_hashes.values() {
                assert!(
                    v.starts_with("sha256:"),
                    "expected sha256: prefix, got {v}"
                );
            }
        }
        other => panic!("expected Deployed, got {other:?}"),
    }
    assert!(
        locked.sha.is_some(),
        "lockfile entry missing commit sha"
    );

    // Original local files preserved (spec requirement: "keep
    // ~/.<tool>/skills/<name>/ files in place").
    assert!(
        sb.claude_skills.join("my-skill").join("SKILL.md").exists(),
        "original orphan SKILL.md must be preserved"
    );
}

#[test]
fn promote_dry_run_does_not_write_or_clone() {
    let sb = Sandbox::new();
    init_bare_upstream_with_main(&sb.upstream);
    seed_orphan(&sb, "my-skill");

    let original_manifest = Manifest::load_from(&manifest_path_in(&sb.home)).unwrap();

    let (out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "my-skill".to_string(),
            to: format!("{}@main", sb.upstream_file_uri()),
            message: None,
            dry_run: true,
            yes: false,
        },
    );
    res.expect("dry-run should succeed without --yes");

    assert!(out.contains("dry-run"), "expected dry-run note: {out}");
    assert!(out.contains("promote my-skill"));

    // Manifest unchanged.
    let after = Manifest::load_from(&manifest_path_in(&sb.home)).unwrap();
    assert_eq!(after, original_manifest, "dry-run must not mutate manifest");

    // Cache dir must not exist.
    assert!(
        !sb.home.join("promote-cache").exists(),
        "dry-run must not create promote-cache"
    );

    // Bare repo still has only the bootstrap README — no skills.
    assert!(
        !cloned_repo_contains(&sb.upstream, "skills/my-skill/SKILL.md"),
        "dry-run must not push"
    );
}

#[test]
fn promote_bails_when_unit_not_in_manifest() {
    let sb = Sandbox::new();
    init_bare_upstream_with_main(&sb.upstream);
    // Empty manifest — no orphan to promote.
    Manifest::default()
        .save_to(&manifest_path_in(&sb.home))
        .unwrap();

    let (_out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "missing".to_string(),
            to: sb.upstream_file_uri(),
            message: None,
            dry_run: false,
            yes: true,
        },
    );
    let err = res.unwrap_err().to_string();
    assert!(err.contains("no unit named"), "got: {err}");
}

#[test]
fn promote_bails_when_unit_already_remote() {
    let sb = Sandbox::new();
    init_bare_upstream_with_main(&sb.upstream);

    let manifest = Manifest {
        schema_version: 1,
        sources: vec![SourceEntry {
            name: "src".to_string(),
            kind: Some("gh".to_string()),
            uri: "gh:foo/bar".to_string(),
            r#ref: "main".to_string(),
            enabled: true,
            read_only: false,
        }],
        units: vec![UnitEntry {
            uri: "gh:foo/bar@main/skills/already-remote".to_string(),
            targets: None,
            shadowed_by: None,
        }],
        defaults: None,
        options: None,
    };
    manifest.save_to(&manifest_path_in(&sb.home)).unwrap();

    let (_out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "already-remote".to_string(),
            to: sb.upstream_file_uri(),
            message: None,
            dry_run: false,
            yes: true,
        },
    );
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("non-local") || err.contains("already on a non-local"),
        "got: {err}"
    );
}

#[test]
fn promote_bails_when_source_dir_missing() {
    let sb = Sandbox::new();
    init_bare_upstream_with_main(&sb.upstream);

    let locator = sb.claude_skills.to_string_lossy().to_string();
    let manifest = Manifest {
        schema_version: 1,
        sources: vec![],
        units: vec![UnitEntry {
            uri: format!("local:{locator}@head/ghost"),
            targets: None,
            shadowed_by: None,
        }],
        defaults: None,
        options: None,
    };
    manifest.save_to(&manifest_path_in(&sb.home)).unwrap();
    // Note: we did NOT create <locator>/ghost on disk.

    let (_out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "ghost".to_string(),
            to: sb.upstream_file_uri(),
            message: None,
            dry_run: false,
            yes: true,
        },
    );
    let err = res.unwrap_err().to_string();
    assert!(err.contains("does not exist"), "got: {err}");
}

#[test]
fn promote_bails_on_reserved_chars_in_unit_name() {
    let sb = Sandbox::new();
    init_bare_upstream_with_main(&sb.upstream);
    Manifest::default()
        .save_to(&manifest_path_in(&sb.home))
        .unwrap();

    let (_out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "../etc/passwd".to_string(),
            to: sb.upstream_file_uri(),
            message: None,
            dry_run: false,
            yes: true,
        },
    );
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("reserved character") || err.contains("path traversal"),
        "got: {err}"
    );
}

#[test]
fn promote_bails_on_bare_traversal_names() {
    let sb = Sandbox::new();
    Manifest::default()
        .save_to(&manifest_path_in(&sb.home))
        .unwrap();
    for name in ["", ".", "..", ".hidden", "evil\\name", "a/b"] {
        let (_out, res) = run_promote(
            &sb.home,
            PromoteArgs {
                unit_name: name.to_string(),
                to: sb.upstream_file_uri(),
                message: None,
                dry_run: false,
                yes: true,
            },
        );
        assert!(
            res.is_err(),
            "expected `{name}` to be rejected by validate_unit_name"
        );
    }
}

#[test]
fn promote_bails_when_source_dir_is_symlink_outside_locator() {
    // Defense in depth: a symlink in the orphan dir that redirects
    // outside the locator must NOT be followed by promote.
    let sb = Sandbox::new();
    init_bare_upstream_with_main(&sb.upstream);

    // Innocent-looking target dir somewhere outside the locator.
    let outside = sb._tmp.path().join("outside-data");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "you shouldn't copy me").unwrap();

    // Create a symlink at <claude_skills>/my-skill -> <outside>.
    let link_at = sb.claude_skills.join("my-skill");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link_at).unwrap();
    #[cfg(not(unix))]
    {
        // Skip on non-Unix: symlink creation requires privilege on Windows.
        eprintln!("skipping symlink test on non-Unix");
        return;
    }

    let locator = sb.claude_skills.to_string_lossy().to_string();
    let manifest = Manifest {
        schema_version: 1,
        sources: vec![],
        units: vec![UnitEntry {
            uri: format!("local:{locator}@head/my-skill"),
            targets: None,
            shadowed_by: None,
        }],
        defaults: None,
        options: None,
    };
    manifest.save_to(&manifest_path_in(&sb.home)).unwrap();

    let (_out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "my-skill".to_string(),
            to: sb.upstream_file_uri(),
            message: None,
            dry_run: false,
            yes: true,
        },
    );
    let err = res.expect_err("symlink redirect must bail").to_string();
    assert!(
        err.contains("symlink or traversal") || err.contains("outside the locator"),
        "got: {err}"
    );
}

#[test]
fn promote_bails_when_remote_has_no_head_ref() {
    // Empty bare repo (no initial commit) → no HEAD symref → bail
    // with a clear "pass --to ...@branch" message instead of dying
    // later inside `git push`.
    let sb = Sandbox::new();
    fs::create_dir_all(&sb.upstream).unwrap();
    run_git(&sb.upstream, &["init", "--bare", "--initial-branch=main"])
        .expect("git init --bare");
    seed_orphan(&sb, "my-skill");

    let (_out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "my-skill".to_string(),
            to: sb.upstream_file_uri(), // no @branch override
            message: None,
            dry_run: false,
            yes: true,
        },
    );
    let err = res.expect_err("empty remote must bail").to_string();
    assert!(
        err.contains("no HEAD ref") || err.contains("@branch"),
        "got: {err}"
    );
}

#[test]
fn promote_bails_on_invalid_to_uri() {
    let sb = Sandbox::new();
    seed_orphan(&sb, "my-skill");

    let (_out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "my-skill".to_string(),
            to: "https://example.com/repo.git".to_string(),
            message: None,
            dry_run: false,
            yes: true,
        },
    );
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("`--to` must be") || err.contains("gh:") || err.contains("file://"),
        "got: {err}"
    );
}

#[test]
fn promote_requires_yes_for_non_dry_run() {
    let sb = Sandbox::new();
    init_bare_upstream_with_main(&sb.upstream);
    seed_orphan(&sb, "my-skill");

    let (_out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "my-skill".to_string(),
            to: sb.upstream_file_uri(),
            message: None,
            dry_run: false,
            yes: false,
        },
    );
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("--yes") || err.contains("interactive"),
        "got: {err}"
    );
}

#[test]
fn promote_custom_message_used_for_commit() {
    let sb = Sandbox::new();
    init_bare_upstream_with_main(&sb.upstream);
    seed_orphan(&sb, "my-skill");

    let (out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "my-skill".to_string(),
            to: format!("{}@main", sb.upstream_file_uri()),
            message: Some("chore(skill): test override".to_string()),
            dry_run: false,
            yes: true,
        },
    );
    res.unwrap_or_else(|e| panic!("promote should succeed; err={e}, output: {out}"));

    // Clone the bare repo and inspect the last commit message.
    let dir = tempfile::tempdir().unwrap();
    let _ = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args([
            "clone",
            "--quiet",
            &format!("file://{}", sb.upstream.display()),
            "verify",
        ])
        .output()
        .unwrap();
    let log_out = Command::new("git")
        .arg("-C")
        .arg(dir.path().join("verify"))
        .args(["log", "-1", "--pretty=%s"])
        .output()
        .unwrap();
    let subject = String::from_utf8_lossy(&log_out.stdout).trim().to_string();
    assert_eq!(subject, "chore(skill): test override");
}

#[test]
fn promote_does_not_rewrite_manifest_when_preflight_fails() {
    // Spec failure mode: "Push fails (network, conflict): commit is
    // locally made; manifest NOT rewritten". Hardest equivalent we
    // can stage hermetically without superuser is a clone failure
    // against a non-existent remote — same invariant: pre-push
    // failure must leave manifest untouched.
    let sb = Sandbox::new();
    // Do NOT init the bare repo. The remote does not exist.
    seed_orphan(&sb, "my-skill");

    let original_manifest = Manifest::load_from(&manifest_path_in(&sb.home)).unwrap();

    let (_out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "my-skill".to_string(),
            // Branch resolution fires `git ls-remote` first → fails
            // because the remote path doesn't exist → we bail before
            // any clone or filesystem write.
            to: sb.upstream_file_uri(),
            message: None,
            dry_run: false,
            yes: true,
        },
    );
    assert!(res.is_err(), "expected failure against non-existent remote");

    // Manifest must be byte-identical to the pre-call state.
    let after = Manifest::load_from(&manifest_path_in(&sb.home)).unwrap();
    assert_eq!(
        after, original_manifest,
        "manifest must NOT be rewritten on pre-push failure"
    );
    // Lockfile must not have a new entry either.
    let lockfile = Lockfile::load_from(&lockfile_path_in(&sb.home)).unwrap();
    assert!(
        lockfile.units.is_empty(),
        "lockfile must remain empty when promote fails before push: {:?}",
        lockfile.units
    );
}

#[test]
fn promote_uses_lockfile_keys_consistently() {
    // Two-step: promote, then re-load lockfile and check the unit URI
    // matches the manifest URI (no drift between the two files).
    let sb = Sandbox::new();
    init_bare_upstream_with_main(&sb.upstream);
    seed_orphan(&sb, "my-skill");

    let (_out, res) = run_promote(
        &sb.home,
        PromoteArgs {
            unit_name: "my-skill".to_string(),
            to: format!("{}@main", sb.upstream_file_uri()),
            message: None,
            dry_run: false,
            yes: true,
        },
    );
    res.expect("promote ok");

    let manifest = Manifest::load_from(&manifest_path_in(&sb.home)).unwrap();
    let lockfile = Lockfile::load_from(&lockfile_path_in(&sb.home)).unwrap();
    let unit = manifest.units.iter().find(|u| u.uri.contains("my-skill")).unwrap();
    let locked = lockfile
        .units
        .iter()
        .find(|u| u.declared_uri == unit.uri)
        .expect("lockfile entry must match manifest URI verbatim");
    // Same string in both places.
    assert_eq!(locked.uri, unit.uri);
    // Old `local:` entry must not linger.
    assert!(
        !lockfile.units.iter().any(|u| u.declared_uri.starts_with("local:")),
        "stale local: entry not cleaned up: {:?}",
        lockfile.units.iter().map(|u| &u.declared_uri).collect::<Vec<_>>()
    );
    // Ensure no duplicate (rewriting + adding shouldn't double-count).
    let count: usize = lockfile
        .units
        .iter()
        .filter(|u| u.declared_uri == unit.uri)
        .count();
    assert_eq!(count, 1, "expected exactly one lockfile row per unit");

    // Sanity: file_hashes wasn't dropped along the way.
    let DeployedRef::Deployed { ref file_hashes, .. } = locked.deployed["claude"] else {
        panic!("expected Deployed");
    };
    let _: &BTreeMap<String, String> = file_hashes;
    assert!(!file_hashes.is_empty(), "file_hashes must be populated");
}
