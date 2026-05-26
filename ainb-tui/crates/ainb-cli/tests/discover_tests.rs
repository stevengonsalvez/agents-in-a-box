//! `ainb migrate --discover` integration tests — covers the
//! orchestration path that wires walkers (hdt.1 + hdt.2) into the
//! reconciler (hdt.4) and persists the resulting manifest patch.
//!
//! Each test seeds its own tempdir-backed `AINB_TOOL_HOME_<TOOL>` so
//! the walkers find synthetic fixtures and never read the real
//! `~/.claude`. The shared `ENV_LOCK` mutex mirrors the convention in
//! `migrate_tests.rs` because tests in this binary mutate process
//! env vars in parallel.

use std::path::{Path, PathBuf};

use ainb_cli::{Command, MigrateArgs, dispatch};
use ainb_skill_core::manifest::{Manifest, UnitEntry};
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const ALL_TOOL_ENV_VARS: &[&str] = &[
    "AINB_TOOL_HOME_CLAUDE",
    "AINB_TOOL_HOME_CODEX",
    "AINB_TOOL_HOME_COPILOT",
    "AINB_TOOL_HOME_GEMINI",
    "AINB_TOOL_HOME_CURSOR",
    "AINB_TOOL_HOME_AMAZONQ",
    "AINB_TOOL_HOME_CLAUDE_DESKTOP",
    "AINB_TOOL_HOME_CLINE",
    "AINB_TOOL_HOME_ROO",
];

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ainb-discover-")
        .tempdir()
        .expect("tempdir")
}

fn run_migrate(home: &Path, args: MigrateArgs) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(home, Command::Migrate(args), &mut buf);
    (String::from_utf8(buf).expect("utf8"), res)
}

/// Set every adapter's AINB_TOOL_HOME_<TOOL> env var to a per-tool
/// directory under `base`, run the body, and clean up. Mirrors the
/// helper in `migrate_tests.rs`.
fn with_tool_homes<R>(base: &Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for var in ALL_TOOL_ENV_VARS {
        let tool = var.trim_start_matches("AINB_TOOL_HOME_").to_lowercase();
        let dir = base.join(&tool);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var(var, &dir);
    }
    let r = body();
    for var in ALL_TOOL_ENV_VARS {
        std::env::remove_var(var);
    }
    r
}

/// Seed `<claude_home>/skills/<name>/SKILL.md` with a minimal valid
/// frontmatter block so the class-C walker registers it as an orphan.
fn seed_orphan_skill(claude_home: &Path, name: &str) {
    let dir = claude_home.join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\nkind: skill\n---\nbody\n"),
    )
    .unwrap();
}

/// Seed a marketplace plugin under
/// `<claude_home>/plugins/cache/<mp>/<plugin>/<ver>/` with one skill
/// under `skills/<name>/SKILL.md` so the class-A walker picks it up.
fn seed_marketplace_plugin(
    claude_home: &Path,
    marketplace: &str,
    plugin: &str,
    version: &str,
    skill_name: &str,
) {
    let plugin_root = claude_home
        .join("plugins")
        .join("cache")
        .join(marketplace)
        .join(plugin)
        .join(version);
    let cp_dir = plugin_root.join(".claude-plugin");
    std::fs::create_dir_all(&cp_dir).unwrap();
    std::fs::write(
        cp_dir.join("plugin.json"),
        format!(
            "{{\"name\": \"{plugin}\", \"version\": \"{version}\"}}"
        ),
    )
    .unwrap();
    let skill_dir = plugin_root.join("skills").join(skill_name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {skill_name}\nkind: skill\n---\nbundled\n"),
    )
    .unwrap();
    // Also seed the marketplace registry so class-A emits the real
    // marketplace name rather than `"unknown"`.
    let registry = claude_home.join("plugins").join("known_marketplaces.json");
    std::fs::write(
        registry,
        format!("{{\"{marketplace}\": {{\"url\": \"https://example.test/{marketplace}\"}}}}"),
    )
    .unwrap();
}

// ----------------------------------------------------------------
// Acceptance (a): --dry-run does NOT mutate fs.
// ----------------------------------------------------------------

#[test]
fn discover_dry_run_does_not_mutate_manifest_or_lockfile() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();

    with_tool_homes(base.path(), || {
        let claude = base.path().join("claude");
        seed_orphan_skill(&claude, "commit");

        let manifest_path = manifest_path_in(home.path());
        let lock_path = lockfile_path_in(home.path());
        assert!(!manifest_path.exists(), "precondition: no manifest yet");
        assert!(!lock_path.exists(), "precondition: no lockfile yet");

        let (out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                discover: true,
                dry_run: true,
                ..Default::default()
            },
        );
        res.expect("discover --dry-run ok");

        assert!(out.contains("# migrate --discover"), "got: {out}");
        assert!(out.contains("orphan units discovered: 1"), "got: {out}");
        assert!(out.contains("# dry-run: not writing manifest"), "got: {out}");

        assert!(
            !manifest_path.exists(),
            "dry-run must not create the manifest"
        );
        assert!(
            !lock_path.exists(),
            "dry-run must not create the lockfile"
        );
    });
}

