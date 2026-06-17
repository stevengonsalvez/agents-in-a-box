//! Provenance-matcher tests (bead ai-ya9, v1.3).
//!
//! Pure-function attribution across the five [`Provenance`] variants,
//! the provenance-aware `synth_uri` reconcile change, and the
//! `ainb skill scan` CLI. NO SQLite anywhere: every input is a parsed
//! manifest / JSON / YAML struct and `attribute()` is a pure function.
//!
//! The matching live tmux tripwire lives in
//! `ainb-core/tests/tripwire_core_skill_manager_provenance_live.rs`.

use std::path::{Path, PathBuf};

use ainb_cli::discovery::class_a::{DiscoveredMarketplaceUnit, DiscoveredUnit, DiscoveredUnitKind};
use ainb_cli::discovery::class_c::DiscoveredOrphanUnit;
use ainb_cli::discovery::provenance::{
    AttributedUnit, Provenance, ProvenanceSources, attribute, parse_external_dependencies,
    parse_installed_plugins,
};
use ainb_cli::discovery::reconcile::{WalkerOutput, reconcile_with_sources};

use ainb_skill_core::{UnitEntry, UnitKind};

// ------------------------------------------------------------------
// Builders
// ------------------------------------------------------------------

fn orphan(tool: &str, kind: UnitKind, name: &str) -> DiscoveredOrphanUnit {
    DiscoveredOrphanUnit {
        tool: tool.to_string(),
        kind,
        name: name.to_string(),
        path: PathBuf::from(format!("/tmp/fixture/{tool}/skills/{name}")),
        frontmatter_valid: true,
    }
}

fn marketplace_plugin(
    plugin: &str,
    marketplace: &str,
    version: &str,
    skills: &[&str],
) -> DiscoveredMarketplaceUnit {
    DiscoveredMarketplaceUnit {
        plugin: plugin.to_string(),
        marketplace: marketplace.to_string(),
        version: version.to_string(),
        units: skills
            .iter()
            .map(|n| DiscoveredUnit {
                kind: DiscoveredUnitKind::Skill,
                name: (*n).to_string(),
                path: PathBuf::from(format!(
                    "/tmp/fixture/cache/{marketplace}/{plugin}/{version}/skills/{n}"
                )),
            })
            .collect(),
    }
}

/// The real-schema `installed_plugins.json` Claude Code v2 writes.
/// Keyed by `<plugin>@<marketplace>`, value is a list of per-scope
/// install records. `gitCommitSha` is optional (absent on some
/// `version: unknown` installs).
const INSTALLED_PLUGINS_JSON: &str = r#"{
  "version": 2,
  "plugins": {
    "discord@sandbox-marketplace": [
      {
        "scope": "user",
        "installPath": "/home/u/.claude/plugins/cache/sandbox-marketplace/discord/0.1.0",
        "version": "0.1.0",
        "gitCommitSha": "fc005f9f17d3bd04534729bc213354b257baf027"
      }
    ],
    "code-review@official": [
      {
        "scope": "user",
        "installPath": "/home/u/.claude/plugins/cache/official/code-review/0.2.0",
        "version": "0.2.0"
      }
    ]
  }
}"#;

/// Trimmed copy of the sandbox fixture's `external-dependencies.yaml`.
const EXTERNAL_DEPS_YAML: &str = r#"version: "1.0.0"
bundled-skills:
  - name: coding-agent
    path: packages/skills/coding-agent
    purpose: "Delegate coding tasks to subagents"
agent-skills:
  - name: fireworks-tech-graph
    repo: https://github.com/yizhiyanhua-ai/fireworks-tech-graph
    applies-to: [claude, codex, copilot, gemini]
    purpose: "SVG technical-diagram generator"
  - name: ui-ux-pro-max
    repo: https://github.com/nextlevelbuilder/ui-ux-pro-max-skill
    version: "2.2.1"
    applies-to: [claude, codex, copilot, gemini]
    purpose: "UI/UX design intelligence"
"#;

fn find<'a>(units: &'a [AttributedUnit], name: &str) -> &'a AttributedUnit {
    units
        .iter()
        .find(|u| u.name == name)
        .unwrap_or_else(|| panic!("no attributed unit named `{name}` in {units:#?}"))
}

