//! Sandbox-fixture smoke: assert `build_skill_manager_sandbox` actually
//! creates everything its docstring promises. Without this we have no
//! ground truth that the fixture (used by every downstream sync /
//! drift / tripwire test) is sound — a broken fixture would silently
//! mask real regressions everywhere.
//!
//! Gated by `feature = "test-fixtures"` via the Cargo.toml
//! `required-features` clause on this test target. Bead `ai-e7t`.

use std::path::Path;

use ainb_skill_core::{
    SANDBOX_MARKER_FILE, SandboxLayout, SandboxTier, build_skill_manager_sandbox,
};

fn assert_has_skill(home: &Path, name: &str) {
    let skill_md = home.join("skills").join(name).join("SKILL.md");
    assert!(
        skill_md.is_file(),
        "missing seeded skill at {}",
        skill_md.display()
    );
    let content = std::fs::read_to_string(&skill_md).expect("read SKILL.md");
    assert!(
        content.starts_with("---\nname: "),
        "SKILL.md at {} missing frontmatter: {content:?}",
        skill_md.display()
    );
}

fn assert_minimal_layout(layout: &SandboxLayout) {
    // sentinel marker present so bash teardown can recognise the dir
    assert!(
        layout.root.join(SANDBOX_MARKER_FILE).is_file(),
        "missing sandbox marker file"
    );
    // four expected dirs
    assert!(layout.ainb_home.is_dir(), "ainb_home missing");
    assert!(layout.claude_home.is_dir(), "claude_home missing");
    assert!(layout.codex_home.is_dir(), "codex_home missing");
    // claude side: hand-authored + external-clone-shape skills + flat-md agent
    assert_has_skill(&layout.claude_home, "commit");
    assert_has_skill(&layout.claude_home, "fireworks-tech-graph");
    let agent = layout.claude_home.join("agents/distinguished-engineer.md");
    assert!(agent.is_file(), "missing flat-md agent");
    // codex side: one skill
    assert_has_skill(&layout.codex_home, "deploy");
    // marketplace plugin under plugins/cache
    let plugin_root = layout.claude_home.join("plugins/cache/sandbox-marketplace/discord/0.1.0");
    assert!(
        plugin_root.join(".claude-plugin/plugin.json").is_file(),
        "missing plugin.json"
    );
    assert!(
        plugin_root.join("skills/access/SKILL.md").is_file(),
        "missing bundled access skill"
    );
    assert!(
        plugin_root.join("skills/configure/SKILL.md").is_file(),
        "missing bundled configure skill"
    );
    // bare remote initialised + reachable via the helper URI
    assert!(layout.bare_remote.is_dir(), "bare remote dir missing");
    let head_path = layout.bare_remote.join("HEAD");
    assert!(
        head_path.is_file(),
        "bare HEAD missing (git init didn't run?)"
    );
    assert!(layout.bare_remote_uri().starts_with("git:file://"));
    assert!(layout.seed_unit_uri().contains("@main/skills/initial-skill"));
}

#[test]
fn minimal_tier_seeds_every_claimed_artifact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let layout = build_skill_manager_sandbox(tmp.path(), SandboxTier::Minimal)
        .expect("build sandbox minimal");
    assert_eq!(layout.tier, SandboxTier::Minimal);
    assert_minimal_layout(&layout);
    // ainb_home is empty in Minimal — no manifest pre-seed; tests
    // drive `ainb source add` themselves.
    assert!(
        !layout.ainb_home.join("manifest.yaml").exists(),
        "Minimal tier must NOT pre-seed manifest.yaml"
    );
}

