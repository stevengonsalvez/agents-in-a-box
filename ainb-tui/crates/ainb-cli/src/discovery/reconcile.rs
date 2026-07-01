//! Reconciler — pure fn over the class-A + class-C walker outputs
//! that synthesizes a [`ManifestPatch`] for the SkillManager TUI
//! to merge into `manifest.yaml`.
//!
//! Spec: `.agents/goals/ainb-skill-manager-v1.1-discovery-spec.md`
//! §Implementation Phases P3 + §URI scheme additions +
//! §User Flow 3 (conflict resolution).
//!
//! ## Contract
//!
//! - **Pure.** No filesystem, no env, no global state. Reproducible
//!   across runs given the same input.
//! - **Deterministic.** Output order is a function of input order.
//!   Same input → identical patch byte-for-byte.
//! - **Total.** Never returns an error and never panics; malformed
//!   walker output is best-effort skipped (the walkers themselves
//!   already filter unparseable units).
//!
//! ## URI shapes emitted
//!
//! Each unit gets a per-skill / per-agent URI so the manifest can
//! flag a single shadow relationship without affecting sibling
//! units in the same plugin.
//!
//! - Class A unit:
//!   `marketplace:<plugin>@<marketplace>/<subdir>/<name>` where
//!   `<subdir>` is `skills` or `agents`.
//! - Class C unit:
//!   `local:<tool-home>/<subdir>@head/<name>` where `<tool-home>`
//!   is the per-tool canonical path
//!   (e.g. `~/.claude`, `~/.aws/amazonq`).
//!
//! Sources are deduplicated. One `marketplace:<marketplace-name>`
//! source per discovered marketplace; one
//! `local:<tool-home>/<subdir>` source per discovered (tool, kind)
//! pair.
//!
//! ## Conflict resolution
//!
//! Per spec §User Flow 3 — "orphan wins by default":
//!
//! - **A + C (same name, C in `claude` tool home)** → C is active
//!   (no shadow); A's UnitEntry gets `shadowed_by = C.uri`.
//! - **A + C (C in any non-claude tool home)** → not a conflict.
//!   Marketplace plugins only deploy to `~/.claude`, so orphans
//!   in `~/.codex`, `~/.gemini`, etc. are independent. Both
//!   adopted, neither shadowed.
//! - **C + C (same name, different tool homes)** → not a conflict.
//!   Per spec edge cases, "adopt as two separate units, each tied
//!   to its tool home." Both active, neither shadowed.
//! - **A + A (same unit name in different marketplaces / plugins)**
//!   → first-seen wins, second's UnitEntry gets
//!   `shadowed_by = first.uri`. Documented policy choice (spec
//!   does not mandate; deterministic + the only sensible default
//!   without user intervention).
//!
//! The `[s]` keybind in the SkillManager TUI flips which side wins
//! per-unit (P7, sibling bead). That mutation re-targets the
//! existing `shadowed_by` and does not call this function.

use std::collections::BTreeSet;
use std::collections::HashMap;

use ainb_skill_core::{SourceEntry, UnitEntry, UnitKind, Uri};

use super::class_a::{DiscoveredMarketplaceUnit, DiscoveredUnitKind};
use super::class_c::DiscoveredOrphanUnit;

/// Bundle of walker outputs handed to the reconciler.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkerOutput {
    /// Class-A: marketplace plugins from
    /// `~/.claude/plugins/cache/<mp>/<plugin>/<ver>/`.
    pub class_a: Vec<DiscoveredMarketplaceUnit>,
    /// Class-C: orphan SKILL.md / `.md` units found in any of the
    /// 9 tool homes by [`super::class_c::walk_orphans`].
    pub class_c: Vec<DiscoveredOrphanUnit>,
}

/// What the reconciler proposes adding to the manifest. Callers
/// merge `new_sources` into `manifest.sources` (dedup by `name`)
/// and `new_units` into `manifest.units` (dedup by `uri`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestPatch {
    pub new_sources: Vec<SourceEntry>,
    pub new_units: Vec<UnitEntry>,
}

