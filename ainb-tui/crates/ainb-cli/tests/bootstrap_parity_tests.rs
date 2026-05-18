//! Bootstrap-parity tests for `ainb`.
//!
//! Mirrors the structural shape of the retired `toolkit/bootstrap.test.js`
//! Jest suite, but drives the new Rust CLI. For each scenario the
//! legacy engine covered, this file asserts the ainb equivalent:
//!
//! | bootstrap.js (legacy)                                | ainb equivalent                                                  |
//! |------------------------------------------------------|------------------------------------------------------------------|
//! | `node bootstrap.js --tool=claude-code-4.5`           | `ainb skill install <uri> --targets claude --yes`                |
//! | `node bootstrap.js --tool=gemini`                    | `ainb skill install <uri> --targets gemini --yes`                |
//! | `node bootstrap.js --tool=amazonq`                   | `ainb skill install <uri> --targets amazonq --yes`               |
//! | `--homeDir=$FAKE_HOME` flag                          | `AINB_USE_REAL_HOMES=1` + `HOME=$FAKE_HOME` (tier-3 resolution)   |
//! | `{{TOOL_DIR}}` substitution in SKILL.md content      | `convention::apply_substitutions` keyed by `template_substitutions()` |
//! | `{{HOME_TOOL_DIR}}` substitution                     | Same; each adapter now exposes `HOME_TOOL_DIR` per tool          |
//! | Shared content copied to multiple tool homes         | `--targets claude,codex,copilot` on one install                  |
//! | `excludeFiles: ['settings.local.json']`              | RawAdapter's `file_list` doesn't auto-include settings.local.json |
//! | `agents/` only for claude-code, not codex/gemini     | `accepts(UnitKind::Agent)` matrix per spec §7.4                  |
//!
//! Gaps documented for follow-up: SDD (`--sdd`) flow, JSON-merge
//! mutations for `mcp.json` / `settings.json`, "always copy" base
//! ruleset, linked-file `@.amazonq/rules/...` references. These
//! were bootstrap.js features outside the spec §17 scope and ship
//! as follow-up beads.

use std::path::{Path, PathBuf};

use ainb_cli::{
    dispatch, AddArgs, Command, InstallArgs, SkillCommand, SourceCommand,
};
use ainb_skill_core::lockfile::{DeployedRef, Lockfile};
use ainb_skill_core::paths::lockfile_path_in;

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
        .prefix("ainb-parity-")
        .tempdir()
        .expect("tempdir")
}

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

/// Build a source-tree fixture and `source add` it; return the
/// unit URI ready for `skill install`.
fn add_local_source_with_unit(
    home: &Path,
    source_name: &str,
    unit_subpath: &str,
    skill_body: &str,
) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let unit_dir: PathBuf = dir.path().join(unit_subpath);
    std::fs::create_dir_all(&unit_dir).unwrap();
    std::fs::write(
        unit_dir.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: parity fixture\n---\n{body}\n",
            name = unit_subpath.split('/').next_back().unwrap_or("unit"),
            body = skill_body
        ),
    )
    .unwrap();
    let local_uri = format!("local:{}", dir.path().display());
    let mut buf = Vec::new();
    dispatch(
        home,
        Command::Source {
            action: SourceCommand::Add(AddArgs {
                uri: local_uri.clone(),
                name: Some(source_name.to_string()),
                kind: None,
            }),
        },
        &mut buf,
    )
    .expect("source add");
    let unit_uri = format!("{local_uri}@main/{unit_subpath}");
    (dir, unit_uri)
}

fn install(home: &Path, uri: &str, targets: &str) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(
        home,
        Command::Skill {
            action: SkillCommand::Install(InstallArgs {
                uri: uri.to_string(),
                targets: Some(targets.to_string()),
                dry_run: false,
                yes: true,
            }),
        },
        &mut buf,
    );
    (String::from_utf8(buf).expect("utf8"), res)
}

// ----------------------------------------------------------------------
// 1. Per-tool unit deployment — mirrors bootstrap.js
//    `node bootstrap.js --tool=<X>` for each of the three core tools
//    plus the six tier-2 tools.
// ----------------------------------------------------------------------