#[test]
fn full_tier_adds_every_extra_artifact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let layout =
        build_skill_manager_sandbox(tmp.path(), SandboxTier::Full).expect("build sandbox full");
    assert_eq!(layout.tier, SandboxTier::Full);
    // Minimal-tier content still present.
    assert_minimal_layout(&layout);

    // All 9 adapter tool homes populated.
    for (tool, subpath) in [
        ("copilot", ".copilot"),
        ("gemini", ".gemini"),
        ("cursor", ".cursor"),
        ("amazonq", ".aws/amazonq"),
        ("claude-desktop", ".config/Claude"),
        ("cline", ".cline"),
        ("roo", ".roo"),
    ] {
        let tool_home = layout.root.join(subpath);
        assert!(
            tool_home.is_dir(),
            "Full tier: {tool} home missing at {}",
            tool_home.display()
        );
        assert_has_skill(&tool_home, &format!("{tool}-side-skill"));
    }

    // Two additional marketplace plugins under different marketplaces.
    assert!(
        layout
            .claude_home
            .join("plugins/cache/official/code-review/0.2.0/skills/review/SKILL.md")
            .is_file(),
        "official/code-review marketplace plugin missing"
    );
    assert!(
        layout
            .claude_home
            .join("plugins/cache/community/third-party-thing/unknown/skills/thing/SKILL.md")
            .is_file(),
        "community/third-party-thing marketplace plugin missing"
    );

    // Full-tier manifest is pre-seeded with a shadowed_by conflict pair.
    let manifest = std::fs::read_to_string(layout.ainb_home.join("manifest.yaml"))
        .expect("Full tier manifest");
    assert!(manifest.contains("shadowed_by:"), "missing conflict pair");
    assert!(
        manifest.contains("git:file://"),
        "manifest source URI missing the bare remote"
    );

    // Banner skip-marker.
    assert!(
        layout.ainb_home.join(".skip-banner").is_file(),
        "skip-banner marker missing"
    );

    // external-dependencies.yaml at sandbox root, real schema.
    let deps_yaml = std::fs::read_to_string(layout.root.join("external-dependencies.yaml"))
        .expect("external-dependencies.yaml");
    assert!(
        deps_yaml.contains("external-packages:"),
        "missing external-packages section"
    );
    assert!(
        deps_yaml.contains("bundled-skills:"),
        "missing bundled-skills section"
    );
    assert!(
        deps_yaml.contains("agent-skills:"),
        "missing agent-skills section"
    );
    // Must parse cleanly — load via serde_yaml_ng to catch malformed YAML
    let _: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&deps_yaml).expect("external-dependencies.yaml must parse");
    // One agent-skills entry must name a seeded Minimal-tier skill so
    // the future provenance matcher has a real fixture to resolve.
    assert!(
        deps_yaml.contains("name: fireworks-tech-graph"),
        "agent-skills must reference the seeded fireworks-tech-graph skill"
    );
}

#[test]
fn rebuild_into_existing_root_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _ = build_skill_manager_sandbox(tmp.path(), SandboxTier::Minimal).expect("first");
    // First run leaves marker + seeded files in place. Second run
    // wipes and reseeds without panicking. Tests that hold a layout
    // from the first call would see invalidated paths — that's OK,
    // test scaffolding is expected to keep `tempdir` and the layout
    // together.
    let l2 = build_skill_manager_sandbox(tmp.path(), SandboxTier::Minimal).expect("second");
    assert_minimal_layout(&l2);
}

#[test]
fn refuses_to_seed_into_real_home() {
    // Only runs when $HOME is set (every Unix shell sets it).
    let Some(home) = std::env::var_os("HOME") else {
        eprintln!("SKIP: $HOME not set");
        return;
    };
    let home = std::path::PathBuf::from(home);
    let err = build_skill_manager_sandbox(&home, SandboxTier::Minimal)
        .expect_err("must refuse $HOME as root");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    let msg = err.to_string();
    assert!(
        msg.contains("refusing") && msg.contains("HOME"),
        "expected refuse-real-home message, got: {msg}"
    );
}

#[test]
fn env_vars_round_trip_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let layout =
        build_skill_manager_sandbox(tmp.path(), SandboxTier::Minimal).expect("build minimal");
    let vars = layout.env_vars();
    let keys: Vec<&str> = vars.iter().map(|(k, _)| *k).collect();
    for required in [
        "HOME",
        "AINB_HOME",
        "AINB_TOOL_HOME_CLAUDE",
        "AINB_TOOL_HOME_CODEX",
        "GIT_TERMINAL_PROMPT",
        "GIT_ASKPASS",
    ] {
        assert!(keys.contains(&required), "missing env var {required}");
    }
}
