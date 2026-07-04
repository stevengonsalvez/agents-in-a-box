//! `preview_source` / `import_selected` — the Skill Manager's
//! preview-first add flow. Preview must leave manifest + lockfile
//! untouched; import persists the source and installs the selected
//! units to the chosen tools.

use std::path::{Path, PathBuf};

use ainb_cli::source::{import_selected, preview_source};
use ainb_skill_core::lockfile::Lockfile;
use ainb_skill_core::manifest::Manifest;
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ainb-preview-test-")
        .tempdir()
        .expect("tempdir")
}

/// Two-skill local fixture repo.
fn fixture_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    for (name, desc) in [
        ("commit", "well-formed commits"),
        ("review", "review diffs"),
    ] {
        let p: PathBuf = dir.path().join(format!("skills/{name}/SKILL.md"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            format!("---\nname: {name}\ndescription: {desc}\n---\nbody\n"),
        )
        .unwrap();
    }
    let uri = format!("local:{}", dir.path().display());
    (dir, uri)
}

fn unit_paths(preview: &ainb_cli::source::SourcePreview) -> Vec<String> {
    preview.units.iter().map(|u| u.path.clone()).collect()
}

#[test]
fn preview_lists_units_without_persisting() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tmp_home();
    let (_repo, uri) = fixture_repo();

    let preview = preview_source(home.path(), &uri).expect("preview");
    assert_eq!(preview.units.len(), 2, "both skills discovered");
    assert!(!preview.already_added);
    // Frontmatter description survives into the descriptor (drives the
    // picker's insight pane).
    let commit = preview.units.iter().find(|u| u.name == "commit").expect("commit unit");
    assert_eq!(commit.description.as_deref(), Some("well-formed commits"));

    // No manifest / lockfile writes on preview.
    assert!(
        !manifest_path_in(home.path()).exists(),
        "preview must not write the manifest"
    );
    assert!(
        !lockfile_path_in(home.path()).exists(),
        "preview must not write the lockfile"
    );
}

#[test]
fn import_persists_source_and_installs_selection() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tmp_home();
    let (_repo, uri) = fixture_repo();

    // Route the claude adapter into the sandbox.
    let claude_root = home.path().join("tools/claude");
    // SAFETY: ENV_LOCK serialises env mutation across this suite.
    unsafe { std::env::set_var("AINB_TOOL_HOME_CLAUDE", &claude_root) };

    let preview = preview_source(home.path(), &uri).expect("preview");
    let commit_path = unit_paths(&preview)
        .into_iter()
        .find(|p| p.contains("commit"))
        .expect("commit path");

    let mut out = Vec::new();
    let (installed, failed) =
        import_selected(home.path(), &preview, &[commit_path], "claude", &mut out).expect("import");
    assert_eq!(
        (installed, failed),
        (1, 0),
        "{}",
        String::from_utf8_lossy(&out)
    );

    // Source persisted once; selected unit locked; unselected one absent.
    let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert_eq!(manifest.sources.len(), 1);
    // The installed unit must land in manifest.units too — that's what
    // the Skill Manager's Units table renders (regression: lockfile-only
    // installs were invisible in the TUI).
    assert_eq!(manifest.units.len(), 1);
    assert!(manifest.units[0].uri.contains("commit"));
    let lockfile = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
    assert_eq!(lockfile.units.len(), 1);
    assert!(lockfile.units[0].declared_uri.contains("commit"));
    // Deployed file actually landed under the claude root.
    assert!(claude_root.join("skills/commit/SKILL.md").exists());

    unsafe { std::env::remove_var("AINB_TOOL_HOME_CLAUDE") };
}

#[test]
fn import_nothing_selected_is_an_error_and_persists_nothing() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tmp_home();
    let (_repo, uri) = fixture_repo();

    let preview = preview_source(home.path(), &uri).expect("preview");
    let mut out = Vec::new();
    let err = import_selected(home.path(), &preview, &[], "claude", &mut out);
    assert!(err.is_err());
    assert!(!manifest_path_in(home.path()).exists());
}