/// Build a [`ManifestPatch`] from walker output.
///
/// Pure / total / deterministic per the module contract. Class C
/// is processed before class A so the conflict-resolution rule
/// "orphan wins" can be applied by looking up the active C URI
/// when scanning each A unit.
pub fn reconcile(walker_out: &WalkerOutput) -> ManifestPatch {
    let mut new_sources: Vec<SourceEntry> = Vec::new();
    let mut new_units: Vec<UnitEntry> = Vec::new();
    let mut source_uris_seen: BTreeSet<String> = BTreeSet::new();

    // Index of (name → URI) for active class-C orphans deployed in
    // the `claude` tool home. Marketplace plugins also deploy to
    // `~/.claude/skills/...`, so this is the only tool where A+C
    // can collide.
    let mut active_claude_orphans: HashMap<String, String> = HashMap::new();

    // ----- Pass 1: class-C orphans (active by default) ------------
    for orphan in &walker_out.class_c {
        let Some(tool_home) = tool_home_prefix(&orphan.tool) else {
            // Unknown tool (e.g. one the walker added speculatively
            // without a path mapping) — skip rather than emit a
            // malformed `local:` URI.
            continue;
        };
        let subdir = unit_kind_subdir(orphan.kind);
        let source_uri = format!("local:{tool_home}/{subdir}");
        let source_name = local_source_name(&orphan.tool, subdir);
        if source_uris_seen.insert(source_uri.clone()) {
            new_sources.push(SourceEntry {
                name: source_name,
                kind: None,
                uri: source_uri.clone(),
                r#ref: "head".to_string(),
                enabled: true,
                read_only: false,
                target_layout: Vec::new(),
            });
        }
        let unit_uri = format!("{source_uri}@head/{}", orphan.name);
        new_units.push(UnitEntry {
            uri: unit_uri.clone(),
            targets: Some(vec![orphan.tool.clone()]),
            shadowed_by: None,
        });
        if orphan.tool == "claude" {
            // Record only the first-seen orphan name in claude — a
            // duplicate name from the same tool home is impossible
            // (filesystem-unique), so first-insert is always the
            // unique entry.
            active_claude_orphans.entry(orphan.name.clone()).or_insert(unit_uri);
        }
    }

    // ----- Pass 2: class-A marketplace plugins -------------------
    // Track the first-seen URI per unit name so a duplicate unit
    // from a second marketplace can be shadowed by the first
    // (A+A first-seen-wins).
    let mut first_seen_marketplace_unit: HashMap<String, String> = HashMap::new();
    for plugin in &walker_out.class_a {
        let mp_source_uri = format!("marketplace:{}", plugin.marketplace);
        let mp_source_name = format!("marketplace-{}", plugin.marketplace);
        if source_uris_seen.insert(mp_source_uri.clone()) {
            new_sources.push(SourceEntry {
                name: mp_source_name,
                kind: Some("claude-marketplace".to_string()),
                uri: mp_source_uri,
                r#ref: "head".to_string(),
                enabled: true,
                read_only: true,
                target_layout: Vec::new(),
            });
        }

        for unit in &plugin.units {
            let subdir = class_a_unit_subdir(unit.kind);
            let unit_uri = format!(
                "marketplace:{plugin}@{marketplace}/{subdir}/{name}",
                plugin = plugin.plugin,
                marketplace = plugin.marketplace,
                subdir = subdir,
                name = unit.name,
            );

            // Determine shadow status:
            // 1. A+C in claude → C wins, this A is shadowed.
            // 2. A+A (second marketplace shipping same name) →
            //    first-seen wins, this A is shadowed.
            let shadowed_by_uri = active_claude_orphans
                .get(&unit.name)
                .cloned()
                .or_else(|| first_seen_marketplace_unit.get(&unit.name).cloned());

            let shadowed_by = shadowed_by_uri.and_then(|s| Uri::parse(&s).ok());

            new_units.push(UnitEntry {
                uri: unit_uri.clone(),
                targets: Some(vec!["claude".to_string()]),
                shadowed_by,
            });

            // Only record this marketplace unit as the "first-seen"
            // for the A+A check when there isn't already a class-C
            // orphan acting as the active winner — that way a third
            // duplicate (A+A+A with C also present) still chains to
            // the C, not to the first A.
            first_seen_marketplace_unit.entry(unit.name.clone()).or_insert(unit_uri);
        }
    }

    ManifestPatch {
        new_sources,
        new_units,
    }
}

