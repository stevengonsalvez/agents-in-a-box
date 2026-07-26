//! P4.6 RED — skill-manager reducer behaviour.
//!
//! The skill manager (hotkey `3`) is a three-pane screen: a skill list, a file
//! tree for the selected skill, and a read-only editor pane. These tests pin the
//! pure reducer contract: list navigation, loading a skill's file tree, the
//! import intent, the Used/Unused filter chips, and the remote-conflict banner
//! that only fires when the local copy is dirty.

use ainb_hangar_proto::events::{HangarEvent, SkillFile, SkillRow};
use ainb_plugin_hangar::screen::skill_manager::{
    SkillFilter, SkillManagerEvent, SkillManagerIntent, SkillManagerState, reduce_skill_manager,
};

fn skill(slug: &str, name: &str, used: bool, updated_at: i64) -> SkillRow {
    SkillRow {
        slug: slug.into(),
        name: name.into(),
        used,
        updated_at,
    }
}

fn skills() -> Vec<SkillRow> {
    vec![
        skill("commit", "Commit", true, 100),
        skill("handover", "Handover", true, 100),
        skill("orphan", "Orphan", false, 100),
    ]
}

fn files() -> Vec<SkillFile> {
    vec![
        SkillFile {
            path: "SKILL.md".into(),
        },
        SkillFile {
            path: "assets/template.md".into(),
        },
        SkillFile {
            path: "assets/checklist.md".into(),
        },
    ]
}

fn state() -> SkillManagerState {
    SkillManagerState::new(skills())
}

/// j/k move the selection within the skill list.
#[test]
fn j_k_navigates_skill_list() {
    let s = state();
    assert_eq!(s.selected_index(), 0);
    let down = reduce_skill_manager(&s, SkillManagerEvent::Key('j')).state;
    assert_eq!(down.selected_index(), 1);
    let up = reduce_skill_manager(&down, SkillManagerEvent::Key('k')).state;
    assert_eq!(up.selected_index(), 0);
}

/// Enter on a skill emits a load-files intent for that skill, and folding the
/// returned file list populates the tree.
#[test]
fn enter_on_skill_loads_file_tree() {
    let s = state();
    let out = reduce_skill_manager(&s, SkillManagerEvent::Key('\n'));
    assert_eq!(
        out.intent,
        Some(SkillManagerIntent::LoadDetail("commit".into()))
    );
    // The daemon replies with the detail (body + files); folding it populates
    // the tree (P6.5: Enter opens the detail pane via `hangar/skill_get`).
    let loaded = reduce_skill_manager(
        &out.state,
        SkillManagerEvent::DetailLoaded {
            slug: "commit".into(),
            body: "# Commit\n".into(),
            files: files(),
        },
    )
    .state;
    assert_eq!(loaded.files().len(), 3);
    assert_eq!(loaded.detail_body(), Some("# Commit\n"));
}

/// `s` emits the sync intent (`hangar/skills_sync`, P6.5).
#[test]
fn s_emits_sync_intent() {
    let s = state();
    let out = reduce_skill_manager(&s, SkillManagerEvent::Key('s'));
    assert_eq!(out.intent, Some(SkillManagerIntent::Sync));
}

/// `i` attaches the selected skill to the selected agent (P6.5).
#[test]
fn i_emits_attach_intent_for_selected_skill() {
    let s = state();
    let out = reduce_skill_manager(&s, SkillManagerEvent::Key('i'));
    assert_eq!(
        out.intent,
        Some(SkillManagerIntent::Attach("commit".into()))
    );
}

/// `d` detaches the selected skill from the selected agent (P6.5).
#[test]
fn d_emits_detach_intent_for_selected_skill() {
    let s = state();
    let out = reduce_skill_manager(&s, SkillManagerEvent::Key('d'));
    assert_eq!(
        out.intent,
        Some(SkillManagerIntent::Detach("commit".into()))
    );
}

/// The `Used` filter chip hides orphan (unused) skills.
#[test]
fn filter_used_hides_orphan_skills() {
    let mut s = state();
    assert_eq!(s.visible_skills().len(), 3);
    s = reduce_skill_manager(&s, SkillManagerEvent::SetFilter(SkillFilter::Used)).state;
    let visible = s.visible_skills();
    assert_eq!(visible.len(), 2);
    assert!(visible.iter().all(|sk| sk.used));
    assert!(!visible.iter().any(|sk| sk.slug == "orphan"));
}