// Compile-time: SourcePreview is Send — it crosses the TUI's
// spawn_blocking boundary in AsyncAction::SkillPreviewFetch.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<ainb_cli::source::SourcePreview>();
};

#[test]
fn reinstall_with_different_targets_refreshes_manifest_entry() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tmp_home();
    let (_repo, uri) = fixture_repo();

    let claude_root = home.path().join("tools/claude");
    let codex_root = home.path().join("tools/codex");
    // SAFETY: ENV_LOCK serialises env mutation across this suite.
    unsafe {
        std::env::set_var("AINB_TOOL_HOME_CLAUDE", &claude_root);
        std::env::set_var("AINB_TOOL_HOME_CODEX", &codex_root);
    }

    let preview = preview_source(home.path(), &uri).expect("preview");
    let commit_path = unit_paths(&preview)
        .into_iter()
        .find(|p| p.contains("commit"))
        .expect("commit path");

    let mut out = Vec::new();
    import_selected(
        home.path(),
        &preview,
        &[commit_path.clone()],
        "claude",
        &mut out,
    )
    .expect("first import");
    // Re-import the same unit targeting codex only.
    import_selected(home.path(), &preview, &[commit_path], "codex", &mut out)
        .expect("second import");

    let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert_eq!(manifest.units.len(), 1, "still one declaration");
    assert_eq!(
        manifest.units[0].targets.as_deref(),
        Some(&["codex".to_string()][..]),
        "declared targets must follow the latest install"
    );

    unsafe {
        std::env::remove_var("AINB_TOOL_HOME_CLAUDE");
        std::env::remove_var("AINB_TOOL_HOME_CODEX");
    }
}

/// Source identity is the URI, not the derived name slug: a source added
/// earlier under a custom --name is recognised (already_added, name
/// reused — no duplicate entry), and a slug collision with a different
/// URI fails fast at preview time.
#[test]
fn preview_matches_existing_source_by_uri_not_slug() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tmp_home();
    let (_repo, uri) = fixture_repo();

    // Add the repo under a custom name via the normal add path.
    let mut buf = Vec::new();
    ainb_cli::dispatch(
        home.path(),
        ainb_cli::Command::Source {
            action: ainb_cli::SourceCommand::Add(ainb_cli::AddArgs {
                uri: uri.clone(),
                name: Some("mytools".to_string()),
                kind: None,
            }),
        },
        &mut buf,
    )
    .expect("add source");

    let preview = preview_source(home.path(), &uri).expect("preview");
    assert!(
        preview.already_added,
        "same URI under custom name must be recognised"
    );
    assert_eq!(preview.name, "mytools", "existing entry's name reused");

    // Import must not create a duplicate source.
    let claude_root = home.path().join("tools/claude");
    unsafe { std::env::set_var("AINB_TOOL_HOME_CLAUDE", &claude_root) };
    let paths = unit_paths(&preview);
    let mut out = Vec::new();
    import_selected(home.path(), &preview, &paths[..1], "claude", &mut out).expect("import");
    let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert_eq!(manifest.sources.len(), 1, "no duplicate source entry");
    unsafe { std::env::remove_var("AINB_TOOL_HOME_CLAUDE") };
}

/// A fully-failed import must leave no trace: the source records written
/// at the start of import_selected are rolled back when zero units land.
#[test]
fn fully_failed_import_rolls_back_the_source() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tmp_home();
    let (_repo, uri) = fixture_repo();

    let preview = preview_source(home.path(), &uri).expect("preview");
    // Bogus unit path → every install fails; no tool-home env needed.
    let mut out = Vec::new();
    let (installed, failed) = import_selected(
        home.path(),
        &preview,
        &["skills/does-not-exist".to_string()],
        "claude",
        &mut out,
    )
    .expect("import returns Ok with counts");
    assert_eq!((installed, failed), (0, 1));

    let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert!(
        manifest.sources.is_empty(),
        "failed import must not leave the source behind: {:?}",
        manifest.sources
    );
    let lockfile = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
    assert!(lockfile.sources.is_empty(), "lockfile source rolled back");
}