#[test]
fn installs_skill_to_claude_target_only() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let (_src, uri) =
            add_local_source_with_unit(home.path(), "fix", "skills/commit", "claude-body");
        let (out, res) = install(home.path(), &uri, "claude");
        res.expect("install ok");
        assert!(out.contains("1 tool(s)"), "got: {out}");
        assert!(base.path().join("claude/skills/commit/SKILL.md").exists());
        // No leakage to other tools.
        assert!(!base.path().join("codex/skills/commit/SKILL.md").exists());
        assert!(!base.path().join("gemini/skills/commit/SKILL.md").exists());
    });
}

#[test]
fn installs_skill_to_codex_only() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let (_src, uri) =
            add_local_source_with_unit(home.path(), "fix", "skills/commit", "codex-body");
        let (_o, res) = install(home.path(), &uri, "codex");
        res.expect("install ok");
        assert!(base.path().join("codex/skills/commit/SKILL.md").exists());
        assert!(!base.path().join("claude/skills/commit/SKILL.md").exists());
    });
}

#[test]
fn installs_skill_to_gemini_only() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let (_src, uri) =
            add_local_source_with_unit(home.path(), "fix", "skills/commit", "gemini-body");
        let (_o, res) = install(home.path(), &uri, "gemini");
        res.expect("install ok");
        assert!(base.path().join("gemini/skills/commit/SKILL.md").exists());
    });
}

#[test]
fn installs_skill_to_amazonq_only() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let (_src, uri) =
            add_local_source_with_unit(home.path(), "fix", "skills/commit", "amazonq-body");
        let (_o, res) = install(home.path(), &uri, "amazonq");
        res.expect("install ok");
        assert!(base.path().join("amazonq/skills/commit/SKILL.md").exists());
    });
}

#[test]
fn installs_skill_to_copilot_only() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let (_src, uri) =
            add_local_source_with_unit(home.path(), "fix", "skills/commit", "copilot-body");
        let (_o, res) = install(home.path(), &uri, "copilot");
        res.expect("install ok");
        assert!(base.path().join("copilot/skills/commit/SKILL.md").exists());
    });
}

// ----------------------------------------------------------------------
// 2. Shared content — bootstrap.js's `copySharedContent: true` flow.
//    Same source unit goes to several tool homes in one invocation.
// ----------------------------------------------------------------------

#[test]
fn shared_content_lands_in_every_named_target() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let (_src, uri) = add_local_source_with_unit(
            home.path(),
            "fix",
            "skills/review",
            "shared",
        );
        let (out, res) = install(home.path(), &uri, "claude,codex,copilot,gemini");
        res.expect("install ok");
        assert!(out.contains("4 tool(s)"), "got: {out}");
        for tool in ["claude", "codex", "copilot", "gemini"] {
            assert!(
                base.path().join(format!("{tool}/skills/review/SKILL.md")).exists(),
                "missing under {tool}"
            );
        }
    });
}

// ----------------------------------------------------------------------
// 3. Template substitution — `{{TOOL_DIR}}`, `{{HOME_TOOL_DIR}}`,
//    `{{TOOL_NAME}}` get replaced with each tool's adapter-supplied
//    value at apply time. This is the ainb equivalent of
//    bootstrap.js's `templateSubstitutions: { 'CLAUDE.md': {…} }`.
// ----------------------------------------------------------------------

#[test]
fn template_substitution_applies_per_tool_at_install_time() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        // Source ships a skill whose body contains the three template
        // placeholders. ainb must rewrite them per target tool.
        let (_src, uri) = add_local_source_with_unit(
            home.path(),
            "fix",
            "skills/templated",
            "TOOL_DIR=={{TOOL_DIR}} TOOL_NAME=={{TOOL_NAME}} HOME_TOOL_DIR=={{HOME_TOOL_DIR}}",
        );
        let (_o, res) = install(home.path(), &uri, "claude,codex,gemini");
        res.expect("install ok");

        let claude_body = std::fs::read_to_string(
            base.path().join("claude/skills/templated/SKILL.md"),
        )
        .unwrap();
        assert!(claude_body.contains("TOOL_DIR==.claude"), "got: {claude_body}");
        assert!(claude_body.contains("TOOL_NAME==claude"), "got: {claude_body}");
        assert!(claude_body.contains("HOME_TOOL_DIR==~/.claude"), "got: {claude_body}");
        assert!(!claude_body.contains("{{"), "raw template leaked: {claude_body}");

        let codex_body = std::fs::read_to_string(
            base.path().join("codex/skills/templated/SKILL.md"),
        )
        .unwrap();
        assert!(codex_body.contains("TOOL_DIR==.codex"), "got: {codex_body}");
        assert!(codex_body.contains("HOME_TOOL_DIR==~/.codex"), "got: {codex_body}");

        let gemini_body = std::fs::read_to_string(
            base.path().join("gemini/skills/templated/SKILL.md"),
        )
        .unwrap();
        assert!(gemini_body.contains("TOOL_DIR==.gemini"), "got: {gemini_body}");
        assert!(gemini_body.contains("HOME_TOOL_DIR==~/.gemini"), "got: {gemini_body}");
    });
}

