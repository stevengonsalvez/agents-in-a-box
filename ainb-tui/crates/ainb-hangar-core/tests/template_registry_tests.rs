//! P6.3 — embedded curated `agent_template` registry tests.
//!
//! These cover the **pure** (IO-free) half of P6.3: the 10 curated templates
//! baked into [`ainb_hangar_core::template`] via `include_str!`, their required
//! fields, and the invariant that every referenced skill name resolves to a real
//! `toolkit/packages/skills/<name>/SKILL.md` (the same invariant `build.rs`
//! enforces at compile time — this test is the runtime mirror so a regression is
//! caught even if the build cache is stale).
//!
//! The transactional `templates_use` (create agent → import skills → junction in
//! one transaction) lives in `ainb-hangar-daemon` because it needs sqlx + the
//! store repos; it is tested in
//! `crates/ainb-hangar-daemon/tests/template_use_tests.rs`.

use ainb_hangar_core::template::TemplateRegistry;

/// The repo-root-relative path to the curated toolkit skills, resolved from this
/// crate's manifest dir (`crates/ainb-hangar-core`) up to the workspace parent.
fn toolkit_skills_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../toolkit/packages/skills")
}

#[test]
fn test_embedded_templates_count() {
    assert_eq!(
        TemplateRegistry::load_embedded().len(),
        10,
        "exactly 10 curated templates must be embedded"
    );
}

#[test]
fn test_each_template_has_required_fields() {
    for t in TemplateRegistry::load_embedded() {
        assert!(!t.name.is_empty(), "template name must be non-empty");
        assert!(
            !t.description.is_empty(),
            "template `{}` description must be non-empty",
            t.name
        );
        assert!(
            !t.instructions.is_empty(),
            "template `{}` instructions must be non-empty",
            t.name
        );
        assert!(
            !t.skills.is_empty(),
            "template `{}` must bundle at least one skill",
            t.name
        );
        assert!(
            !t.agent_md_path.is_empty(),
            "template `{}` must map to an agent .md path",
            t.name
        );
    }
}

#[test]
fn test_template_skill_refs_resolve_to_toolkit_dir() {
    let dir = toolkit_skills_dir();
    for t in TemplateRegistry::load_embedded() {
        for skill in &t.skills {
            let skill_md = dir.join(skill).join("SKILL.md");
            assert!(
                skill_md.is_file(),
                "template `{}` references skill `{}` but {} does not exist \
                 (build.rs must also fail on this)",
                t.name,
                skill,
                skill_md.display()
            );
        }
    }
}

#[test]
fn test_registry_get_and_list_agree() {
    let registry = TemplateRegistry::load_embedded();
    // `list` returns every template; `get(name)` resolves each by name.
    for t in &registry {
        let got = TemplateRegistry::get(t.name.as_str())
            .unwrap_or_else(|| panic!("get({}) returned None", t.name));
        assert_eq!(got.name, t.name);
        assert_eq!(got.skills, t.skills);
    }
    // An unknown name resolves to None (not a panic / empty template).
    assert!(
        TemplateRegistry::get("no-such-template").is_none(),
        "unknown template name must resolve to None"
    );
}

#[test]
fn test_code_reviewer_template_is_present_and_shaped() {
    let t = TemplateRegistry::get("code-reviewer").expect("code-reviewer template present");
    assert_eq!(t.name, "code-reviewer");
    assert!(
        t.skills.iter().any(|s| s == "commit"),
        "code-reviewer must bundle the `commit` skill"
    );
    assert!(
        t.skills.iter().any(|s| s == "find-missing-tests"),
        "code-reviewer must bundle the `find-missing-tests` skill"
    );
}
