// P7: LayoutPersist::save then load round-trips a snapshot.

use ainb::ui::bsp::{LayoutNode, LayoutSnapshot};
use ainb::ui::bsp_persist::{LayoutPersist, PersistError, CURRENT_SCHEMA_VERSION};

fn snap() -> LayoutSnapshot {
    LayoutSnapshot {
        version: CURRENT_SCHEMA_VERSION,
        root: LayoutNode::split_h(
            0.4,
            LayoutNode::pane("session_list").focused(true),
            LayoutNode::pane("live_logs_stream"),
        ),
        min_cols: 30,
        min_rows: 10,
    }
}

#[test]
fn save_then_load_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("layout.json");
    let wrote = LayoutPersist::save(&snap(), &path).expect("save");
    assert!(wrote > 0);
    let loaded = LayoutPersist::load(&path).expect("load").expect("Some");
    assert_eq!(loaded, snap());
}

#[test]
fn load_missing_file_returns_ok_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("does-not-exist.json");
    let loaded = LayoutPersist::load(&path).expect("load");
    assert!(loaded.is_none());
}

#[test]
fn load_v99_returns_schema_mismatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("layout.json");
    let bad = r#"{"version":99,"root":{"kind":"pane","id":"x","focused":false},"min_cols":30,"min_rows":10}"#;
    std::fs::write(&path, bad).expect("write");
    let err = LayoutPersist::load(&path).expect_err("must error");
    assert!(matches!(err, PersistError::SchemaMismatch { got: 99 }));
}

#[test]
fn save_creates_parent_directories() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested/dirs/layout.json");
    LayoutPersist::save(&snap(), &path).expect("save");
    assert!(path.exists(), "save must mkdir -p the parent chain");
}

#[test]
fn save_is_atomic_no_tmp_left_on_disk_after_success() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("layout.json");
    LayoutPersist::save(&snap(), &path).expect("save");
    // *.tmp sibling must not exist after a successful rename
    let tmp = dir.path().join("layout.json.tmp");
    assert!(!tmp.exists(), "tmp file should not survive a successful save");
}