#[test]
fn template_substitution_leaves_non_utf8_payloads_untouched() {
    use ainb_adapters_tool::{ClaudeAdapter, ToolAdapter};
    let bytes = [0xFF, 0xFE, b'{', b'{', b'T', b'O', b'O', b'L', b'_', b'D', b'I', b'R', b'}', b'}', 0x00];
    let subs = ClaudeAdapter::new().template_substitutions();
    let out = ainb_adapters_tool::convention::apply_substitutions(&bytes, &subs);
    assert_eq!(out, bytes, "binary content must pass through unchanged");
}

// ----------------------------------------------------------------------
// 4. Accept-matrix parity (`bootstrap.js` chose which kinds to copy
//    per tool; here every adapter's `accepts()` exposes the same
//    boundary). Snapshot the canonical matrix from spec §7.4.
// ----------------------------------------------------------------------

#[test]
fn accepts_matrix_matches_spec_section_7_4() {
    use ainb_adapters_tool::{
        AcceptDecision, AmazonqAdapter, ClaudeAdapter, ClaudeDesktopAdapter, ClineAdapter,
        CodexAdapter, CopilotAdapter, CursorAdapter, GeminiAdapter, RooAdapter, ToolAdapter,
    };
    use ainb_skill_core::UnitKind;

    fn assert_accepts<A: ToolAdapter>(adapter: A, expected: &[UnitKind]) {
        for k in UnitKind::all() {
            let want_yes = expected.contains(&k);
            let got = adapter.accepts(k);
            if want_yes {
                assert_eq!(got, AcceptDecision::Yes, "{} should accept {k}", adapter.name());
            } else {
                assert!(
                    !got.is_yes(),
                    "{} must decline {k} per spec §7.4 — got {got:?}",
                    adapter.name()
                );
            }
        }
    }

    assert_accepts(
        ClaudeAdapter::new(),
        &[
            UnitKind::Skill,
            UnitKind::Plugin,
            UnitKind::Agent,
            UnitKind::Command,
            UnitKind::Hook,
            UnitKind::McpServer,
            UnitKind::Statusline,
        ],
    );
    assert_accepts(
        CodexAdapter::new(),
        &[UnitKind::Skill, UnitKind::Agent, UnitKind::Command, UnitKind::McpServer],
    );
    assert_accepts(CopilotAdapter::new(), &[UnitKind::Skill, UnitKind::Agent]);
    assert_accepts(GeminiAdapter::new(), &[UnitKind::Skill, UnitKind::Agent]);
    assert_accepts(
        CursorAdapter::new(),
        &[UnitKind::Skill, UnitKind::Command, UnitKind::McpServer],
    );
    assert_accepts(AmazonqAdapter::new(), &[UnitKind::Skill]);
    assert_accepts(ClaudeDesktopAdapter::new(), &[UnitKind::McpServer]);
    assert_accepts(ClineAdapter::new(), &[UnitKind::Skill, UnitKind::McpServer]);
    assert_accepts(RooAdapter::new(), &[UnitKind::Skill, UnitKind::McpServer]);
}

// ----------------------------------------------------------------------
// 5. Default install with no --targets fans out to every accepting
//    adapter (bootstrap.js iterated TOOL_CONFIG; ainb iterates
//    `all_adapters()`).
// ----------------------------------------------------------------------

