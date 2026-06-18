//! P6.3 — embedded curated `agent_template` registry tests.
//!
//! These cover the **pure** (IO-free) half of P6.3: the 10 curated templates
//! baked into [`ainb_hangar_core::template`] via `include_str!`, their required
//! fields, and the invariant that every referenced skill name resolves to a real
//! `<name>/SKILL.md` in the curated skill set.
//!
//! The curated skills live in the standalone `stevengonsalvez/ainb-toolkit`
//! repo (flattened `skills/<name>/SKILL.md`), not in a local `toolkit/`
//! checkout. So the skill-existence test resolves against
//! `AINB_TOOLKIT_SKILLS_DIR` (a fetched ainb-toolkit `skills/` dir, the same env
//! the hangar daemon's sync uses) when present, and **skips** otherwise. CI sets
//! it after cloning `ainb-toolkit@<pinned-tag>`; local runs without the env skip
//! the existence check (template *shape* is still enforced unconditionally).
//!
//! The transactional `templates_use` (create agent → import skills → junction in
//! one transaction) lives in `ainb-hangar-daemon` because it needs sqlx + the
//! store repos; it is tested in
//! `crates/ainb-hangar-daemon/tests/template_use_tests.rs`.

use ainb_hangar_core::template::TemplateRegistry;

/// The curated skills dir from a fetched ainb-toolkit checkout, via
/// `AINB_TOOLKIT_SKILLS_DIR`. `None` when unset — the existence check skips.
fn toolkit_skills_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("AINB_TOOLKIT_SKILLS_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir())
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
    let Some(dir) = toolkit_skills_dir() else {
        eprintln!(
            "skipping skill-existence check: AINB_TOOLKIT_SKILLS_DIR unset \
             (set it to a fetched ainb-toolkit skills/ dir to enforce)"
        );
        return;
    };
    for t in TemplateRegistry::load_embedded() {
        for skill in &t.skills {
            let skill_md = dir.join(skill).join("SKILL.md");
            assert!(
                skill_md.is_file(),
                "template `{}` references skill `{}` but {} does not exist \
                 in the fetched ainb-toolkit skills/ dir",
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
