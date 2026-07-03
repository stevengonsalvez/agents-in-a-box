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

fn _assert_send(_: impl Send) {}
fn _types(p: ainb_cli::source::SourcePreview, home: &Path) {
    // Compile-time: preview is Send (safe to move across the TUI's async
    // action boundary later) and paths derive from `home`.
    _assert_send(p);
    let _ = home;
}
