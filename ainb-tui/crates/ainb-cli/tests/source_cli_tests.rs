//! Integration tests for `ainb source ...` — call dispatch directly with
//! an explicit tempdir so we never touch the real `$AINB_HOME`. P2's
//! `add` actually fetches, so each test seeds a `local:` fixture and
//! references that path; no test reaches the network.

use std::path::{Path, PathBuf};

use ainb_cli::{AddArgs, Command, NameArg, RemoveArgs, SourceCommand, dispatch};
use ainb_skill_core::lockfile::{DeployedRef, LockedUnit, Lockfile};
use ainb_skill_core::manifest::Manifest;
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("ainb-cli-test-").tempdir().expect("tempdir")
}

fn run(home: &Path, action: SourceCommand) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(home, Command::Source { action }, &mut buf);
    (String::from_utf8(buf).expect("utf8 output"), res)
}

/// Build a tempdir source whose layout matches the raw-convention so
/// RawAdapter picks it up. Returns a `local:` URI string + the
/// fixture dir (kept alive by the caller).
fn raw_fixture(skill_names: &[&str]) -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    for name in skill_names {
        let p: PathBuf = dir.path().join(format!("skills/{name}/SKILL.md"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            format!("---\nname: {name}\ndescription: test\n---\nbody\n"),
        )
        .unwrap();
    }
    (format!("local:{}", dir.path().display()), dir)
}

#[test]
fn add_fetches_local_and_records_unit_count() {
    let home = tmp_home();
    let (uri, _fixture) = raw_fixture(&["alpha", "beta"]);
    let (out, res) = run(
        home.path(),
        SourceCommand::Add(AddArgs {
            uri,
            name: Some("local-src".into()),
            kind: None,
        }),
    );
    res.expect("add ok");
    assert!(out.contains("added source local-src"), "got: {out}");
    assert!(out.contains("(raw)"), "auto-detected kind missing: {out}");
    assert!(out.contains("2 unit(s)"), "unit count missing: {out}");

    let m = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert_eq!(m.sources[0].name, "local-src");
    assert_eq!(m.sources[0].kind.as_deref(), Some("raw"));

    let l = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
    assert_eq!(l.sources.len(), 1);
    let lsrc = &l.sources[0];
    assert!(lsrc.resolved_sha.is_some());
    assert!(lsrc.fetched_path.is_some());
}

#[test]
fn add_with_forced_type_overrides_detection() {
    let home = tmp_home();
    let (uri, _fixture) = raw_fixture(&["x"]);
    let (out, res) = run(
        home.path(),
        SourceCommand::Add(AddArgs {
            uri,
            name: Some("forced".into()),
            kind: Some("raw".into()),
        }),
    );
    res.unwrap();
    assert!(out.contains("(raw)"));
    let m = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert_eq!(m.sources[0].kind.as_deref(), Some("raw"));
}

