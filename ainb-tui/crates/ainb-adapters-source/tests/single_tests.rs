//! SingleAdapter tests + adapter-priority pick tests.

use ainb_adapters_source::{
    pick_adapter, ManifestAdapter, MarketplaceAdapter, RawAdapter, SingleAdapter, SourceAdapter,
};

#[test]
fn detect_true_for_single_md_in_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("skill.md"), "---\nname: x\n---\n").unwrap();
    assert!(SingleAdapter::new().detect(dir.path()));
}

#[test]
fn detect_true_when_root_is_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("skill.md");
    std::fs::write(&file, "# direct\n").unwrap();
    assert!(SingleAdapter::new().detect(&file));
}

#[test]
fn detect_skips_dot_directories() {
    // A cloned gist will leave a `.git/` next to the one tracked file.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join("skill.md"), "x").unwrap();
    assert!(SingleAdapter::new().detect(dir.path()));
}

#[test]
fn detect_false_when_two_visible_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "").unwrap();
    std::fs::write(dir.path().join("b.md"), "").unwrap();
    assert!(!SingleAdapter::new().detect(dir.path()));
}

#[test]
fn list_units_reads_frontmatter() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("commit.md"),
        "---\nname: commit\ndescription: well-formed\ntags: [git]\n---\nbody\n",
    )
    .unwrap();
    let units = SingleAdapter::new().list_units(dir.path()).unwrap();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].name, "commit");
    assert_eq!(units[0].kind, "skill");
    assert_eq!(units[0].description.as_deref(), Some("well-formed"));
    assert_eq!(units[0].tags, vec!["git"]);
}

#[test]
fn frontmatter_kind_overrides_default() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("agent.md"),
        "---\nname: a\nkind: agent\n---\n",
    )
    .unwrap();
    let units = SingleAdapter::new().list_units(dir.path()).unwrap();
    assert_eq!(units[0].kind, "agent");
}

#[test]
fn json_file_is_treated_as_plugin() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("plugin.json"),
        r#"{"name":"reflect","description":"self"}"#,
    )
    .unwrap();
    let units = SingleAdapter::new().list_units(dir.path()).unwrap();
    assert_eq!(units[0].kind, "plugin");
    assert_eq!(units[0].name, "reflect");
}

#[test]
fn pick_adapter_prefers_marketplace_over_raw() {
    let dir = tempfile::tempdir().unwrap();
    // Has both signatures — marketplace wins.
    std::fs::create_dir(dir.path().join("skills")).unwrap();
    std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
    std::fs::write(
        dir.path().join(".claude-plugin/marketplace.json"),
        "[]",
    )
    .unwrap();
    let picked = pick_adapter(dir.path()).unwrap();
    assert_eq!(picked.name(), "marketplace");
}

#[test]
fn pick_adapter_prefers_manifest_over_raw() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("skills")).unwrap();
    std::fs::write(dir.path().join("manifest.yaml"), "units: []\n").unwrap();
    let picked = pick_adapter(dir.path()).unwrap();
    assert_eq!(picked.name(), "manifest");
}

#[test]
fn pick_adapter_falls_back_to_single() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("just-one.md"), "x").unwrap();
    let picked = pick_adapter(dir.path()).unwrap();
    assert_eq!(picked.name(), "single");
}

#[test]
fn pick_adapter_returns_none_for_unrecognized_structure() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "").unwrap();
    std::fs::write(dir.path().join("b.md"), "").unwrap();
    // Two files, no convention dirs — single declines, raw declines.
    assert!(pick_adapter(dir.path()).is_none());
}

// Ensure unused imports are kept compilable when adding new adapters.
#[allow(dead_code)]
fn ensure_adapters_compile() {
    let _ = MarketplaceAdapter::new();
    let _ = ManifestAdapter::new();
    let _ = RawAdapter::new();
    let _ = SingleAdapter::new();
}
