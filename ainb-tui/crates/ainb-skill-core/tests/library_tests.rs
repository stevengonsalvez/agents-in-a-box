//! Library (`~/.agents-in-a-box/library.yaml`) load/save round-trip +
//! register-dedup tests — bead ai-lgk. Pure functions over YAML state,
//! no SQLite. Use explicit tempdir paths so the suite never touches the
//! real `$AINB_HOME`.

use ainb_skill_core::library::{library_path_in, Library, OwnedUnit};
use ainb_skill_core::UnitKind;

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ainb-library-test-")
        .tempdir()
        .expect("tempdir")
}

#[test]
fn missing_file_yields_default() {
    let home = tmp_home();
    let path = library_path_in(home.path());
    let lib = Library::load_from(&path).expect("load missing");
    assert_eq!(lib, Library::default());
    assert_eq!(lib.schema_version, 1);
    assert!(lib.owned.is_empty());
}

#[test]
fn library_yaml_roundtrips() {
    let home = tmp_home();
    let path = library_path_in(home.path());
    assert!(!path.exists());

    let mut lib = Library::default();
    lib.owned.push(OwnedUnit {
        name: "my-skill".into(),
        kind: UnitKind::Skill,
        path: ".claude/skills/my-skill".into(),
        created: "2026-06-02T00:00:00+00:00".into(),
        promoted_uri: None,
    });
    lib.owned.push(OwnedUnit {
        name: "deploy".into(),
        kind: UnitKind::Command,
        path: ".codex/commands/deploy.md".into(),
        created: "2026-06-02T01:00:00+00:00".into(),
        promoted_uri: Some("gh:me/deploy@main/commands/deploy".into()),
    });

    lib.save_to(&path).expect("save");
    assert!(path.exists(), "library.yaml written");

    let loaded = Library::load_from(&path).expect("reload");
    assert_eq!(loaded, lib, "round-trip equality");

    // The on-disk text is YAML and stores the tool-home-relative path
    // verbatim (the `path:` field is the contract the TUI reads).
    let text = std::fs::read_to_string(&path).expect("read yaml");
    assert!(text.contains("schema_version: 1"), "schema marker: {text}");
    assert!(text.contains("my-skill"), "owned name persisted: {text}");
    assert!(
        text.contains(".claude/skills/my-skill"),
        "tool-home-relative path persisted: {text}"
    );
}

#[test]
fn library_add_registers_path() {
    let mut lib = Library::default();
    let added = lib.register(OwnedUnit {
        name: "my-skill".into(),
        kind: UnitKind::Skill,
        path: ".claude/skills/my-skill".into(),
        created: "2026-06-02T00:00:00+00:00".into(),
        promoted_uri: None,
    });
    assert!(added, "first register reports inserted");
    assert_eq!(lib.owned.len(), 1);
    assert_eq!(lib.owned[0].name, "my-skill");
}

#[test]
fn library_register_dedups_by_name() {
    let mut lib = Library::default();
    let first = OwnedUnit {
        name: "my-skill".into(),
        kind: UnitKind::Skill,
        path: ".claude/skills/my-skill".into(),
        created: "2026-06-02T00:00:00+00:00".into(),
        promoted_uri: None,
    };
    assert!(lib.register(first.clone()), "first insert");

    // Re-register the same name with an updated path / promoted_uri.
    let updated = OwnedUnit {
        name: "my-skill".into(),
        kind: UnitKind::Skill,
        path: ".codex/skills/my-skill".into(),
        created: "2026-06-02T02:00:00+00:00".into(),
        promoted_uri: Some("gh:me/my-skill@main/skills/my-skill".into()),
    };
    let inserted = lib.register(updated.clone());
    assert!(!inserted, "second register on same name reports update, not insert");
    assert_eq!(lib.owned.len(), 1, "dedup by name — no duplicate row");
    assert_eq!(
        lib.owned[0].path, updated.path,
        "register overwrites the existing entry in place"
    );
    assert_eq!(lib.owned[0].promoted_uri, updated.promoted_uri);
}

#[test]
fn library_get_returns_owned_by_name() {
    let mut lib = Library::default();
    lib.register(OwnedUnit {
        name: "alpha".into(),
        kind: UnitKind::Skill,
        path: ".claude/skills/alpha".into(),
        created: "2026-06-02T00:00:00+00:00".into(),
        promoted_uri: None,
    });
    assert!(lib.get("alpha").is_some(), "found by name");
    assert!(lib.get("missing").is_none(), "absent name is None");
}
