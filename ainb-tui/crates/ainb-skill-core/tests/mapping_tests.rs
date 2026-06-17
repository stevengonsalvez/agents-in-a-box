//! MappingEngine (`resolve_pair`) tests — bead v12.C.2.
//!
//! `resolve_pair` is a pure resolver: given a source's `target_layout`
//! and a unit's repo-relative path, it returns the first matching
//! mapping's translated `(home, repo)` destinations, or `None`.

use std::path::{Path, PathBuf};

use ainb_skill_core::manifest::{SourceEntry, TargetMapping};
use ainb_skill_core::mapping::{BOOTSTRAP_DEFAULT_MAPPINGS, resolve_pair};

/// Build a `SourceEntry` carrying just the `target_layout` under test —
/// every other field is irrelevant to `resolve_pair`.
fn source_with(mappings: Vec<TargetMapping>) -> SourceEntry {
    SourceEntry {
        name: "test-source".into(),
        kind: Some("manifest".into()),
        uri: "gh:org/repo".into(),
        r#ref: "main".into(),
        enabled: true,
        read_only: false,
        target_layout: mappings,
    }
}

fn mapping(glob: &str, home: &str, repo: &str) -> TargetMapping {
    TargetMapping {
        glob: glob.into(),
        home: PathBuf::from(home),
        repo: PathBuf::from(repo),
    }
}

#[test]
fn empty_target_layout_unmapped_path_returns_none() {
    // With an empty target_layout, resolve_pair falls back to the
    // BOOTSTRAP_DEFAULT_MAPPINGS (see the C.3 tests below). A unit that
    // matches none of the defaults still resolves to None.
    let src = source_with(vec![]);
    assert_eq!(resolve_pair(&src, Path::new("hooks/pre-commit.sh")), None);
}

#[test]
fn single_match_translates_home_and_repo() {
    let src = source_with(vec![mapping(
        "skills/**",
        ".claude/skills",
        "toolkit/packages/skills",
    )]);

    let got = resolve_pair(&src, Path::new("skills/commit/SKILL.md"));
    assert_eq!(
        got,
        Some((
            PathBuf::from(".claude/skills/commit/SKILL.md"),
            PathBuf::from("toolkit/packages/skills/commit/SKILL.md"),
        )),
        "tail `commit/SKILL.md` must re-root under both home and repo"
    );
}

#[test]
fn single_segment_glob_translates_file() {
    // `commands/*.md` selects a single file directly under `commands`.
    let src = source_with(vec![mapping(
        "commands/*.md",
        ".claude/commands",
        "toolkit/packages/commands",
    )]);

    let got = resolve_pair(&src, Path::new("commands/prime.md"));
    assert_eq!(
        got,
        Some((
            PathBuf::from(".claude/commands/prime.md"),
            PathBuf::from("toolkit/packages/commands/prime.md"),
        ))
    );
}

#[test]
fn brace_alternation_glob_matches() {
    // bootstrap.js-parity pattern with `{a,b,c}` alternation.
    let src = source_with(vec![mapping(
        "agents/{engineering,universal,design}/*.md",
        ".claude/agents",
        "toolkit/packages/agents",
    )]);

    let got = resolve_pair(&src, Path::new("agents/engineering/code-reviewer.md"));
    assert_eq!(
        got,
        Some((
            PathBuf::from(".claude/agents/engineering/code-reviewer.md"),
            PathBuf::from("toolkit/packages/agents/engineering/code-reviewer.md"),
        ))
    );

    // A directory not in the alternation set does not match.
    assert_eq!(
        resolve_pair(&src, Path::new("agents/unknown/foo.md")),
        None,
        "directory outside the brace set must not match"
    );
}

#[test]
fn multiple_globs_first_match_wins() {
    // Two mappings whose globs both match the same unit_path. The first
    // in declaration order must win — deterministic resolution.
    let src = source_with(vec![
        mapping("skills/**", ".claude/skills", "first/skills"),
        mapping("skills/**", ".claude/skills-alt", "second/skills"),
    ]);

    let got = resolve_pair(&src, Path::new("skills/commit/SKILL.md"));
    assert_eq!(
        got,
        Some((
            PathBuf::from(".claude/skills/commit/SKILL.md"),
            PathBuf::from("first/skills/commit/SKILL.md"),
        )),
        "first declared mapping wins on overlap"
    );
}

#[test]
fn first_listed_glob_that_matches_wins_over_later_nonoverlapping() {
    // Order matters only among *matching* globs: a non-matching first
    // mapping is skipped, the matching second one resolves.
    let src = source_with(vec![
        mapping("commands/*.md", ".claude/commands", "repo/commands"),
        mapping("skills/**", ".claude/skills", "repo/skills"),
    ]);

    let got = resolve_pair(&src, Path::new("skills/commit/SKILL.md"));
    assert_eq!(
        got,
        Some((
            PathBuf::from(".claude/skills/commit/SKILL.md"),
            PathBuf::from("repo/skills/commit/SKILL.md"),
        ))
    );
}

#[test]
fn globstar_matches_top_level_file_zero_segments() {
    // `**/*.md` must catch a root-level file (the globstar matches zero
    // segments), translating with the full unit path as the tail.
    let src = source_with(vec![mapping("**/*.md", ".claude/docs", "toolkit/docs")]);

    assert_eq!(
        resolve_pair(&src, Path::new("readme.md")),
        Some((
            PathBuf::from(".claude/docs/readme.md"),
            PathBuf::from("toolkit/docs/readme.md"),
        ))
    );
    assert_eq!(
        resolve_pair(&src, Path::new("guide/intro.md")),
        Some((
            PathBuf::from(".claude/docs/guide/intro.md"),
            PathBuf::from("toolkit/docs/guide/intro.md"),
        ))
    );
}