// ==================================================================
// Test ladder #1-#5: attribute() across all five variants
// ==================================================================

#[test]
fn attribute_marketplace_from_installed_plugins() {
    let class_a = vec![marketplace_plugin(
        "discord",
        "sandbox-marketplace",
        "0.1.0",
        &["access"],
    )];
    let sources = ProvenanceSources {
        installed_plugins: parse_installed_plugins(INSTALLED_PLUGINS_JSON),
        external_deps: parse_external_dependencies(""),
        manifest_units: Vec::new(),
        toolkit_bundled: Vec::new(),
    };
    let attributed = attribute(&class_a, &[], &sources);
    let unit = find(&attributed, "access");
    match &unit.provenance {
        Provenance::Marketplace {
            plugin,
            marketplace,
            version,
            git_commit_sha,
            scope,
        } => {
            assert_eq!(plugin, "discord");
            assert_eq!(marketplace, "sandbox-marketplace");
            assert_eq!(version, "0.1.0");
            assert_eq!(
                git_commit_sha.as_deref(),
                Some("fc005f9f17d3bd04534729bc213354b257baf027"),
                "git sha must be joined from installed_plugins.json"
            );
            assert_eq!(scope.as_deref(), Some("user"));
        }
        other => panic!("expected Marketplace, got {other:?}"),
    }
}

#[test]
fn attribute_external_repo_from_deps_yaml() {
    // A `~/.claude/skills/fireworks-tech-graph` orphan that name-matches
    // an `agent-skills[].name` in external-dependencies.yaml resolves
    // to ExternalRepo with the upstream repo + applies_to list.
    let class_c = vec![orphan("claude", UnitKind::Skill, "fireworks-tech-graph")];
    let sources = ProvenanceSources {
        installed_plugins: parse_installed_plugins("{}"),
        external_deps: parse_external_dependencies(EXTERNAL_DEPS_YAML),
        manifest_units: Vec::new(),
        toolkit_bundled: Vec::new(),
    };
    let attributed = attribute(&[], &class_c, &sources);
    let unit = find(&attributed, "fireworks-tech-graph");
    match &unit.provenance {
        Provenance::ExternalRepo {
            repo,
            version,
            applies_to,
        } => {
            assert_eq!(
                repo,
                "https://github.com/yizhiyanhua-ai/fireworks-tech-graph"
            );
            assert_eq!(version.as_deref(), None);
            assert_eq!(
                applies_to,
                &["claude", "codex", "copilot", "gemini"].map(String::from).to_vec()
            );
        }
        other => panic!("expected ExternalRepo, got {other:?}"),
    }
}

#[test]
fn attribute_toolkit_from_bundled() {
    // A name in `bundled-skills` resolves to Toolkit{path}.
    let class_c = vec![orphan("claude", UnitKind::Skill, "coding-agent")];
    let sources = ProvenanceSources {
        installed_plugins: parse_installed_plugins("{}"),
        external_deps: parse_external_dependencies(EXTERNAL_DEPS_YAML),
        manifest_units: Vec::new(),
        toolkit_bundled: Vec::new(),
    };
    let attributed = attribute(&[], &class_c, &sources);
    let unit = find(&attributed, "coding-agent");
    match &unit.provenance {
        Provenance::Toolkit { path } => {
            assert_eq!(path, &PathBuf::from("packages/skills/coding-agent"));
        }
        other => panic!("expected Toolkit, got {other:?}"),
    }
}

#[test]
fn attribute_already_adopted_from_manifest() {
    // A unit already present in manifest.yaml (matched by name) is
    // AlreadyAdopted, carrying the manifest URI.
    let class_c = vec![orphan("claude", UnitKind::Skill, "commit")];
    let manifest_units = vec![UnitEntry {
        uri: "gh:org/repo@main/skills/commit".to_string(),
        targets: Some(vec!["claude".to_string()]),
        shadowed_by: None,
    }];
    let sources = ProvenanceSources {
        installed_plugins: parse_installed_plugins("{}"),
        external_deps: parse_external_dependencies(""),
        manifest_units,
        toolkit_bundled: Vec::new(),
    };
    let attributed = attribute(&[], &class_c, &sources);
    let unit = find(&attributed, "commit");
    match &unit.provenance {
        Provenance::AlreadyAdopted { uri } => {
            assert_eq!(uri.display(), "gh:org/repo@main/skills/commit");
        }
        other => panic!("expected AlreadyAdopted, got {other:?}"),
    }
}

