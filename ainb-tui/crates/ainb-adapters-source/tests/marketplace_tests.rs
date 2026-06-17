//! MarketplaceAdapter tests — exercises both the bare-array and
//! `{plugins: [...]}` wire shapes.

use ainb_adapters_source::{MarketplaceAdapter, SourceAdapter};

fn write_marketplace(dir: &std::path::Path, body: &str) {
    let f = dir.join(".claude-plugin/marketplace.json");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, body).unwrap();
}

#[test]
fn detect_finds_marketplace_json() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!MarketplaceAdapter::new().detect(dir.path()));
    write_marketplace(dir.path(), "[]");
    assert!(MarketplaceAdapter::new().detect(dir.path()));
}

#[test]
fn parses_bare_array_shape() {
    let dir = tempfile::tempdir().unwrap();
    write_marketplace(
        dir.path(),
        r#"[
            {"name": "reflect", "description": "self improvement"},
            {"name": "commit", "description": "well-formed commits"}
        ]"#,
    );
    let units = MarketplaceAdapter::new().list_units(dir.path()).unwrap();
    assert_eq!(units.len(), 2);
    assert!(units.iter().all(|u| u.kind == "plugin"));
    let reflect = units.iter().find(|u| u.name == "reflect").unwrap();
    assert_eq!(reflect.description.as_deref(), Some("self improvement"));
    assert_eq!(reflect.path, "plugins/reflect");
}

#[test]
fn parses_plugins_object_shape() {
    let dir = tempfile::tempdir().unwrap();
    write_marketplace(
        dir.path(),
        r#"{"plugins": [{"name": "x", "source": {"path": "custom/x"}}]}"#,
    );
    let units = MarketplaceAdapter::new().list_units(dir.path()).unwrap();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].path, "custom/x");
}

#[test]
fn skips_entries_without_name() {
    let dir = tempfile::tempdir().unwrap();
    write_marketplace(
        dir.path(),
        r#"[
            {"name": "ok"},
            {"description": "no name"}
        ]"#,
    );
    let units = MarketplaceAdapter::new().list_units(dir.path()).unwrap();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].name, "ok");
}

#[test]
fn rejects_invalid_json() {
    let dir = tempfile::tempdir().unwrap();
    write_marketplace(dir.path(), "{not valid");
    let err = MarketplaceAdapter::new().list_units(dir.path()).unwrap_err();
    assert!(err.to_string().contains("parsing"), "got: {err}");
}

#[test]
fn resolve_unit_returns_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    write_marketplace(dir.path(), r#"[{"name": "x"}]"#);
    let resolved = MarketplaceAdapter::new().resolve_unit(dir.path(), "plugins/x").unwrap();
    assert_eq!(resolved.descriptor.name, "x");
}