#[test]
fn unit_path_under_no_glob_returns_none() {
    let src = source_with(vec![
        mapping("skills/**", ".claude/skills", "repo/skills"),
        mapping("commands/*.md", ".claude/commands", "repo/commands"),
    ]);

    assert_eq!(
        resolve_pair(&src, Path::new("hooks/pre-commit.sh")),
        None,
        "a unit under no configured glob resolves to None"
    );
}

#[test]
fn single_star_does_not_cross_path_separator() {
    // `*` matches within one segment only — it must not span `/`.
    let src = source_with(vec![mapping(
        "commands/*.md",
        ".claude/commands",
        "repo/commands",
    )]);

    // Nested file is two segments deep — `commands/*.md` should NOT match.
    assert_eq!(
        resolve_pair(&src, Path::new("commands/sub/deep.md")),
        None,
        "single-star glob must not cross a path separator"
    );
}

// === v12.C.3 — BOOTSTRAP_DEFAULT_MAPPINGS fallback ===

/// The six canonical agent subdirectories ported from bootstrap.js.
const AGENT_SUBDIRS: [&str; 6] = [
    "engineering",
    "universal",
    "orchestrators",
    "design",
    "meta",
    "swarm",
];

#[test]
fn default_mappings_cover_all_canonical_kinds() {
    let globs: Vec<&str> = BOOTSTRAP_DEFAULT_MAPPINGS.iter().map(|(g, _, _)| *g).collect();

    assert!(
        globs.iter().any(|g| g.starts_with("agents/")),
        "missing agents mapping: {globs:?}"
    );
    assert!(
        globs.iter().any(|g| g.starts_with("skills/")),
        "missing skills mapping: {globs:?}"
    );
    assert!(
        globs.iter().any(|g| g.starts_with("commands/")),
        "missing commands mapping: {globs:?}"
    );

    // The agents glob must enumerate every canonical subdir.
    let agents_glob = globs.iter().find(|g| g.starts_with("agents/")).expect("agents glob present");
    for sub in AGENT_SUBDIRS {
        assert!(
            agents_glob.contains(sub),
            "agents glob `{agents_glob}` missing subdir `{sub}`"
        );
    }
}

#[test]
fn empty_layout_falls_back_to_defaults_for_every_agent_subdir() {
    let src = source_with(vec![]); // no explicit target_layout → defaults
    for sub in AGENT_SUBDIRS {
        let unit = format!("agents/{sub}/code-reviewer.md");
        let got = resolve_pair(&src, Path::new(&unit));
        assert_eq!(
            got,
            Some((
                PathBuf::from(format!(".claude/agents/{sub}/code-reviewer.md")),
                PathBuf::from(format!("agents/{sub}/code-reviewer.md")),
            )),
            "default mapping should resolve agents/{sub}"
        );
    }
}

#[test]
fn empty_layout_falls_back_to_defaults_for_top_level_agents() {
    // Agents that live directly under `agents/` (no subdir), e.g.
    // distinguished-engineer.md — must still resolve via the defaults.
    let src = source_with(vec![]);
    assert_eq!(
        resolve_pair(&src, Path::new("agents/distinguished-engineer.md")),
        Some((
            PathBuf::from(".claude/agents/distinguished-engineer.md"),
            PathBuf::from("agents/distinguished-engineer.md"),
        )),
        "top-level agent files must resolve under the default agents mapping"
    );
}

#[test]
fn empty_layout_falls_back_to_defaults_for_skills() {
    let src = source_with(vec![]);
    assert_eq!(
        resolve_pair(&src, Path::new("skills/commit/SKILL.md")),
        Some((
            PathBuf::from(".claude/skills/commit/SKILL.md"),
            PathBuf::from("skills/commit/SKILL.md"),
        ))
    );
}

#[test]
fn empty_layout_falls_back_to_defaults_for_commands() {
    let src = source_with(vec![]);
    assert_eq!(
        resolve_pair(&src, Path::new("commands/prime.md")),
        Some((
            PathBuf::from(".claude/commands/prime.md"),
            PathBuf::from("commands/prime.md"),
        ))
    );
}

#[test]
fn explicit_layout_does_not_consult_defaults() {
    // A source WITH an explicit target_layout uses only its own mappings;
    // the bootstrap defaults are NOT a fallback in that case. Here the
    // explicit layout covers only `commands`, so a skills unit (which a
    // default WOULD match) must resolve to None.
    let src = source_with(vec![mapping(
        "commands/*.md",
        ".claude/commands",
        "commands",
    )]);
    assert_eq!(
        resolve_pair(&src, Path::new("skills/commit/SKILL.md")),
        None,
        "explicit layout must not fall through to bootstrap defaults"
    );
}

#[test]
fn resolve_pair_is_pure_and_repeatable() {
    // Same inputs → same output, no observable side effects.
    let src = source_with(vec![mapping(
        "skills/**",
        ".claude/skills",
        "toolkit/packages/skills",
    )]);
    let unit = Path::new("skills/commit/SKILL.md");
    let a = resolve_pair(&src, unit);
    let b = resolve_pair(&src, unit);
    assert_eq!(a, b);
}