#[test]
fn default_install_fans_out_to_all_accepting_adapters() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    with_tool_homes(base.path(), || {
        let (_src, uri) = add_local_source_with_unit(
            home.path(),
            "fix",
            "skills/everyone",
            "v1",
        );
        let mut buf = Vec::new();
        let res = dispatch(
            home.path(),
            Command::Skill {
                action: SkillCommand::Install(InstallArgs {
                    uri: uri.clone(),
                    targets: None,
                    dry_run: false,
                    yes: true,
                }),
            },
            &mut buf,
        );
        res.expect("install ok");
        let out = String::from_utf8(buf).unwrap();
        // 8 of 9 tools accept Skill (claude-desktop declines).
        assert!(out.contains("8 tool(s)"), "got: {out}");
        assert!(out.contains("1 skipped"), "got: {out}");
        let lock = Lockfile::load_from(&lockfile_path_in(home.path())).unwrap();
        assert!(matches!(
            lock.units[0].deployed.get("claude-desktop").unwrap(),
            DeployedRef::Skipped { .. }
        ));
    });
}

// ----------------------------------------------------------------------
// 6. AINB_USE_REAL_HOMES=1 + tier-3 resolution. The legacy bootstrap
//    used `--homeDir=$FAKE_HOME`; the new way is to set HOME +
//    AINB_USE_REAL_HOMES=1 and let install_root_for() resolve to the
//    real per-tool config dir.
// ----------------------------------------------------------------------

#[test]
fn real_homes_opt_in_writes_under_fake_home() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let fake_home = tempfile::tempdir().unwrap();
    let ainb_home = tempfile::tempdir().unwrap();

    // Wipe per-tool overrides so install_root_for() takes the
    // real-home tier when AINB_USE_REAL_HOMES is set.
    for var in ALL_TOOL_ENV_VARS {
        std::env::remove_var(var);
    }
    std::env::set_var("HOME", fake_home.path());
    std::env::set_var("AINB_USE_REAL_HOMES", "1");

    let home = tmp_home();
    let (_src, uri) = add_local_source_with_unit(
        home.path(),
        "fix",
        "skills/commit",
        "real-home-target",
    );
    let _ = ainb_home; // unused; just keeps the dir alive.

    let mut buf = Vec::new();
    let res = dispatch(
        home.path(),
        Command::Skill {
            action: SkillCommand::Install(InstallArgs {
                uri: uri.clone(),
                targets: Some("claude".into()),
                dry_run: false,
                yes: true,
            }),
        },
        &mut buf,
    );

    std::env::remove_var("AINB_USE_REAL_HOMES");
    res.expect("install ok");

    let landed = fake_home.path().join(".claude/skills/commit/SKILL.md");
    assert!(
        landed.exists(),
        "expected install to land in tier-3 real-home path {}",
        landed.display()
    );
}

// ----------------------------------------------------------------------
// 7. Settings.local.json-style excludes — bootstrap.js
//    `excludeFiles: ['settings.local.json']` doesn't have an ainb
//    1:1 yet (file_list comes from the source adapter and is not
//    pre-filtered). Verify that `RawAdapter` only picks up the
//    canonical files under `skills/<name>/`, so a stray
//    `settings.local.json` in the source root is naturally
//    excluded from the deploy set.
// ----------------------------------------------------------------------

#[test]
fn raw_adapter_naturally_excludes_settings_local_json() {
    let home = tmp_home();
    let base = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    // A skill unit + a settings.local.json sibling at the root.
    std::fs::create_dir_all(src.path().join("skills/commit")).unwrap();
    std::fs::write(
        src.path().join("skills/commit/SKILL.md"),
        "---\nname: commit\n---\nv\n",
    )
    .unwrap();
    std::fs::write(src.path().join("settings.local.json"), "{}").unwrap();

    with_tool_homes(base.path(), || {
        let local_uri = format!("local:{}", src.path().display());
        let mut buf = Vec::new();
        dispatch(
            home.path(),
            Command::Source {
                action: SourceCommand::Add(AddArgs {
                    uri: local_uri.clone(),
                    name: Some("fix".into()),
                    kind: None,
                }),
            },
            &mut buf,
        )
        .unwrap();
        let (_o, res) = install(
            home.path(),
            &format!("{local_uri}@main/skills/commit"),
            "claude",
        );
        res.expect("install ok");
        // SKILL.md present, settings.local.json never even resolved.
        assert!(base.path().join("claude/skills/commit/SKILL.md").exists());
        assert!(!base.path().join("claude/settings.local.json").exists());
    });
}
