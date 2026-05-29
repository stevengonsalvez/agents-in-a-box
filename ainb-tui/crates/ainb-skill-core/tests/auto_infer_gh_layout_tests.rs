//! `target_layout` auto-infer for `gh:owner/repo` URIs — bead v12.D.6.
//!
//! C.3's `resolve_pair` already falls back to
//! `BOOTSTRAP_DEFAULT_MAPPINGS` when a source's `target_layout` is
//! empty. This bead confirms that the fallback fires correctly for
//! the gh-URI shape `gh:owner/repo` — the canonical case for v1.2's
//! "drop your gh URI in, get sensible defaults" UX. The confirmation
//! is explicit so a regression that narrows the fallback (e.g.
//! gating it behind an opt-in flag, or restricting it to a different
//! URI scheme) trips a named tripwire instead of silently changing
//! the install path for every legacy source.

use std::path::PathBuf;

use ainb_skill_core::manifest::SourceEntry;
use ainb_skill_core::resolve_pair;

fn gh_source(uri: &str) -> SourceEntry {
    SourceEntry {
        name: "auto-infer".into(),
        kind: Some("gh".into()),
        uri: uri.to_string(),
        r#ref: "main".into(),
        enabled: true,
        read_only: false,
        // Empty target_layout — the auto-infer path is what we are
        // exercising.
        target_layout: Vec::new(),
    }
}

#[test]
fn gh_skills_path_uses_bootstrap_default_mapping() {
    let source = gh_source("gh:stevengonsalvez/my-skills");
    let unit = PathBuf::from("skills/commit/SKILL.md");
    let (home, repo) = resolve_pair(&source, &unit).expect("default mapping must match");
    assert_eq!(
        home,
        PathBuf::from(".claude/skills/commit/SKILL.md"),
        "skills glob → ~/.claude/skills/<name>/SKILL.md"
    );
    assert_eq!(
        repo,
        PathBuf::from("skills/commit/SKILL.md"),
        "skills glob → repo root skills/<name>/SKILL.md"
    );
}

#[test]
fn gh_agents_categorised_path_uses_bootstrap_default_mapping() {
    let source = gh_source("gh:stevengonsalvez/my-agents");
    let unit = PathBuf::from("agents/engineering/code-reviewer.md");
    let (home, repo) = resolve_pair(&source, &unit).expect("default mapping must match");
    // Static prefix is "agents"; the trailing
    // {engineering,...}/code-reviewer.md falls through to both
    // sides verbatim — that mirrors bootstrap.js's mirror-the-source
    // layout under `~/.claude/agents/<cat>/<name>.md`.
    assert_eq!(
        home,
        PathBuf::from(".claude/agents/engineering/code-reviewer.md")
    );
    assert_eq!(repo, PathBuf::from("agents/engineering/code-reviewer.md"));
}

#[test]
fn gh_top_level_agent_file_uses_bootstrap_default_mapping() {
    let source = gh_source("gh:owner/repo");
    let unit = PathBuf::from("agents/distinguished-engineer.md");
    let (home, repo) = resolve_pair(&source, &unit).expect("default mapping must match");
    assert_eq!(home, PathBuf::from(".claude/agents/distinguished-engineer.md"));
    assert_eq!(repo, PathBuf::from("agents/distinguished-engineer.md"));
}

#[test]
fn gh_commands_path_uses_bootstrap_default_mapping() {
    let source = gh_source("gh:owner/repo");
    let unit = PathBuf::from("commands/prime.md");
    let (home, repo) = resolve_pair(&source, &unit).expect("default mapping must match");
    assert_eq!(home, PathBuf::from(".claude/commands/prime.md"));
    assert_eq!(repo, PathBuf::from("commands/prime.md"));
}

#[test]
fn gh_path_outside_bootstrap_defaults_returns_none() {
    let source = gh_source("gh:owner/repo");
    let unit = PathBuf::from("random/unmapped/file.md");
    assert!(
        resolve_pair(&source, &unit).is_none(),
        "paths outside the bootstrap layout must NOT auto-infer"
    );
}

#[test]
fn explicit_target_layout_overrides_bootstrap_defaults() {
    // Sanity-check: a source that DOES declare target_layout uses
    // only its own mappings — the bootstrap fallback must not
    // double-apply on top.
    use ainb_skill_core::manifest::TargetMapping;
    let mut source = gh_source("gh:owner/repo");
    source.target_layout = vec![TargetMapping {
        glob: "custom/**".into(),
        home: PathBuf::from(".my-tool/custom"),
        repo: PathBuf::from("src/custom"),
    }];
    let unit = PathBuf::from("custom/foo.md");
    let (home, repo) = resolve_pair(&source, &unit).expect("custom mapping must match");
    assert_eq!(home, PathBuf::from(".my-tool/custom/foo.md"));
    assert_eq!(repo, PathBuf::from("src/custom/foo.md"));

    // And explicit layout means the bootstrap defaults do not fire,
    // even when the unit path looks like a canonical skills layout.
    let unit_skills = PathBuf::from("skills/commit/SKILL.md");
    assert!(
        resolve_pair(&source, &unit_skills).is_none(),
        "bootstrap defaults must NOT silently augment an explicit \
         target_layout"
    );
}
