//! Manifest load/save round-trip tests — use explicit tempdir paths to
//! avoid touching `$AINB_HOME` and racing with other tests.

use std::path::PathBuf;

use ainb_skill_core::manifest::{Defaults, Manifest, SourceEntry, SourceKind, UnitEntry};
use ainb_skill_core::paths::manifest_path_in;
use ainb_skill_core::Uri;

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
        read_only: false,
    })
    .unwrap();
    m.units.push(UnitEntry {
        uri: "gh:stevengonsalvez/ai-coder-rules@main/toolkit/packages/skills/commit".into(),
        targets: Some(vec!["claude".into(), "codex".into()]),
        shadowed_by: None,
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
        read_only: false,
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
        read_only: false,
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

// === hdt.3 / spec §Manifest/lockfile additions ===

const V1_MANIFEST_YAML: &str = r#"
schema_version: 1
sources:
  - name: toolkit
    type: manifest
    uri: gh:stevengonsalvez/ai-coder-rules
    ref: main
    enabled: true
units:
  - uri: gh:stevengonsalvez/ai-coder-rules@main/toolkit/packages/skills/commit
    targets: [claude, codex]
"#;

#[test]
fn v1_manifest_parses_with_default_new_fields() {
    // v1 manifests written without `shadowed_by` / `read_only` / `claude-marketplace`
    // must still load — fields default. Backward compat guarantee.
    let m: Manifest = serde_yaml_ng::from_str(V1_MANIFEST_YAML).expect("v1 parses");
    assert_eq!(m.schema_version, 1);
    assert_eq!(m.sources.len(), 1);
    assert_eq!(m.units.len(), 1);

    let src = &m.sources[0];
    assert_eq!(src.name, "toolkit");
    assert!(
        !src.read_only,
        "read_only must default to false for v1 manifests"
    );

    let unit = &m.units[0];
    assert!(
        unit.shadowed_by.is_none(),
        "shadowed_by must default to None for v1 manifests"
    );
}

#[test]
fn v1_manifest_save_does_not_grow_new_fields_when_defaulted() {
    // Roundtrip: load v1, save, reload — output must not introduce
    // `read_only: false` or `shadowed_by: null` rows where they didn't exist.
    let m: Manifest = serde_yaml_ng::from_str(V1_MANIFEST_YAML).unwrap();
    let dumped = serde_yaml_ng::to_string(&m).unwrap();
    assert!(
        !dumped.contains("read_only"),
        "default `read_only` should be skipped in serialised output, got:\n{dumped}"
    );
    assert!(
        !dumped.contains("shadowed_by"),
        "default `shadowed_by` should be skipped in serialised output, got:\n{dumped}"
    );
}

#[test]
fn source_entry_read_only_roundtrips() {
    let home = tmp_home();
    let path = manifest_path_in(home.path());

    let mut m = Manifest::default();
    m.add_source(SourceEntry {
        name: "claude-plugins-official".into(),
        kind: Some("claude-marketplace".into()),
        uri: "marketplace://claude-plugins-official".into(),
        r#ref: "main".into(),
        enabled: true,
        read_only: true,
    })
    .unwrap();

    m.save_to(&path).unwrap();
    let loaded = Manifest::load_from(&path).unwrap();
    assert_eq!(loaded, m);

    let s = &loaded.sources[0];
    assert!(s.read_only);
    assert_eq!(s.kind.as_deref(), Some("claude-marketplace"));
}

#[test]
fn unit_entry_shadowed_by_roundtrips() {
    let home = tmp_home();
    let path = manifest_path_in(home.path());

    let mut m = Manifest::default();
    m.units.push(UnitEntry {
        uri: "local:~/.claude/skills@head/commit".into(),
        targets: Some(vec!["claude".into()]),
        shadowed_by: Some(
            Uri::parse("marketplace:reflect@claude-plugins-official").unwrap(),
        ),
    });

    m.save_to(&path).unwrap();
    let loaded = Manifest::load_from(&path).unwrap();
    assert_eq!(loaded, m);

    let unit = &loaded.units[0];
    let shadow = unit.shadowed_by.as_ref().expect("shadowed_by present");
    assert_eq!(
        shadow.display(),
        "marketplace:reflect@claude-plugins-official"
    );
}

#[test]
fn source_kind_enum_roundtrips_via_string_field() {
    // SourceKind is the typed view over the free-form `kind: Option<String>`
    // field. Roundtrip every variant — including the new ClaudeMarketplace.
    let cases = [
        (SourceKind::Manifest, "manifest"),
        (SourceKind::Raw, "raw"),
        (SourceKind::Gh, "gh"),
        (SourceKind::Single, "single"),
        (SourceKind::Marketplace, "marketplace"),
        (SourceKind::ClaudeMarketplace, "claude-marketplace"),
    ];
    for (variant, expected) in cases {
        assert_eq!(variant.to_string(), expected, "Display of {variant:?}");
        let parsed: SourceKind = expected.parse().unwrap();
        assert_eq!(parsed, variant, "FromStr of `{expected}`");
    }

    // Unknown strings fall back to `Other` — preserves forward compat with
    // future kinds and arbitrary v1 strings.
    let unknown: SourceKind = "future-kind-xyz".parse().unwrap();
    assert_eq!(unknown, SourceKind::Other("future-kind-xyz".into()));
    assert_eq!(unknown.to_string(), "future-kind-xyz");
}

#[test]
fn source_kind_typed_accessor_on_source_entry() {
    let src = SourceEntry {
        name: "x".into(),
        kind: Some("claude-marketplace".into()),
        uri: "marketplace://x".into(),
        r#ref: "main".into(),
        enabled: true,
        read_only: true,
    };
    assert_eq!(src.kind_typed(), Some(SourceKind::ClaudeMarketplace));

    let src_no_kind = SourceEntry {
        name: "x".into(),
        kind: None,
        uri: "gh:a/b".into(),
        r#ref: "main".into(),
        enabled: true,
        read_only: false,
    };
    assert!(src_no_kind.kind_typed().is_none());
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
    assert!(
        !tmp.exists(),
        "sibling .tmp should be cleaned up after rename"
    );
    assert!(path.exists());
}