// ----------------------------------------------------------------
// Discover writes manifest correctly when --dry-run is off.
// ----------------------------------------------------------------

#[test]
fn discover_writes_manifest_with_orphan_and_marketplace() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();

    with_tool_homes(base.path(), || {
        let claude = base.path().join("claude");
        seed_orphan_skill(&claude, "my-secret-skill");
        seed_marketplace_plugin(&claude, "claude-plugins-official", "reflect", "v1.0", "summarize");

        let (out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                discover: true,
                ..Default::default()
            },
        );
        res.expect("discover ok");
        assert!(out.contains("# migrate --discover"), "got: {out}");
        assert!(out.contains("marketplace plugins discovered: 1"), "got: {out}");
        assert!(out.contains("orphan units discovered: 1"), "got: {out}");
        assert!(out.contains("new units to add: 2"), "got: {out}");

        let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        let unit_uris: Vec<&str> = manifest.units.iter().map(|u| u.uri.as_str()).collect();
        assert!(
            unit_uris.contains(&"local:~/.claude/skills@head/my-secret-skill"),
            "got units: {unit_uris:?}"
        );
        assert!(
            unit_uris.contains(
                &"marketplace:reflect@claude-plugins-official/skills/summarize"
            ),
            "got units: {unit_uris:?}"
        );

        let source_uris: Vec<&str> = manifest.sources.iter().map(|s| s.uri.as_str()).collect();
        assert!(
            source_uris.contains(&"local:~/.claude/skills"),
            "got sources: {source_uris:?}"
        );
        assert!(
            source_uris.contains(&"marketplace:claude-plugins-official"),
            "got sources: {source_uris:?}"
        );

        let marketplace_src = manifest
            .sources
            .iter()
            .find(|s| s.uri == "marketplace:claude-plugins-official")
            .unwrap();
        assert!(marketplace_src.read_only);
        assert_eq!(marketplace_src.kind.as_deref(), Some("claude-marketplace"));
    });
}

// ----------------------------------------------------------------
// Acceptance (b): --force overrides the empty-manifest guard.
// ----------------------------------------------------------------

#[test]
fn discover_bails_when_manifest_already_has_units_without_force() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();

    with_tool_homes(base.path(), || {
        // Seed a manifest with one pre-existing unit so the guard fires.
        let mut manifest = Manifest::default();
        manifest.units.push(UnitEntry {
            uri: "local:~/.claude/skills@head/already-tracked".to_string(),
            targets: Some(vec!["claude".to_string()]),
            shadowed_by: None,
        });
        manifest.save_to(&manifest_path_in(home.path())).unwrap();

        // Seed a fresh orphan that discovery would want to import.
        let claude = base.path().join("claude");
        seed_orphan_skill(&claude, "new-skill");

        let (_out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                discover: true,
                ..Default::default()
            },
        );
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("already declares") && err.contains("--force"),
            "got: {err}"
        );

        // Manifest still has only the original unit — nothing merged.
        let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        assert_eq!(manifest.units.len(), 1);
        assert_eq!(
            manifest.units[0].uri,
            "local:~/.claude/skills@head/already-tracked"
        );
    });
}

#[test]
fn discover_force_merges_into_existing_manifest() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();

    with_tool_homes(base.path(), || {
        // Seed pre-existing manifest with one unit.
        let mut manifest = Manifest::default();
        manifest.units.push(UnitEntry {
            uri: "local:~/.claude/skills@head/already-tracked".to_string(),
            targets: Some(vec!["claude".to_string()]),
            shadowed_by: None,
        });
        manifest.save_to(&manifest_path_in(home.path())).unwrap();

        // Seed orphan discovery should add.
        let claude = base.path().join("claude");
        seed_orphan_skill(&claude, "freshly-found");

        let (out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                discover: true,
                force: true,
                ..Default::default()
            },
        );
        res.expect("discover --force ok");
        assert!(out.contains("# migrate --discover"), "got: {out}");

        let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        let unit_uris: Vec<&str> = manifest.units.iter().map(|u| u.uri.as_str()).collect();
        assert!(
            unit_uris.contains(&"local:~/.claude/skills@head/already-tracked"),
            "pre-existing unit must survive: {unit_uris:?}"
        );
        // The freshly-discovered orphan must be present too. Note the
        // walker also picks up the `already-tracked/SKILL.md` we never
        // wrote — since we only wrote the manifest entry, no file on
        // disk — so the only discovered orphan is `freshly-found`.
        assert!(
            unit_uris.contains(&"local:~/.claude/skills@head/freshly-found"),
            "newly-discovered unit must be added: {unit_uris:?}"
        );
    });
}