#[test]
fn add_local_with_ref_records_ref() {
    let home = tmp_home();
    let (uri_base, _fixture) = raw_fixture(&["x"]);
    let uri = format!("{uri_base}@dev");
    let (_, res) = run(
        home.path(),
        SourceCommand::Add(AddArgs {
            uri,
            name: Some("with-ref".into()),
            kind: None,
        }),
    );
    res.unwrap();
    let m = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert_eq!(m.sources[0].r#ref, "dev");
}

#[test]
fn add_rejects_unit_uri_with_path() {
    let home = tmp_home();
    let (_, res) = run(
        home.path(),
        SourceCommand::Add(AddArgs {
            uri: "gh:org/repo@main/skills/foo".into(),
            name: None,
            kind: None,
        }),
    );
    let err = res.unwrap_err().to_string();
    assert!(err.contains("unit URI"), "got: {err}");
}

#[test]
fn add_rejects_missing_local_path() {
    let home = tmp_home();
    let (_, res) = run(
        home.path(),
        SourceCommand::Add(AddArgs {
            uri: "local:/no/such/path/at/all-9f7c1e3".into(),
            name: None,
            kind: None,
        }),
    );
    assert!(res.is_err(), "fetch should fail on missing path");

    // Manifest should NOT have been mutated since fetch failed.
    let m = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert!(m.sources.is_empty());
}

#[test]
fn add_rejects_duplicate_name() {
    let home = tmp_home();
    let (uri, _fixture) = raw_fixture(&["x"]);
    let args = AddArgs {
        uri: uri.clone(),
        name: Some("alpha".into()),
        kind: None,
    };
    run(
        home.path(),
        SourceCommand::Add(AddArgs {
            uri: uri.clone(),
            name: Some("alpha".into()),
            kind: None,
        }),
    )
    .1
    .unwrap();
    let (_, res2) = run(
        home.path(),
        SourceCommand::Add(AddArgs {
            uri: args.uri,
            name: args.name,
            kind: args.kind,
        }),
    );
    let err = res2.unwrap_err().to_string();
    assert!(err.contains("already exists"), "got: {err}");
}

#[test]
fn list_shows_table() {
    let home = tmp_home();
    let (uri_a, _fa) = raw_fixture(&["x"]);
    let (uri_b, _fb) = raw_fixture(&["y"]);

    run(
        home.path(),
        SourceCommand::Add(AddArgs {
            uri: uri_a,
            name: Some("alpha".into()),
            kind: None,
        }),
    )
    .1
    .unwrap();
    run(
        home.path(),
        SourceCommand::Add(AddArgs {
            uri: format!("{uri_b}@dev"),
            name: Some("beta".into()),
            kind: None,
        }),
    )
    .1
    .unwrap();

    let (out, res) = run(home.path(), SourceCommand::List);
    res.unwrap();
    assert!(out.contains("NAME"));
    assert!(out.contains("alpha"));
    assert!(out.contains("beta"));
    assert!(out.contains("raw"));
    assert!(out.contains("dev"));
}

#[test]
fn list_empty_manifest_prints_hint() {
    let home = tmp_home();
    let (out, res) = run(home.path(), SourceCommand::List);
    res.unwrap();
    assert!(out.contains("no sources configured"), "got: {out}");
}

#[test]
fn remove_requires_yes_flag() {
    let home = tmp_home();
    let (uri, _fixture) = raw_fixture(&["x"]);
    run(
        home.path(),
        SourceCommand::Add(AddArgs {
            uri,
            name: Some("alpha".into()),
            kind: None,
        }),
    )
    .1
    .unwrap();
    let (_, res) = run(
        home.path(),
        SourceCommand::Remove(RemoveArgs {
            name: "alpha".into(),
            yes: false,
        }),
    );
    assert!(res.unwrap_err().to_string().contains("--yes"));
    let m = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert_eq!(m.sources.len(), 1);
}

#[test]
fn remove_flags_lockfile_units_pending_uninstall() {
    let home = tmp_home();
    let (uri, fixture) = raw_fixture(&["x"]);
    let source_uri_for_lock = format!("local:{}", fixture.path().display());
    run(
        home.path(),
        SourceCommand::Add(AddArgs {
            uri,
            name: Some("dropme".into()),
            kind: None,
        }),
    )
    .1
    .unwrap();

    // Seed lockfile with one deployed unit from this source.
    let mut deployed = std::collections::BTreeMap::new();
    deployed.insert(
        "claude".to_string(),
        DeployedRef::Deployed {
            path: "~/.claude/skills/x".into(),
            file_hashes: std::collections::BTreeMap::new(),
        },
    );
    let mut lockfile = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
    lockfile.units.push(LockedUnit {
        uri: format!("{source_uri_for_lock}@sha/x"),
        declared_uri: format!("{source_uri_for_lock}@main/x"),
        kind: "skill".into(),
        sha: None,
        deployed,
        usage: Default::default(),
    });
    lockfile.save_to(&lockfile_path_in(home.path())).unwrap();

    let (out, res) = run(
        home.path(),
        SourceCommand::Remove(RemoveArgs {
            name: "dropme".into(),
            yes: true,
        }),
    );
    res.unwrap();
    assert!(out.contains("removed source dropme"));
    assert!(out.contains("1 unit(s)"));

    let lockfile = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
    assert!(matches!(
        lockfile.units[0].deployed.get("claude").unwrap(),
        DeployedRef::PendingUninstall
    ));
}

#[test]
fn enable_disable_toggles_flag() {
    let home = tmp_home();
    let (uri, _fixture) = raw_fixture(&["x"]);
    run(
        home.path(),
        SourceCommand::Add(AddArgs {
            uri,
            name: Some("alpha".into()),
            kind: None,
        }),
    )
    .1
    .unwrap();

    let (_, res) = run(
        home.path(),
        SourceCommand::Disable(NameArg {
            name: "alpha".into(),
        }),
    );
    res.unwrap();
    let m = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert!(!m.sources[0].enabled);

    let (_, res) = run(
        home.path(),
        SourceCommand::Enable(NameArg {
            name: "alpha".into(),
        }),
    );
    res.unwrap();
    let m = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert!(m.sources[0].enabled);
}

#[test]
fn enable_missing_source_errors() {
    let home = tmp_home();
    let (_, res) = run(
        home.path(),
        SourceCommand::Enable(NameArg {
            name: "ghost".into(),
        }),
    );
    assert!(res.unwrap_err().to_string().contains("ghost"));
}