#[test]
fn attribute_local_fallback() {
    // A hand-authored skill that matches nothing falls back to Local.
    let class_c = vec![orphan("claude", UnitKind::Skill, "my-hand-rolled-skill")];
    let sources = ProvenanceSources {
        installed_plugins: parse_installed_plugins("{}"),
        external_deps: parse_external_dependencies(EXTERNAL_DEPS_YAML),
        manifest_units: Vec::new(),
        toolkit_bundled: Vec::new(),
    };
    let attributed = attribute(&[], &class_c, &sources);
    let unit = find(&attributed, "my-hand-rolled-skill");
    assert!(
        matches!(unit.provenance, Provenance::Local),
        "expected Local fallback, got {:?}",
        unit.provenance
    );
}

// ==================================================================
// Test ladder #6: synth_uri is provenance-aware
// ==================================================================

#[test]
fn synth_uri_is_provenance_aware() {
    // The external-clone orphan must reconcile to a `gh:` URI; the
    // hand-authored orphan stays `local:`; the marketplace unit stays
    // `marketplace:`. This is the one behavioural change to the import
    // path — it only fires when ProvenanceSources is supplied.
    let walker = WalkerOutput {
        class_a: vec![marketplace_plugin(
            "discord",
            "sandbox-marketplace",
            "0.1.0",
            &["access"],
        )],
        class_c: vec![
            orphan("claude", UnitKind::Skill, "fireworks-tech-graph"),
            orphan("claude", UnitKind::Skill, "my-hand-rolled-skill"),
        ],
    };
    let sources = ProvenanceSources {
        installed_plugins: parse_installed_plugins(INSTALLED_PLUGINS_JSON),
        external_deps: parse_external_dependencies(EXTERNAL_DEPS_YAML),
        manifest_units: Vec::new(),
        toolkit_bundled: Vec::new(),
    };
    let patch = reconcile_with_sources(&walker, &sources);

    let uris: Vec<&str> = patch.new_units.iter().map(|u| u.uri.as_str()).collect();

    // External clone → gh: upstream, NOT local:.
    assert!(
        uris.iter().any(|u| u.starts_with("gh:") && u.contains("fireworks-tech-graph")),
        "fireworks-tech-graph should reconcile to a gh: URI, got: {uris:?}"
    );
    assert!(
        !uris
            .iter()
            .any(|u| u.starts_with("local:") && u.contains("fireworks-tech-graph")),
        "fireworks-tech-graph must NOT stay local: — {uris:?}"
    );
    // Hand-authored orphan stays local:.
    assert!(
        uris.iter()
            .any(|u| u.starts_with("local:") && u.contains("my-hand-rolled-skill")),
        "hand-authored orphan should stay local:, got: {uris:?}"
    );
    // Marketplace unit stays marketplace:.
    assert!(
        uris.iter().any(|u| u.starts_with("marketplace:") && u.contains("access")),
        "marketplace unit should stay marketplace:, got: {uris:?}"
    );
}

#[test]
fn reconcile_without_sources_is_byte_identical_to_legacy() {
    // v1.2 round-trip guard: with no provenance sources, the new
    // entrypoint must produce the EXACT same patch as the legacy
    // `reconcile` (no gh: rewriting). Protects lockfile byte-stability.
    let walker = WalkerOutput {
        class_a: vec![marketplace_plugin(
            "discord",
            "sandbox-marketplace",
            "0.1.0",
            &["access"],
        )],
        class_c: vec![orphan("claude", UnitKind::Skill, "fireworks-tech-graph")],
    };
    let legacy = ainb_cli::discovery::reconcile::reconcile(&walker);
    let empty_sources = ProvenanceSources::default();
    let with_empty = reconcile_with_sources(&walker, &empty_sources);
    assert_eq!(
        legacy, with_empty,
        "empty ProvenanceSources must not change reconcile output (v1.2 byte-stability)"
    );
}