// ----------------------------------------------------------------
// Acceptance (c): --legacy-yaml=<path> name-matches orphans → gh: URI.
// ----------------------------------------------------------------

#[test]
fn legacy_yaml_name_match_rewrites_orphan_to_gh_uri() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();

    with_tool_homes(base.path(), || {
        let claude = base.path().join("claude");
        // Two orphans: `commit` matches the YAML, `random-one` doesn't.
        seed_orphan_skill(&claude, "commit");
        seed_orphan_skill(&claude, "random-one");

        // Stage an external-dependencies.yaml fixture.
        let yaml_path = base.path().join("toolkit-external-deps.yaml");
        std::fs::write(
            &yaml_path,
            r#"
version: "1.0.0"
bundled-skills:
  - name: commit
    repo: stevie/dotfiles
    ref: main
    path: skills/commit
agent-skills: []
"#,
        )
        .unwrap();

        let (out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                discover: true,
                legacy_yaml: Some(yaml_path),
                ..Default::default()
            },
        );
        res.expect("discover --legacy-yaml ok");
        assert!(
            out.contains("legacy-yaml matches: 1"),
            "expected 1 match in: {out}"
        );

        let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        let unit_uris: Vec<&str> = manifest.units.iter().map(|u| u.uri.as_str()).collect();
        assert!(
            unit_uris.contains(&"gh:stevie/dotfiles@main/skills/commit"),
            "matched orphan must become a gh: URI: {unit_uris:?}"
        );
        assert!(
            !unit_uris.contains(&"local:~/.claude/skills@head/commit"),
            "matched orphan's old local: URI must be removed: {unit_uris:?}"
        );
        assert!(
            unit_uris.contains(&"local:~/.claude/skills@head/random-one"),
            "non-matching orphan must stay local:, got: {unit_uris:?}"
        );

        // gh: source registered with the right kind.
        let gh_src = manifest
            .sources
            .iter()
            .find(|s| s.uri == "gh:stevie/dotfiles")
            .expect("gh: source must be added");
        assert_eq!(gh_src.kind.as_deref(), Some("gh"));
        assert_eq!(gh_src.r#ref, "main");
    });
}

// ----------------------------------------------------------------
// Acceptance (d): v1 migrate --from-bootstrap / --check / --clean
// still work — smoke tests via dispatch with the new MigrateArgs
// shape ensure we didn't regress the existing CLI surface.
// ----------------------------------------------------------------

#[test]
fn v1_migrate_check_smoke_still_green() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let (out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                check: true,
                ..Default::default()
            },
        );
        res.expect("check ok");
        assert!(out.contains("# migrate --check"), "got: {out}");
        assert!(out.contains("# total: 0 unit(s)"), "got: {out}");
    });
}

#[test]
fn v1_migrate_from_bootstrap_smoke_still_green() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();

    let toolkit = tempfile::tempdir().unwrap();
    let skill_dir = toolkit.path().join("packages/skills/commit");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: commit\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(
        toolkit.path().join("external-dependencies.yaml"),
        r#"
version: "1.0.0"
bundled-skills:
  - name: commit
    path: packages/skills/commit
    purpose: "commit messages"
agent-skills: []
"#,
    )
    .unwrap();

    with_tool_homes(base.path(), || {
        let (out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                from_bootstrap: true,
                toolkit_root: Some(PathBuf::from(toolkit.path())),
                ..Default::default()
            },
        );
        res.expect("from-bootstrap ok");
        assert!(out.contains("# migrate --from-bootstrap"), "got: {out}");
        assert!(out.contains("unit entries added: 1"), "got: {out}");
        let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        assert!(manifest.sources.iter().any(|s| s.name == "toolkit"));
        assert_eq!(manifest.units.len(), 1);
    });
}

// ----------------------------------------------------------------
// Defensive: discover with no candidates at all writes an empty patch
// (no error, no sources, no units added).
// ----------------------------------------------------------------

#[test]
fn discover_with_no_candidates_yields_empty_patch_and_writes_clean_manifest() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let (out, res) = run_migrate(
            home.path(),
            MigrateArgs {
                discover: true,
                ..Default::default()
            },
        );
        res.expect("discover with no candidates ok");
        assert!(out.contains("marketplace plugins discovered: 0"), "got: {out}");
        assert!(out.contains("orphan units discovered: 0"), "got: {out}");
        assert!(out.contains("new units to add: 0"), "got: {out}");

        // Manifest exists (we wrote an empty one) but has no units.
        let manifest = Manifest::load_from(&manifest_path_in(home.path())).unwrap();
        assert!(manifest.units.is_empty());
    });
}