/// Provenance-aware reconcile (skill-manager v1.3, bead `ai-ya9`).
///
/// Runs the legacy [`reconcile`] to build the base patch, then —
/// **only when `sources` carries provenance data** — rewrites any
/// `local:` orphan unit whose [`provenance::attribute`] resolves to
/// [`provenance::Provenance::ExternalRepo`] so it is tracked as its
/// `gh:` upstream rather than a local orphan.
///
/// ## v1.2 byte-stability guarantee
///
/// With an empty [`provenance::ProvenanceSources`] (the v1.2 caller
/// shape — no `external-dependencies.yaml`, no `installed_plugins.json`,
/// no manifest units) this function is **byte-identical** to
/// [`reconcile`]: `attribute()` returns no `ExternalRepo` matches, so
/// the post-process is a no-op. v1.2 lockfiles therefore round-trip
/// unchanged — the provenance gate is the only behavioural fork.
///
/// Pure / total / deterministic, same as [`reconcile`].
pub fn reconcile_with_sources(
    walker_out: &WalkerOutput,
    sources: &super::provenance::ProvenanceSources,
) -> ManifestPatch {
    let mut patch = reconcile(walker_out);

    // Attribute the class-C orphans; we only act on ExternalRepo.
    let attributed = super::provenance::attribute(&[], &walker_out.class_c, sources);
    let external: HashMap<&str, &super::provenance::AttributedUnit> = attributed
        .iter()
        .filter(|u| {
            matches!(
                u.provenance,
                super::provenance::Provenance::ExternalRepo { .. }
            )
        })
        .map(|u| (u.name.as_str(), u))
        .collect();

    if external.is_empty() {
        // Fast path = v1.2 byte-stable. No rewriting whatsoever.
        return patch;
    }

    rewrite_external_local_units(&mut patch, &external);
    patch
}

/// Rewrite `local:` orphan units in `patch` whose name resolves to an
/// `ExternalRepo` provenance into `gh:<repo-without-scheme>@<ref>/<path>`
/// URIs, dropping any now-orphaned `local:` source and adding the
/// matching `gh:` source.
fn rewrite_external_local_units(
    patch: &mut ManifestPatch,
    external: &HashMap<&str, &super::provenance::AttributedUnit>,
) {
    use super::provenance::Provenance;

    let mut new_gh_sources: Vec<SourceEntry> = Vec::new();
    let mut gh_sources_seen: BTreeSet<String> = BTreeSet::new();
    let mut local_source_uris_drained: BTreeSet<String> = BTreeSet::new();

    let mut rewritten: Vec<UnitEntry> = Vec::with_capacity(patch.new_units.len());
    for unit in patch.new_units.drain(..) {
        if !unit.uri.starts_with("local:") {
            rewritten.push(unit);
            continue;
        }
        let Some(name) = unit.uri.rsplit('/').next() else {
            rewritten.push(unit);
            continue;
        };
        let Some(att) = external.get(name) else {
            rewritten.push(unit);
            continue;
        };
        let Provenance::ExternalRepo { repo, version, .. } = &att.provenance else {
            rewritten.push(unit);
            continue;
        };

        // Track the local source so we can drop it if now dangling.
        if let Some((local_src_uri, _)) = unit.uri.split_once('@') {
            local_source_uris_drained.insert(local_src_uri.to_string());
        }

        let repo_locator = repo_to_gh_locator(repo);
        let git_ref = version.clone().unwrap_or_else(|| "main".to_string());
        let new_uri = format!("gh:{repo_locator}@{git_ref}/skills/{name}");

        let source_uri = format!("gh:{repo_locator}");
        if gh_sources_seen.insert(source_uri.clone()) {
            new_gh_sources.push(SourceEntry {
                name: format!("gh-{}", repo_locator.replace('/', "-")),
                kind: Some("gh".into()),
                uri: source_uri,
                r#ref: git_ref.clone(),
                enabled: true,
                read_only: false,
                target_layout: Vec::new(),
            });
        }

        rewritten.push(UnitEntry {
            uri: new_uri,
            targets: unit.targets,
            shadowed_by: unit.shadowed_by,
        });
    }
    patch.new_units = rewritten;

    // Drop now-orphaned local: sources.
    if !local_source_uris_drained.is_empty() {
        let still_used: BTreeSet<String> = patch
            .new_units
            .iter()
            .filter_map(|u| u.uri.split_once('@').map(|(src, _)| src.to_string()))
            .collect();
        patch
            .new_sources
            .retain(|s| !local_source_uris_drained.contains(&s.uri) || still_used.contains(&s.uri));
    }

    patch.new_sources.extend(new_gh_sources);
}

