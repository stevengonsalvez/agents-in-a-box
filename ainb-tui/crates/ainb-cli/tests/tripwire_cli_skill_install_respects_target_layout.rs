//! Tripwire — bead v12.C.4.
//!
//! `ainb skill install` must honour `SourceEntry.target_layout`: the file
//! lands at the mapped `home` path, **not** at the legacy
//! `<tool-home>/<unit.descriptor.path>` location the convention adapter
//! emits when no mapping is present.
//!
//! Scenario:
//!   - Manifest source declares an explicit `target_layout` that maps
//!     `skills/**` to `home: .claude/skills`. Sandbox tool home for
//!     `claude` is pointed at a tempdir via `AINB_TOOL_HOME_CLAUDE`.
//!   - The file should land at `<claude-home>/skills/commit/SKILL.md`
//!     (the `.claude/` prefix is consumed by the env override — see
//!     `read_root_for` / `install_root_for`).
//!   - The legacy path it would land at WITHOUT the mapping is the same
//!     in this case (`<claude-home>/skills/commit/SKILL.md`), so the
//!     tripwire also asserts that when `target_layout` re-roots the
//!     destination explicitly to `.claude/agents`, install ends up at
//!     `<claude-home>/agents/commit/SKILL.md` instead.
//!
//! Refers to the salvaged-design note in the leader brief: the resolved
//! `home` embeds the tool dotdir (`.claude/...`), and `install_root_for`
//! already returns the tool home — so the join must strip the leading
//! `.<tool>/` segment to avoid double-counting.

use std::path::{Path, PathBuf};

use ainb_cli::{AddArgs, Command, InstallArgs, SkillCommand, SourceCommand, dispatch};
use ainb_skill_core::manifest::{Manifest, TargetMapping};
use ainb_skill_core::paths::manifest_path_in;

/// Serialises env mutation so cargo's parallel-in-process test runner
/// doesn't have two tripwires fighting over `AINB_TOOL_HOME_CLAUDE`.
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
        .prefix("ainb-tripwire-skill-install-target-layout-")
        .tempdir()
        .expect("tempdir")
}

/// Seed a `local:` source with one `skills/commit/SKILL.md` and add it
/// to the manifest, then mutate the persisted source entry to carry
/// the supplied `target_layout`. Returns the unit URI and the fixture
/// dir guard.
fn seed_source_with_layout(
    home: &Path,
    name: &str,
    layout: Vec<TargetMapping>,
) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p: PathBuf = dir.path().join("skills/commit/SKILL.md");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        "---\nname: commit\ndescription: well-formed commits\n---\nbody\n",
    )
    .unwrap();
    let local_uri = format!("local:{}", dir.path().display());

    // `source add` registers the source + fetches it into the cache.
    let mut buf = Vec::new();
    dispatch(
        home,
        Command::Source {
            action: SourceCommand::Add(AddArgs {
                uri: local_uri.clone(),
                name: Some(name.to_string()),
                kind: None,
            }),
        },
        &mut buf,
    )
    .expect("add source");

    // Patch the persisted source to carry the explicit target_layout.
    // (Source add always writes an empty layout — see
    // `ainb-cli/src/source.rs::add`.)
    let manifest_path = manifest_path_in(home);
    let mut m = Manifest::load_from(&manifest_path).unwrap();
    let s = m.sources.iter_mut().find(|s| s.name == name).expect("source present");
    s.target_layout = layout;
    m.save_to(&manifest_path).unwrap();

    let unit_uri = format!("{local_uri}@main/skills/commit");
    (dir, unit_uri)
}

fn with_claude_home<R>(p: &Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for var in ALL_TOOL_ENV_VARS {
        std::env::remove_var(var);
    }
    std::env::set_var("AINB_TOOL_HOME_CLAUDE", p);
    let r = body();
    for var in ALL_TOOL_ENV_VARS {
        std::env::remove_var(var);
    }
    r
}

#[test]
fn install_remaps_destination_via_explicit_target_layout() {
    // Source declares `skills/**` → `.claude/agents` so the install
    // path is NOT the convention default of `<tool-home>/skills/...`.
    // If install honours `target_layout`, the file lands under
    // `<claude-home>/agents/commit/SKILL.md`. If it doesn't (the
    // legacy convention path), the file lands at
    // `<claude-home>/skills/commit/SKILL.md` — which is what this
    // tripwire detects.
    let home = tmp_home();
    let layout = vec![TargetMapping {
        glob: "skills/**".to_string(),
        home: PathBuf::from(".claude/agents"),
        repo: PathBuf::from("skills"),
    }];
    let (_src, unit_uri) = seed_source_with_layout(home.path(), "fixture", layout);

    let claude_dst = tempfile::tempdir().unwrap();

    with_claude_home(claude_dst.path(), || {
        let mut buf = Vec::new();
        dispatch(
            home.path(),
            Command::Skill {
                action: SkillCommand::Install(InstallArgs {
                    uri: unit_uri.clone(),
                    targets: Some("claude".into()),
                    dry_run: false,
                    yes: true,
                }),
            },
            &mut buf,
        )
        .expect("install ok");

        // Honoured: target_layout re-roots `skills/**` → `.claude/agents`.
        let mapped = claude_dst.path().join("agents/commit/SKILL.md");
        // Legacy convention path (what the bug would produce).
        let legacy = claude_dst.path().join("skills/commit/SKILL.md");

        assert!(
            mapped.exists(),
            "expected file at mapped path {mapped:?} (target_layout `skills/** → .claude/agents`); \
             install_root must honour SourceEntry.target_layout"
        );
        assert!(
            !legacy.exists(),
            "must NOT write to legacy convention path {legacy:?} when target_layout overrides it"
        );
    });
}

#[test]
fn install_uses_bootstrap_default_when_target_layout_empty() {
    // No explicit layout → resolve_pair falls back to
    // BOOTSTRAP_DEFAULT_MAPPINGS which maps `skills/*/SKILL.md` to
    // `.claude/skills`. After stripping `.claude/`, the file should
    // still land at `<claude-home>/skills/commit/SKILL.md`. This
    // documents the path-equivalence baseline so a future regression
    // that breaks the strip + join doesn't silently pass.
    let home = tmp_home();
    let (_src, unit_uri) = seed_source_with_layout(home.path(), "fixture-default", Vec::new());

    let claude_dst = tempfile::tempdir().unwrap();

    with_claude_home(claude_dst.path(), || {
        let mut buf = Vec::new();
        dispatch(
            home.path(),
            Command::Skill {
                action: SkillCommand::Install(InstallArgs {
                    uri: unit_uri.clone(),
                    targets: Some("claude".into()),
                    dry_run: false,
                    yes: true,
                }),
            },
            &mut buf,
        )
        .expect("install ok");

        let path = claude_dst.path().join("skills/commit/SKILL.md");
        assert!(
            path.exists(),
            "bootstrap default `skills/*/SKILL.md → .claude/skills` should resolve to {path:?}"
        );
    });
}