/// A remote `SkillUpdated` for a skill whose local copy is dirty surfaces the
/// conflict banner.
#[test]
fn event_skill_updated_remote_shows_conflict_banner_when_local_dirty() {
    let mut s = state();
    // Mark the selected skill (commit) locally dirty.
    s = reduce_skill_manager(&s, SkillManagerEvent::MarkDirty("commit".into())).state;
    let out = reduce_skill_manager(
        &s,
        SkillManagerEvent::Event(HangarEvent::SkillUpdated {
            skill: "commit".into(),
            updated_at: 200,
        }),
    );
    assert!(out.state.conflict_banner().is_some());
    assert!(out.state.conflict_banner().unwrap().contains("commit"));
}

/// A remote `SkillUpdated` for a clean local copy refreshes silently — no banner.
#[test]
fn event_skill_updated_remote_silent_when_local_clean() {
    let s = state();
    let out = reduce_skill_manager(
        &s,
        SkillManagerEvent::Event(HangarEvent::SkillUpdated {
            skill: "commit".into(),
            updated_at: 200,
        }),
    );
    assert!(out.state.conflict_banner().is_none());
}

/// Filtering down to `Unused` shows only orphan skills.
#[test]
fn filter_unused_shows_only_orphans() {
    let s = reduce_skill_manager(&state(), SkillManagerEvent::SetFilter(SkillFilter::Unused)).state;
    let visible = s.visible_skills();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].slug, "orphan");
}

// ---- Per-agent skill enablement (parity #24) --------------------------------

/// `t` raises the toggle intent for the SELECTED skill, not the first one.
#[test]
fn t_raises_toggle_enabled_intent_for_selected_skill() {
    let s = reduce_skill_manager(&state(), SkillManagerEvent::Key('j')).state;
    let out = reduce_skill_manager(&s, SkillManagerEvent::Key('t'));
    assert_eq!(
        out.intent,
        Some(SkillManagerIntent::ToggleEnabled("handover".into())),
        "t toggles the selected skill's link"
    );
}

/// Reserved-key invariant: `t` maps to the toggle and NOTHING else on this
/// screen. A future rebind of `t` collides here loudly instead of silently
/// stealing the toggle.
#[test]
fn t_is_unbound_elsewhere() {
    let s = state();
    for c in ['j', 'k', 's', 'i', 'd', '\n', '\r', 'r'] {
        let intent = reduce_skill_manager(&s, SkillManagerEvent::Key(c)).intent;
        assert_ne!(
            intent,
            Some(SkillManagerIntent::ToggleEnabled("commit".into())),
            "`{c}` must not also raise the toggle intent"
        );
    }
    // …and every OTHER skill-manager intent must not be reachable via `t`.
    let via_t = reduce_skill_manager(&s, SkillManagerEvent::Key('t')).intent;
    assert_eq!(
        via_t,
        Some(SkillManagerIntent::ToggleEnabled("commit".into())),
        "`t` raises exactly the toggle intent"
    );
}

/// `LinksLoaded` marks the disabled rows — and only those — in the rendered
/// card titles.
#[test]
fn links_loaded_marks_disabled_rows() {
    use ainb_hangar_proto::events::AgentSkillLinkRow;

    let s = reduce_skill_manager(
        &state(),
        SkillManagerEvent::LinksLoaded(vec![
            AgentSkillLinkRow {
                skill_id: "commit".into(),
                name: "Commit".into(),
                enabled: true,
            },
            AgentSkillLinkRow {
                skill_id: "handover".into(),
                name: "Handover".into(),
                enabled: false,
            },
        ]),
    )
    .state;

    assert!(!s.link_disabled("commit"), "an enabled link is not marked");
    assert!(s.link_disabled("handover"), "a disabled link is marked");
    assert!(
        !s.link_disabled("orphan"),
        "an unreported link is never rendered as disabled"
    );

    let cards = &s.board_columns()[0].cards;
    let title = |slug: &str| {
        cards
            .iter()
            .find(|c| c.issue_id == slug)
            .unwrap_or_else(|| panic!("no card for {slug}"))
            .title
            .clone()
    };
    assert!(
        !title("commit").contains("disabled"),
        "enabled row is unmarked: {}",
        title("commit")
    );
    assert!(
        title("handover").contains("disabled"),
        "disabled row carries the marker: {}",
        title("handover")
    );

    // A later reply that no longer lists `handover` (it was detached) must clear
    // the marker rather than leave it stale.
    let cleared = reduce_skill_manager(&s, SkillManagerEvent::LinksLoaded(vec![])).state;
    assert!(
        !cleared.link_disabled("handover"),
        "a wholesale reload drops markers for links that are gone"
    );
}