/// Reduce an `external-dependencies.yaml` `repo` value to the
/// `owner/repo` locator a `gh:` URI expects, stripping a
/// `https://github.com/` prefix and a trailing `.git` when present.
/// A non-GitHub URL is passed through verbatim (still a valid `gh:`
/// locator string, just not shortened).
fn repo_to_gh_locator(repo: &str) -> String {
    let trimmed = repo
        .trim_start_matches("https://github.com/")
        .trim_start_matches("http://github.com/")
        .trim_start_matches("git@github.com:");
    trimmed.trim_end_matches(".git").to_string()
}

/// Per-tool canonical home prefix used to build `local:` URIs.
/// Returns `None` for unknown tools so callers skip emitting
/// nonsense rather than guess.
fn tool_home_prefix(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "claude" => "~/.claude",
        "codex" => "~/.codex",
        "copilot" => "~/.copilot",
        "gemini" => "~/.gemini",
        "cursor" => "~/.cursor",
        "amazonq" => "~/.aws/amazonq",
        "claude-desktop" => "~/Library/Application Support/Claude",
        "cline" => "~/.cline",
        "roo" => "~/.roo",
        _ => return None,
    })
}

/// Map a class-C `UnitKind` to the on-disk subdir name used by the
/// tool adapters (`skills/`, `plugins/`, etc.).
fn unit_kind_subdir(k: UnitKind) -> &'static str {
    match k {
        UnitKind::Skill => "skills",
        UnitKind::Plugin => "plugins",
        UnitKind::Agent => "agents",
        UnitKind::Command => "commands",
        UnitKind::Hook => "hooks",
        UnitKind::McpServer => "mcp-servers",
        UnitKind::Statusline => "statuslines",
    }
}

/// Class-A only carries Skill/Agent. Map to its bundled subdir
/// inside the marketplace plugin layout.
fn class_a_unit_subdir(k: DiscoveredUnitKind) -> &'static str {
    match k {
        DiscoveredUnitKind::Skill => "skills",
        DiscoveredUnitKind::Agent => "agents",
    }
}

/// Stable, human-readable source name for a `local:` source.
/// Mirrors the URI structure so list output is predictable.
fn local_source_name(tool: &str, subdir: &str) -> String {
    format!("local-{tool}-{subdir}")
}

#[cfg(test)]
mod tests {
    use super::super::class_a::DiscoveredUnit;
    use super::*;

    use std::path::PathBuf;

    fn orphan(tool: &str, kind: UnitKind, name: &str) -> DiscoveredOrphanUnit {
        DiscoveredOrphanUnit {
            tool: tool.to_string(),
            kind,
            name: name.to_string(),
            path: PathBuf::from(format!(
                "/tmp/fixture/{tool}/{}/{name}",
                unit_kind_subdir(kind)
            )),
            frontmatter_valid: true,
        }
    }

