//! Manifest load/save round-trip tests — use explicit tempdir paths to
//! avoid touching `$AINB_HOME` and racing with other tests.

use std::path::PathBuf;

use ainb_skill_core::manifest::{Defaults, Manifest, SourceEntry, UnitEntry};
use ainb_skill_core::paths::manifest_path_in;

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ainb-manifest-test-")
        .tempdir()
        .expect("tempdir")
}

#[test]
fn missing_file_yields_default() {
    let home = tmp_home();
    let path = manifest_path_in(home.path());
    let m = Manifest::load_from(&path).expect("load missing");
    assert_eq!(m, Manifest::default());
    assert_eq!(m.schema_version, 1);
    assert!(m.sources.is_empty());
    assert!(m.units.is_empty());
}

#[test]
fn save_creates_parents_and_rounds_trip() {
    let home = tmp_home();
    let path = manifest_path_in(home.path());
    assert!(!path.exists());

    let mut m = Manifest::default();
    m.add_source(SourceEntry {
        name: "toolkit".into(),
        kind: Some("manifest".into()),
        uri: "gh:stevengonsalvez/ai-coder-rules".into(),
        r#ref: "main".into(),
        enabled: true,
    })
    .unwrap();
    m.units.push(UnitEntry {
        uri: "gh:stevengonsalvez/ai-coder-rules@main/toolkit/packages/skills/commit".into(),
        targets: Some(vec!["claude".into(), "codex".into()]),
    });
    m.defaults = Some(Defaults {
        targets: Some(vec!["claude".into()]),
    });

    m.save_to(&path).expect("save");
    assert!(path.exists(), "manifest file written");

    let loaded = Manifest::load_from(&path).expect("reload");
    assert_eq!(loaded, m, "round-trip equality");
}

#[test]
fn add_source_rejects_duplicates() {
    let mut m = Manifest::default();
    let e = SourceEntry {
        name: "toolkit".into(),
        kind: None,
        uri: "gh:org/repo".into(),
        r#ref: "main".into(),
        enabled: true,
    };
    m.add_source(e.clone()).unwrap();
    let err = m.add_source(e).unwrap_err();
    assert!(err.to_string().contains("already exists"), "got: {err}");
}

#[test]
fn remove_source_returns_entry() {
    let mut m = Manifest::default();
    m.add_source(SourceEntry {
        name: "alpha".into(),
        kind: None,
        uri: "gh:a/b".into(),
        r#ref: "main".into(),
        enabled: true,
    })
    .unwrap();
    let removed = m.remove_source("alpha").unwrap();
    assert_eq!(removed.name, "alpha");
    assert!(m.sources.is_empty());

    let err = m.remove_source("missing").unwrap_err();
    assert!(err.to_string().contains("not found"), "got: {err}");
}

#[test]
fn rejects_invalid_yaml() {
    let home = tmp_home();
    let path = manifest_path_in(home.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"not: [valid\n yaml").unwrap();
    let err = Manifest::load_from(&path).unwrap_err();
    assert!(err.to_string().contains("invalid"), "got: {err}");
}

#[test]
fn save_is_atomic_via_tmp_rename() {
    let home = tmp_home();
    let path = manifest_path_in(home.path());
    Manifest::default().save_to(&path).unwrap();

    let tmp = {
        let mut s: PathBuf = path.clone();
        let mut os = s.into_os_string();
        os.push(".tmp");
        s = PathBuf::from(os);
        s
    };
    assert!(!tmp.exists(), "sibling .tmp should be cleaned up after rename");
    assert!(path.exists());
}