/// remove_source_units: keep_source leaves the source registered (back to
/// preview) with its units gone; without keep_source the source is dropped
/// entirely. Files are torn down either way.
#[test]
fn remove_source_units_keep_vs_drop() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tmp_home();
    let (_repo, uri) = fixture_repo();
    let claude_root = home.path().join("tools/claude");
    unsafe { std::env::set_var("AINB_TOOL_HOME_CLAUDE", &claude_root) };

    // Import both units.
    let preview = preview_source(home.path(), &uri).expect("preview");
    let paths = unit_paths(&preview);
    let mut out = Vec::new();
    import_selected(home.path(), &preview, &paths, "claude", &mut out).expect("import");
    let name = preview.name.clone();

    // Keep-source removal: units + files gone, source remains.
    let removed =
        ainb_cli::source::remove_source_units(home.path(), &name, true, &mut out).expect("remove");
    assert_eq!(removed, 2);
    let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert!(manifest.units.is_empty(), "units removed");
    assert_eq!(manifest.sources.len(), 1, "source kept for re-import");
    assert!(
        !claude_root.join("skills/commit/SKILL.md").exists(),
        "files torn down"
    );

    // Re-import, then drop the source entirely.
    let preview = preview_source(home.path(), &uri).expect("re-preview");
    import_selected(
        home.path(),
        &preview,
        &unit_paths(&preview),
        "claude",
        &mut out,
    )
    .expect("re-import");
    ainb_cli::source::remove_source_units(home.path(), &name, false, &mut out).expect("drop");
    let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert!(manifest.sources.is_empty(), "source dropped");
    assert!(manifest.units.is_empty());

    unsafe { std::env::remove_var("AINB_TOOL_HOME_CLAUDE") };
}

/// remove_source_units must drop DISCOVERED units (manifest-declared but
/// never installed by ainb, so no lockfile entry) — `skill remove` bails
/// on those, so the manifest teardown must not depend on it. Regression:
/// source-remove reported success while the units stayed.
#[test]
fn remove_source_units_drops_discovered_units_without_lockfile() {
    use ainb_skill_core::manifest::{SourceEntry, UnitEntry};
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tmp_home();

    // Hand-author a source + two units with NO lockfile (the discovery /
    // adopt shape). std::fs so no install path runs.
    std::fs::create_dir_all(manifest_path_in(home.path()).parent().unwrap()).unwrap();
    let mut manifest = Manifest::default();
    manifest
        .add_source(SourceEntry {
            name: "mkt".into(),
            kind: Some("marketplace".into()),
            uri: "marketplace:caveman".into(),
            r#ref: "caveman".into(),
            enabled: true,
            read_only: false,
            target_layout: Vec::new(),
        })
        .unwrap();
    for path in ["skills/a", "skills/b"] {
        manifest.units.push(UnitEntry {
            uri: format!("marketplace:caveman@caveman/{path}"),
            targets: Some(vec!["claude".into()]),
            shadowed_by: None,
        });
    }
    manifest.save_to(&manifest_path_in(home.path())).unwrap();

    let mut out = Vec::new();
    let removed =
        ainb_cli::source::remove_source_units(home.path(), "mkt", false, &mut out).unwrap();
    assert_eq!(
        removed,
        2,
        "both discovered units dropped: {}",
        String::from_utf8_lossy(&out)
    );

    let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
    assert!(
        manifest.units.is_empty(),
        "units gone: {:?}",
        manifest.units
    );
    assert!(manifest.sources.is_empty(), "source gone");
}
