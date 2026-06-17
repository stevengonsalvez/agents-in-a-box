//! ClassA walker integration tests — drives discovery::class_a against
//! tempdir-rooted `~/.claude/plugins/` fixtures so the suite stays
//! offline and never touches the real user home.
//!
//! Three core fixtures from §P0 acceptance criteria:
//!   (1) single marketplace, single plugin → one entry, units found
//!   (2) multiple marketplaces, multiple plugins → all enumerated
//!   (3) missing known_marketplaces.json → marketplace = "unknown"
//!
//! Plus the §Walker perf budget assertion (P0+P1 spec):
//!   100 plugin units complete in <500ms on debug build.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ainb_cli::discovery::class_a::{DiscoveredMarketplaceUnit, DiscoveredUnitKind, walk};

/// Write a minimal plugin.json at `cache/<mp>/<plugin>/<ver>/.claude-plugin/plugin.json`.
fn seed_plugin(claude_home: &Path, mp: &str, plugin: &str, version: &str) -> PathBuf {
    let plugin_dir = claude_home.join("plugins").join("cache").join(mp).join(plugin).join(version);
    fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        format!(
            r#"{{"name": "{plugin}", "version": "{version}"}}
"#
        ),
    )
    .unwrap();
    plugin_dir
}

/// Drop a SKILL.md under `<plugin_dir>/skills/<name>/SKILL.md`.
fn seed_skill(plugin_dir: &Path, name: &str) {
    let p = plugin_dir.join("skills").join(name);
    fs::create_dir_all(&p).unwrap();
    fs::write(
        p.join("SKILL.md"),
        format!("---\nname: {name}\n---\nbody\n"),
    )
    .unwrap();
}

/// Drop an AGENT.md under `<plugin_dir>/agents/<name>.md`.
fn seed_agent(plugin_dir: &Path, name: &str) {
    let p = plugin_dir.join("agents");
    fs::create_dir_all(&p).unwrap();
    fs::write(
        p.join(format!("{name}.md")),
        format!("---\nname: {name}\n---\nagent body\n"),
    )
    .unwrap();
}

fn write_known_marketplaces(claude_home: &Path, marketplaces: &[&str]) {
    let plugins = claude_home.join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    let entries: String = marketplaces
        .iter()
        .map(|m| format!(r#""{m}": {{"url": "https://example.test/{m}.git"}}"#))
        .collect::<Vec<_>>()
        .join(", ");
    let body = format!(
        r#"{{"marketplaces": {{{entries}}}}}
"#
    );
    fs::write(plugins.join("known_marketplaces.json"), body).unwrap();
}

fn find_entry<'a>(
    units: &'a [DiscoveredMarketplaceUnit],
    plugin: &str,
) -> Option<&'a DiscoveredMarketplaceUnit> {
    units.iter().find(|u| u.plugin == plugin)
}

#[test]
fn single_marketplace_single_plugin_discovers_units() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_home = tmp.path();

    write_known_marketplaces(claude_home, &["claude-plugins-official"]);
    let plugin_dir = seed_plugin(claude_home, "claude-plugins-official", "reflect", "1.0.0");
    seed_skill(&plugin_dir, "commit");
    seed_skill(&plugin_dir, "reflect");
    seed_agent(&plugin_dir, "code-reviewer");

    let units = walk(claude_home);

    assert_eq!(units.len(), 1, "expected 1 plugin entry, got: {units:?}");
    let entry = &units[0];
    assert_eq!(entry.plugin, "reflect");
    assert_eq!(entry.marketplace, "claude-plugins-official");
    assert_eq!(entry.version, "1.0.0");

    let skill_names: Vec<&str> = entry
        .units
        .iter()
        .filter(|u| u.kind == DiscoveredUnitKind::Skill)
        .map(|u| u.name.as_str())
        .collect();
    assert!(
        skill_names.contains(&"commit"),
        "missing skill commit: {:?}",
        entry.units
    );
    assert!(
        skill_names.contains(&"reflect"),
        "missing skill reflect: {:?}",
        entry.units
    );

    let agent_names: Vec<&str> = entry
        .units
        .iter()
        .filter(|u| u.kind == DiscoveredUnitKind::Agent)
        .map(|u| u.name.as_str())
        .collect();
    assert!(
        agent_names.contains(&"code-reviewer"),
        "missing agent: {:?}",
        entry.units
    );
}