// ==================================================================
// Test ladder #7: `ainb skill scan` CLI
// ==================================================================

use ainb_cli::{Command, ScanArgs, SkillCommand, dispatch};

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

fn run_scan(home: &Path, args: ScanArgs) -> String {
    let mut buf = Vec::new();
    dispatch(
        home,
        Command::Skill {
            action: SkillCommand::Scan(args),
        },
        &mut buf,
    )
    .expect("scan dispatch");
    String::from_utf8(buf).expect("utf8")
}

fn seed_orphan_skill(claude_home: &Path, name: &str) {
    let dir = claude_home.join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\nkind: skill\n---\nbody\n"),
    )
    .unwrap();
}

#[test]
fn cli_scan_tree_and_filters() {
    let home_tmp = tempfile::tempdir().expect("ainb home");
    let tools_tmp = tempfile::tempdir().expect("tool homes");
    let home = home_tmp.path();

    with_tool_homes(tools_tmp.path(), || {
        let claude_home = tools_tmp.path().join("claude");
        // External-clone orphan + a hand-authored orphan.
        seed_orphan_skill(&claude_home, "fireworks-tech-graph");
        seed_orphan_skill(&claude_home, "my-hand-rolled-skill");
        // external-dependencies.yaml next to the tool home so the scan
        // can resolve the external clone's provenance.
        std::fs::write(
            tools_tmp.path().join("external-dependencies.yaml"),
            EXTERNAL_DEPS_YAML,
        )
        .unwrap();

        // Default scan: a tree showing both, with provenance labels.
        let out = run_scan(
            home,
            ScanArgs {
                provenance: None,
                tool: None,
                json: false,
                ext_deps: Some(tools_tmp.path().join("external-dependencies.yaml")),
            },
        );
        assert!(
            out.contains("fireworks-tech-graph"),
            "scan tree missing external clone:\n{out}"
        );
        assert!(
            out.contains("my-hand-rolled-skill"),
            "scan tree missing hand-authored skill:\n{out}"
        );
        assert!(
            out.contains("external") || out.contains("gh:"),
            "scan should label the clone as external:\n{out}"
        );
        assert!(
            out.contains("local"),
            "scan should label the hand-authored skill as local:\n{out}"
        );

        // --provenance external: only the external clone survives.
        let only_ext = run_scan(
            home,
            ScanArgs {
                provenance: Some("external".to_string()),
                tool: None,
                json: false,
                ext_deps: Some(tools_tmp.path().join("external-dependencies.yaml")),
            },
        );
        assert!(
            only_ext.contains("fireworks-tech-graph"),
            "--provenance external dropped the external clone:\n{only_ext}"
        );
        assert!(
            !only_ext.contains("my-hand-rolled-skill"),
            "--provenance external should exclude local skills:\n{only_ext}"
        );

        // --tool claude scopes to one tool home.
        let only_claude = run_scan(
            home,
            ScanArgs {
                provenance: None,
                tool: Some("claude".to_string()),
                json: false,
                ext_deps: Some(tools_tmp.path().join("external-dependencies.yaml")),
            },
        );
        assert!(
            only_claude.contains("fireworks-tech-graph"),
            "--tool claude should still include claude orphans:\n{only_claude}"
        );

        // --json emits machine-readable output containing the provenance kind.
        let as_json = run_scan(
            home,
            ScanArgs {
                provenance: None,
                tool: None,
                json: true,
                ext_deps: Some(tools_tmp.path().join("external-dependencies.yaml")),
            },
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&as_json).expect("scan --json must be valid JSON");
        let arr = parsed.as_array().expect("scan --json is a JSON array");
        assert!(
            arr.iter()
                .any(|v| v.get("name").and_then(|n| n.as_str()) == Some("fireworks-tech-graph")),
            "scan --json missing fireworks-tech-graph: {as_json}"
        );
    });
}