    fn marketplace_plugin(
        plugin: &str,
        marketplace: &str,
        version: &str,
        unit_kinds_and_names: &[(DiscoveredUnitKind, &str)],
    ) -> DiscoveredMarketplaceUnit {
        DiscoveredMarketplaceUnit {
            plugin: plugin.to_string(),
            marketplace: marketplace.to_string(),
            version: version.to_string(),
            units: unit_kinds_and_names
                .iter()
                .map(|(k, n)| DiscoveredUnit {
                    kind: *k,
                    name: (*n).to_string(),
                    path: PathBuf::from(format!(
                        "/tmp/fixture/cache/{marketplace}/{plugin}/{version}/{}/{n}",
                        class_a_unit_subdir(*k)
                    )),
                })
                .collect(),
        }
    }

    fn find_unit<'a>(patch: &'a ManifestPatch, uri: &str) -> &'a UnitEntry {
        patch
            .new_units
            .iter()
            .find(|u| u.uri == uri)
            .unwrap_or_else(|| panic!("missing unit `{uri}` in {patch:#?}"))
    }

    fn find_source<'a>(patch: &'a ManifestPatch, uri: &str) -> &'a SourceEntry {
        patch
            .new_sources
            .iter()
            .find(|s| s.uri == uri)
            .unwrap_or_else(|| panic!("missing source `{uri}` in {patch:#?}"))
    }

    // ---- empty / smoke ------------------------------------------

    #[test]
    fn empty_input_yields_empty_patch() {
        let out = reconcile(&WalkerOutput::default());
        assert!(out.new_sources.is_empty());
        assert!(out.new_units.is_empty());
    }

    // ---- class-C only -------------------------------------------

    #[test]
    fn c_only_no_shadow() {
        let walker = WalkerOutput {
            class_c: vec![orphan("claude", UnitKind::Skill, "commit")],
            ..Default::default()
        };
        let patch = reconcile(&walker);
        assert_eq!(patch.new_sources.len(), 1);
        assert_eq!(patch.new_units.len(), 1);
        let unit = find_unit(&patch, "local:~/.claude/skills@head/commit");
        assert_eq!(unit.targets.as_deref(), Some(&["claude".to_string()][..]));
        assert!(unit.shadowed_by.is_none());
        let src = find_source(&patch, "local:~/.claude/skills");
        assert_eq!(src.r#ref, "head");
        assert!(!src.read_only);
        assert!(src.enabled);
    }

    #[test]
    fn c_only_multiple_orphans_same_source_dedupe_to_one_source() {
        let walker = WalkerOutput {
            class_c: vec![
                orphan("claude", UnitKind::Skill, "a"),
                orphan("claude", UnitKind::Skill, "b"),
                orphan("claude", UnitKind::Skill, "c"),
            ],
            ..Default::default()
        };
        let patch = reconcile(&walker);
        assert_eq!(patch.new_sources.len(), 1, "one source per (tool, subdir)");
        assert_eq!(patch.new_units.len(), 3);
    }

    #[test]
    fn c_only_multiple_subdirs_emit_one_source_each() {
        let walker = WalkerOutput {
            class_c: vec![
                orphan("claude", UnitKind::Skill, "a"),
                orphan("claude", UnitKind::Agent, "b"),
                orphan("claude", UnitKind::Hook, "c"),
            ],
            ..Default::default()
        };
        let patch = reconcile(&walker);
        assert_eq!(patch.new_sources.len(), 3);
        assert!(patch.new_sources.iter().any(|s| s.uri == "local:~/.claude/skills"));
        assert!(patch.new_sources.iter().any(|s| s.uri == "local:~/.claude/agents"));
        assert!(patch.new_sources.iter().any(|s| s.uri == "local:~/.claude/hooks"));
    }

    #[test]
    fn c_only_amazonq_uses_aws_subdir_in_uri() {
        let walker = WalkerOutput {
            class_c: vec![orphan("amazonq", UnitKind::Skill, "x")],
            ..Default::default()
        };
        let patch = reconcile(&walker);
        let unit = find_unit(&patch, "local:~/.aws/amazonq/skills@head/x");
        assert_eq!(unit.targets.as_deref(), Some(&["amazonq".to_string()][..]));
    }

    // ---- class-A only -------------------------------------------

    #[test]
    fn a_only_no_shadow() {
        let walker = WalkerOutput {
            class_a: vec![marketplace_plugin(
                "reflect",
                "claude-plugins-official",
                "v1.0",
                &[(DiscoveredUnitKind::Skill, "commit")],
            )],
            ..Default::default()
        };
        let patch = reconcile(&walker);
        assert_eq!(patch.new_sources.len(), 1);
        assert_eq!(patch.new_units.len(), 1);
        let src = find_source(&patch, "marketplace:claude-plugins-official");
        assert_eq!(src.kind.as_deref(), Some("claude-marketplace"));
        assert!(src.read_only);
        let unit = find_unit(
            &patch,
            "marketplace:reflect@claude-plugins-official/skills/commit",
        );
        assert!(unit.shadowed_by.is_none());
        assert_eq!(unit.targets.as_deref(), Some(&["claude".to_string()][..]));
    }

    #[test]
    fn a_only_multiple_plugins_share_marketplace_source() {
        let walker = WalkerOutput {
            class_a: vec![
                marketplace_plugin(
                    "reflect",
                    "claude-plugins-official",
                    "v1.0",
                    &[(DiscoveredUnitKind::Skill, "commit")],
                ),
                marketplace_plugin(
                    "burndown",
                    "claude-plugins-official",
                    "v0.5",
                    &[(DiscoveredUnitKind::Skill, "summarize")],
                ),
            ],
            ..Default::default()
        };
        let patch = reconcile(&walker);
        assert_eq!(
            patch.new_sources.len(),
            1,
            "both plugins live under the same marketplace source"
        );
        assert_eq!(patch.new_units.len(), 2);
    }

    #[test]
    fn a_only_two_marketplaces_emit_two_sources() {
        let walker = WalkerOutput {
            class_a: vec![
                marketplace_plugin(
                    "reflect",
                    "claude-plugins-official",
                    "v1.0",
                    &[(DiscoveredUnitKind::Skill, "commit")],
                ),
                marketplace_plugin(
                    "alt-reflect",
                    "third-party",
                    "v0.1",
                    &[(DiscoveredUnitKind::Skill, "summarise")],
                ),
            ],
            ..Default::default()
        };
        let patch = reconcile(&walker);
        assert_eq!(patch.new_sources.len(), 2);
        assert!(patch.new_sources.iter().any(|s| s.uri == "marketplace:claude-plugins-official"));
        assert!(patch.new_sources.iter().any(|s| s.uri == "marketplace:third-party"));
    }

    // ---- A + C conflict matrix ----------------------------------

    #[test]
    fn a_plus_c_same_name_in_claude_marks_a_shadowed_by_c() {
        let walker = WalkerOutput {
            class_a: vec![marketplace_plugin(
                "reflect",
                "claude-plugins-official",
                "v1.0",
                &[(DiscoveredUnitKind::Skill, "commit")],
            )],
            class_c: vec![orphan("claude", UnitKind::Skill, "commit")],
        };
        let patch = reconcile(&walker);
        let c_unit = find_unit(&patch, "local:~/.claude/skills@head/commit");
        assert!(c_unit.shadowed_by.is_none(), "orphan is the active winner");
        let a_unit = find_unit(
            &patch,
            "marketplace:reflect@claude-plugins-official/skills/commit",
        );
        let shadow = a_unit
            .shadowed_by
            .as_ref()
            .expect("marketplace unit must be shadowed by the orphan");
        assert_eq!(shadow.display(), "local:~/.claude/skills@head/commit");
    }

    #[test]
    fn a_plus_c_different_name_no_shadow() {
        let walker = WalkerOutput {
            class_a: vec![marketplace_plugin(
                "reflect",
                "claude-plugins-official",
                "v1.0",
                &[(DiscoveredUnitKind::Skill, "commit")],
            )],
            class_c: vec![orphan("claude", UnitKind::Skill, "different")],
        };
        let patch = reconcile(&walker);
        assert!(
            patch.new_units.iter().all(|u| u.shadowed_by.is_none()),
            "no name collision → nothing shadowed"
        );
        assert_eq!(patch.new_units.len(), 2);
    }

    #[test]
    fn a_plus_c_same_name_but_c_in_non_claude_tool_is_not_a_conflict() {
        // Marketplace plugins only deploy to ~/.claude/. An orphan
        // with the same name in ~/.codex/skills/ is independent
        // (different tool home).
        let walker = WalkerOutput {
            class_a: vec![marketplace_plugin(
                "reflect",
                "claude-plugins-official",
                "v1.0",
                &[(DiscoveredUnitKind::Skill, "commit")],
            )],
            class_c: vec![orphan("codex", UnitKind::Skill, "commit")],
        };
        let patch = reconcile(&walker);
        for unit in &patch.new_units {
            assert!(
                unit.shadowed_by.is_none(),
                "no shadow expected (different tool home): {unit:#?}"
            );
        }
        assert_eq!(patch.new_units.len(), 2);
    }

    #[test]
    fn a_plus_c_plus_c_cross_tool_only_claude_c_shadows_a() {
        // Marketplace "commit" plugin + claude orphan "commit" +
        // codex orphan "commit": A shadowed by claude-C, codex-C
        // independent.
        let walker = WalkerOutput {
            class_a: vec![marketplace_plugin(
                "reflect",
                "claude-plugins-official",
                "v1.0",
                &[(DiscoveredUnitKind::Skill, "commit")],
            )],
            class_c: vec![
                orphan("claude", UnitKind::Skill, "commit"),
                orphan("codex", UnitKind::Skill, "commit"),
            ],
        };
        let patch = reconcile(&walker);
        assert_eq!(patch.new_units.len(), 3);
        let a_unit = find_unit(
            &patch,
            "marketplace:reflect@claude-plugins-official/skills/commit",
        );
        let shadow = a_unit.shadowed_by.as_ref().unwrap();
        assert_eq!(shadow.display(), "local:~/.claude/skills@head/commit");
        let claude_c = find_unit(&patch, "local:~/.claude/skills@head/commit");
        assert!(claude_c.shadowed_by.is_none());
        let codex_c = find_unit(&patch, "local:~/.codex/skills@head/commit");
        assert!(codex_c.shadowed_by.is_none());
    }

    // ---- C + C across tools (not a conflict) --------------------

    #[test]
    fn c_plus_c_same_name_different_tools_both_active_two_units() {
        // Spec edge case: two orphans same name in different tool
        // homes → adopt as two separate units, neither shadowed.
        let walker = WalkerOutput {
            class_c: vec![
                orphan("claude", UnitKind::Skill, "commit"),
                orphan("codex", UnitKind::Skill, "commit"),
            ],
            ..Default::default()
        };
        let patch = reconcile(&walker);
        assert_eq!(patch.new_units.len(), 2);
        for unit in &patch.new_units {
            assert!(unit.shadowed_by.is_none(), "{unit:#?}");
        }
        assert!(patch.new_units.iter().any(|u| u.uri == "local:~/.claude/skills@head/commit"));
        assert!(patch.new_units.iter().any(|u| u.uri == "local:~/.codex/skills@head/commit"));
    }

    // ---- A + A across marketplaces (first-seen wins) ------------

    #[test]
    fn a_plus_a_same_unit_name_across_marketplaces_first_wins() {
        // Two plugins from different marketplaces ship a `commit`
        // skill. Filesystem can only realise one at
        // ~/.claude/skills/commit; reconciler picks the first
        // walker-output entry as the winner and shadows the second.
        let walker = WalkerOutput {
            class_a: vec![
                marketplace_plugin(
                    "reflect",
                    "mp-first",
                    "v1.0",
                    &[(DiscoveredUnitKind::Skill, "commit")],
                ),
                marketplace_plugin(
                    "alt-reflect",
                    "mp-second",
                    "v0.5",
                    &[(DiscoveredUnitKind::Skill, "commit")],
                ),
            ],
            ..Default::default()
        };
        let patch = reconcile(&walker);
        let first = find_unit(&patch, "marketplace:reflect@mp-first/skills/commit");
        assert!(first.shadowed_by.is_none(), "first-seen marketplace wins");
        let second = find_unit(&patch, "marketplace:alt-reflect@mp-second/skills/commit");
        let shadow = second.shadowed_by.as_ref().unwrap();
        assert_eq!(
            shadow.display(),
            "marketplace:reflect@mp-first/skills/commit"
        );
    }

    #[test]
    fn a_plus_c_plus_a_c_active_both_a_shadowed_by_c() {
        // Edge case: C present in claude AND two A's with the same
        // unit name. Active winner is C (orphan wins by default);
        // BOTH A's get shadowed_by = C.uri, not chained A1 → A2.
        let walker = WalkerOutput {
            class_a: vec![
                marketplace_plugin(
                    "reflect",
                    "mp-first",
                    "v1.0",
                    &[(DiscoveredUnitKind::Skill, "commit")],
                ),
                marketplace_plugin(
                    "alt-reflect",
                    "mp-second",
                    "v0.5",
                    &[(DiscoveredUnitKind::Skill, "commit")],
                ),
            ],
            class_c: vec![orphan("claude", UnitKind::Skill, "commit")],
        };
        let patch = reconcile(&walker);
        let c_uri = "local:~/.claude/skills@head/commit";
        for a_uri in [
            "marketplace:reflect@mp-first/skills/commit",
            "marketplace:alt-reflect@mp-second/skills/commit",
        ] {
            let unit = find_unit(&patch, a_uri);
            assert_eq!(
                unit.shadowed_by.as_ref().unwrap().display(),
                c_uri,
                "both A's should chain to the C, not to each other"
            );
        }
    }

    // ---- determinism --------------------------------------------

    #[test]
    fn deterministic_repeated_calls_produce_identical_patches() {
        let walker = WalkerOutput {
            class_a: vec![marketplace_plugin(
                "reflect",
                "claude-plugins-official",
                "v1.0",
                &[
                    (DiscoveredUnitKind::Skill, "commit"),
                    (DiscoveredUnitKind::Agent, "reviewer"),
                ],
            )],
            class_c: vec![
                orphan("claude", UnitKind::Skill, "commit"),
                orphan("codex", UnitKind::Skill, "summarize"),
            ],
        };
        let a = reconcile(&walker);
        let b = reconcile(&walker);
        assert_eq!(a, b);
    }

    // ---- side-effect-free (no fs, no env) -----------------------

    #[test]
    fn reconciler_does_not_consult_env_vars() {
        // Set an unrelated AINB_TOOL_HOME_CLAUDE; the reconciler
        // must not read it (URIs use canonical ~/.claude regardless).
        let prev = std::env::var("AINB_TOOL_HOME_CLAUDE").ok();
        std::env::set_var("AINB_TOOL_HOME_CLAUDE", "/tmp/should-not-be-read");
        let walker = WalkerOutput {
            class_c: vec![orphan("claude", UnitKind::Skill, "x")],
            ..Default::default()
        };
        let patch = reconcile(&walker);
        // Restore env.
        match prev {
            Some(v) => std::env::set_var("AINB_TOOL_HOME_CLAUDE", v),
            None => std::env::remove_var("AINB_TOOL_HOME_CLAUDE"),
        }
        let unit = find_unit(&patch, "local:~/.claude/skills@head/x");
        assert!(unit.shadowed_by.is_none());
    }

    // ---- unknown-tool defensive guard ---------------------------

    #[test]
    fn unknown_tool_orphan_is_skipped() {
        let walker = WalkerOutput {
            class_c: vec![orphan("not-a-real-tool", UnitKind::Skill, "x")],
            ..Default::default()
        };
        let patch = reconcile(&walker);
        assert!(patch.new_sources.is_empty());
        assert!(patch.new_units.is_empty());
    }
}