#[test]
fn multiple_marketplaces_enumerates_all_plugins() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_home = tmp.path();

    write_known_marketplaces(
        claude_home,
        &["claude-plugins-official", "stevengonsalvez-marketplace"],
    );

    let p1 = seed_plugin(claude_home, "claude-plugins-official", "reflect", "1.0.0");
    seed_skill(&p1, "commit");

    let p2 = seed_plugin(
        claude_home,
        "claude-plugins-official",
        "session-tools",
        "0.2.1",
    );
    seed_skill(&p2, "session-summary");

    let p3 = seed_plugin(
        claude_home,
        "stevengonsalvez-marketplace",
        "ainb-extras",
        "2.0.0",
    );
    seed_agent(&p3, "tech-lead");

    let units = walk(claude_home);

    assert_eq!(units.len(), 3, "expected 3 plugin entries, got: {units:?}");

    let reflect = find_entry(&units, "reflect").expect("reflect missing");
    assert_eq!(reflect.marketplace, "claude-plugins-official");
    assert_eq!(reflect.version, "1.0.0");
    assert_eq!(reflect.units.len(), 1);

    let session_tools = find_entry(&units, "session-tools").expect("session-tools missing");
    assert_eq!(session_tools.marketplace, "claude-plugins-official");
    assert_eq!(session_tools.version, "0.2.1");

    let ainb_extras = find_entry(&units, "ainb-extras").expect("ainb-extras missing");
    assert_eq!(ainb_extras.marketplace, "stevengonsalvez-marketplace");
    assert_eq!(ainb_extras.version, "2.0.0");
    assert!(
        ainb_extras
            .units
            .iter()
            .any(|u| u.kind == DiscoveredUnitKind::Agent && u.name == "tech-lead")
    );
}

#[test]
fn missing_known_marketplaces_json_marks_marketplace_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_home = tmp.path();

    // Do NOT write known_marketplaces.json. Plugin cache still exists.
    let plugin_dir = seed_plugin(claude_home, "some-marketplace", "mystery-plugin", "0.1.0");
    seed_skill(&plugin_dir, "secret-skill");

    let units = walk(claude_home);

    assert_eq!(units.len(), 1, "expected 1 plugin entry, got: {units:?}");
    let entry = &units[0];
    assert_eq!(entry.plugin, "mystery-plugin");
    assert_eq!(
        entry.marketplace, "unknown",
        "marketplace should be 'unknown' when registry missing"
    );
    assert_eq!(entry.version, "0.1.0");
    assert!(
        entry
            .units
            .iter()
            .any(|u| u.name == "secret-skill" && u.kind == DiscoveredUnitKind::Skill)
    );
}

#[test]
fn empty_plugins_dir_returns_empty_vec() {
    let tmp = tempfile::tempdir().unwrap();
    let units = walk(tmp.path());
    assert!(units.is_empty(), "expected empty result, got: {units:?}");
}

#[test]
fn plugin_missing_manifest_json_is_skipped() {
    // A directory tree that looks plugin-shaped but has no
    // .claude-plugin/plugin.json must be silently skipped — not
    // returned as a half-formed entry.
    let tmp = tempfile::tempdir().unwrap();
    let claude_home = tmp.path();
    let bogus = claude_home
        .join("plugins")
        .join("cache")
        .join("mp")
        .join("not-a-plugin")
        .join("1.0.0");
    fs::create_dir_all(&bogus).unwrap();
    // Stick a stray skills/ dir but no plugin.json:
    fs::create_dir_all(bogus.join("skills").join("ghost")).unwrap();
    fs::write(
        bogus.join("skills").join("ghost").join("SKILL.md"),
        "---\nname: ghost\n---\n",
    )
    .unwrap();

    let units = walk(claude_home);
    assert!(
        units.is_empty(),
        "expected no entries for orphan dirs, got: {units:?}"
    );
}

#[test]
fn walker_perf_budget_500ms_for_100_units() {
    // Spec §Walker perf budget (P0+P1): 100 units must complete in
    // under 500ms on debug build. Synthetic fixture: 25 plugins × 4
    // units each = 100 units across 2 marketplaces.
    let tmp = tempfile::tempdir().unwrap();
    let claude_home = tmp.path();

    write_known_marketplaces(claude_home, &["mp-a", "mp-b"]);

    for mp_idx in 0..2 {
        let mp = if mp_idx == 0 { "mp-a" } else { "mp-b" };
        for p in 0..25 {
            let plugin = format!("plugin-{p:02}");
            let plugin_dir = seed_plugin(claude_home, mp, &plugin, "1.0.0");
            for s in 0..2 {
                seed_skill(&plugin_dir, &format!("skill-{p:02}-{s}"));
            }
            for a in 0..2 {
                seed_agent(&plugin_dir, &format!("agent-{p:02}-{a}"));
            }
        }
    }

    let start = Instant::now();
    let units = walk(claude_home);
    let elapsed = start.elapsed();

    // 50 plugins, 4 units each = 200 unit rows.
    assert_eq!(units.len(), 50, "expected 50 plugin entries");
    let total_units: usize = units.iter().map(|u| u.units.len()).sum();
    assert_eq!(total_units, 200, "expected 200 discovered units");
    assert!(
        elapsed.as_millis() < 500,
        "walker took {}ms (>500ms budget) for {} units",
        elapsed.as_millis(),
        total_units
    );
}
