//! P4.3 — Issue list screen (RED → GREEN reducer tests).
//!
//! These are the canonical reducer cases named in `docs/hangar/phases/P4.md`
//! under `hangar:P4.3`. The issue-list reducer is pure (no IO): it maps an
//! [`IssueListState`] plus an [`IssueListEvent`] to a new state and an optional
//! [`IssueListIntent`]. Rows are grouped by lifecycle status into the
//! Todo / In Progress / Done columns; filter chips narrow which rows show; host
//! [`HangarEvent`]s fold into the cached row set.

use ainb_hangar_core::ids::{AgentId, IssueId, TaskId};
use ainb_hangar_proto::events::{HangarEvent, IssueRow};
use ainb_plugin_hangar::screen::issue_list::{
    CreateWizard, FilterChip, IssueColumn, IssueListEvent, IssueListIntent, IssueListMode,
    IssueListState, WizardKey, reduce_issue_list,
};

/// A wire `IssueRow` for tests. `state` drives column grouping (`open` → Todo,
/// `in_progress` → In Progress, `done` → Done); `assignee` drives the
/// member/agent filter chips.
fn row(id: &str, state: &str, assignee: Option<&str>) -> IssueRow {
    IssueRow {
        id: IssueId::from_str(id).unwrap(),
        display_id: None,
        workspace_id: "ws".to_string(),
        title: format!("Issue {id}"),
        description: None,
        state: state.to_string(),
        assignee: assignee.map(ToString::to_string),
        creator: "member:alice".to_string(),
        created_at: 0,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
        pr_url: None,
        branch: None,
    }
}

/// A three-row fixture: two Todo, one In Progress, mixed assignees.
fn seeded_state() -> IssueListState {
    IssueListState::with_rows(vec![
        row("i1", "open", Some("member:alice")),
        row("i2", "open", Some("agent:claude")),
        row("i3", "in_progress", Some("agent:claude")),
    ])
}

/// `j` advances the row selection by one (vim-style down).
#[test]
fn j_key_moves_selection_down() {
    let s = seeded_state();
    assert_eq!(s.selected_index(), 0);

    let out = reduce_issue_list(&s, IssueListEvent::Key('j'));

    assert_eq!(out.state.selected_index(), 1);
    assert!(out.intent.is_none());
}

/// `enter` on the selected row emits an open-task-detail intent carrying that
/// row's issue id, so the router can route to the task-detail screen.
#[test]
fn enter_emits_open_task_detail_intent() {
    let s = seeded_state();

    let out = reduce_issue_list(&s, IssueListEvent::Key('\n'));

    assert_eq!(
        out.intent,
        Some(IssueListIntent::OpenTaskDetail(
            IssueId::from_str("i1").unwrap()
        ))
    );
}

/// The `Agents` filter chip hides rows assigned to a human member, leaving only
/// agent-assigned rows visible.
#[test]
fn filter_chip_agents_hides_human_assigned_rows() {
    let mut s = seeded_state();
    s = reduce_issue_list(&s, IssueListEvent::SetFilter(FilterChip::Agents)).state;

    let visible: Vec<&str> = s.visible_rows().map(|r| r.id.as_str()).collect();

    // i1 is member-assigned → hidden; i2 + i3 are agent-assigned → shown.
    assert!(!visible.contains(&"i1"), "member row leaked: {visible:?}");
    assert!(visible.contains(&"i2"), "agent row missing: {visible:?}");
    assert!(visible.contains(&"i3"), "agent row missing: {visible:?}");
}

/// `/` switches the screen into filter-input mode (the user can then type a
/// query); it does not emit an intent.
#[test]
fn slash_enters_filter_input_mode() {
    let s = seeded_state();
    assert_eq!(s.mode(), IssueListMode::Normal);

    let out = reduce_issue_list(&s, IssueListEvent::Key('/'));

    assert_eq!(out.state.mode(), IssueListMode::FilterInput);
    assert!(out.intent.is_none());
}

// ---------------------------------------------------------------------------
// Phase 5 — the staged create wizard (Title → Repo → SourceBranch →
// TargetBranch → Agent), which FORCES an agent assignment before any create
// can commit. The old title-only quick-create (an inert `◇ None` card) is
// unreachable from this screen.
// ---------------------------------------------------------------------------

/// Fold one wizard key.
fn wiz(
    s: &IssueListState,
    k: WizardKey,
) -> ainb_plugin_hangar::screen::issue_list::IssueListReduction {
    reduce_issue_list(s, IssueListEvent::Wizard(k))
}

/// Type a string into the open wizard stage, char by char.
fn type_str(mut s: IssueListState, text: &str) -> IssueListState {
    for ch in text.chars() {
        s = wiz(&s, WizardKey::Char(ch)).state;
    }
    s
}

/// `c` opens the create wizard on its Title stage; no intent yet.
#[test]
fn c_opens_wizard_on_title_stage() {
    let s = seeded_state();
    assert_eq!(s.mode(), IssueListMode::Normal);

    let out = reduce_issue_list(&s, IssueListEvent::Key('c'));

    assert_eq!(out.state.mode(), IssueListMode::CreateInput);
    assert!(matches!(
        out.state.wizard(),
        Some(CreateWizard::Title { title }) if title.is_empty()
    ));
    assert!(out.intent.is_none());
}

/// Enter on a blank/whitespace title HOLDS the Title stage open and raises no
/// intent — never an empty issue.
#[test]
fn wizard_blank_title_enter_holds() {
    let s = reduce_issue_list(&seeded_state(), IssueListEvent::Key('c')).state;
    let s = type_str(s, "  ");

    let out = wiz(&s, WizardKey::Enter);

    assert!(out.intent.is_none());
    assert!(matches!(
        out.state.wizard(),
        Some(CreateWizard::Title { .. })
    ));
    assert_eq!(out.state.mode(), IssueListMode::CreateInput);
}

/// Walk the wizard up to (but not into) the Agent stage: title typed, scratch
/// repo picked, both branch stages accepted at their `main` prefill.
fn wizard_at(stage: &str) -> IssueListState {
    let s = reduce_issue_list(&seeded_state(), IssueListEvent::Key('c')).state;
    let s = type_str(s, "Fix login");
    if stage == "title" {
        return s;
    }
    let s = wiz(&s, WizardKey::Enter).state; // → Repo
    if stage == "repo" {
        return s;
    }
    let s = wiz(&s, WizardKey::Char('@')).state; // open dropdown (cursor at scratch)
    let s = wiz(&s, WizardKey::Enter).state; // pick scratch → SourceBranch
    if stage == "source" {
        return s;
    }
    let s = wiz(&s, WizardKey::Enter).state; // accept "main" → TargetBranch
    if stage == "target" {
        return s;
    }
    wiz(&s, WizardKey::Enter).state // accept "main" → Agent
}

/// Repo is REQUIRED: Enter with the dropdown closed RE-OPENS the dropdown (the
/// cursor at scratch) instead of ever advancing repo-less.
#[test]
fn wizard_repo_enter_repo_less_reopens_dropdown() {
    let s = wizard_at("repo");
    assert!(matches!(
        s.wizard(),
        Some(CreateWizard::Repo { dropdown: None, .. })
    ));

    let out = wiz(&s, WizardKey::Enter);

    // Still on the Repo stage — now with the dropdown open at scratch.
    assert!(matches!(
        out.state.wizard(),
        Some(CreateWizard::Repo {
            dropdown: Some(0),
            ..
        })
    ));
    assert!(out.intent.is_none());
}

/// The branch stages prefill `main` (source first, then target).
#[test]
fn wizard_branch_stages_prefill_main() {
    let s = wizard_at("source");
    assert!(matches!(
        s.wizard(),
        Some(CreateWizard::SourceBranch { branch, .. }) if branch == "main"
    ));

    let s = wiz(&s, WizardKey::Enter).state;
    assert!(matches!(
        s.wizard(),
        Some(CreateWizard::TargetBranch { branch, source_branch, .. })
            if branch == "main" && source_branch == "main"
    ));
}

/// The full walk commits ONLY on the Agent stage, emitting `CreateAndRun` with
/// every collected field — and the agent token is always a real provider.
#[test]
fn wizard_full_walk_commits_create_and_run_with_agent() {
    let s = wizard_at("agent");
    assert!(matches!(s.wizard(), Some(CreateWizard::Agent { .. })));

    // ↓ once: claude → codex.
    let s = wiz(&s, WizardKey::Down).state;
    let out = wiz(&s, WizardKey::Enter);

    assert_eq!(
        out.intent,
        Some(IssueListIntent::CreateAndRun {
            title: "Fix login".to_string(),
            repo_ref: "scratch".to_string(),
            source_branch: Some("main".to_string()),
            target_branch: Some("main".to_string()),
            agent: "codex".to_string(),
        })
    );
    // The wizard closed and the screen is back to normal navigation.
    assert_eq!(out.state.mode(), IssueListMode::Normal);
    assert!(out.state.wizard().is_none());
}

/// Editing the source branch to a custom ref carries it through; blanking a
/// branch input means "unset" (`None` on the intent).
#[test]
fn wizard_branch_edits_and_blanks_round_trip() {
    // Clear the "main" prefill on source, type a feature ref.
    let s = wizard_at("source");
    let s = (0..4).fold(s, |s, _| wiz(&s, WizardKey::Backspace).state);
    let s = type_str(s, "feature/x");
    let s = wiz(&s, WizardKey::Enter).state;
    // Blank the target prefill entirely (→ unset).
    let s = (0..4).fold(s, |s, _| wiz(&s, WizardKey::Backspace).state);
    let s = wiz(&s, WizardKey::Enter).state;

    let out = wiz(&s, WizardKey::Enter); // commit on claude (cursor 0)

    assert_eq!(
        out.intent,
        Some(IssueListIntent::CreateAndRun {
            title: "Fix login".to_string(),
            repo_ref: "scratch".to_string(),
            source_branch: Some("feature/x".to_string()),
            target_branch: None,
            agent: "claude".to_string(),
        })
    );
}

/// The commit intent is IMPOSSIBLE before the Agent stage: Enter on every
/// earlier stage only holds, advances one stage, or re-opens the repo dropdown —
/// it NEVER emits `CreateAndRun`. Combined with the full-walk test (whose commit
/// fires only FROM the Agent stage) this proves no path yields an agent-less
/// create: the intent constructor lives solely on the Agent stage's Enter.
#[test]
fn wizard_never_commits_before_agent_stage() {
    for stage in ["title", "repo", "source", "target"] {
        let s = wizard_at(stage);
        let out = wiz(&s, WizardKey::Enter);
        assert!(
            !matches!(out.intent, Some(IssueListIntent::CreateAndRun { .. })),
            "stage {stage:?} must never emit CreateAndRun on Enter"
        );
        // Every pre-agent Enter keeps the wizard open (a hold or an advance) —
        // the only Enter that CLOSES the wizard is the Agent-stage commit.
        assert!(
            out.state.wizard().is_some(),
            "stage {stage:?} Enter must keep the wizard open"
        );
    }
    // And the one commit that IS reachable always carries a real provider token.
    let out = wiz(&wizard_at("agent"), WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun { agent, .. }) => {
            assert!(["claude", "codex", "copilot"].contains(&agent.as_str()));
        }
        other => panic!("agent-stage Enter must commit CreateAndRun, got {other:?}"),
    }
}

/// Esc cancels the WHOLE wizard from every stage in ONE press — back to normal
/// navigation, nothing retained, no intent (the user is never trapped).
#[test]
fn wizard_esc_cancels_whole_overlay_from_every_stage() {
    for stage in ["title", "repo", "source", "target", "agent"] {
        let s = wizard_at(stage);
        let out = wiz(&s, WizardKey::Esc);
        assert_eq!(
            out.state.mode(),
            IssueListMode::Normal,
            "Esc from {stage:?} must return to Normal"
        );
        assert!(
            out.state.wizard().is_none(),
            "Esc from {stage:?} must drop the wizard"
        );
        assert!(out.intent.is_none());
    }
    // Esc with the repo DROPDOWN open is still a single-press whole-overlay
    // cancel (never a trapped inner state).
    let s = wizard_at("repo");
    let s = wiz(&s, WizardKey::Char('@')).state;
    let out = wiz(&s, WizardKey::Esc);
    assert_eq!(out.state.mode(), IssueListMode::Normal);
    assert!(out.state.wizard().is_none());
}

/// An `IssueCreated` host event lands the new row in the Todo column.
#[test]
fn event_issue_created_appears_in_todo_column() {
    let s = IssueListState::default();
    let new_row = row("i9", "open", None);

    let out = reduce_issue_list(
        &s,
        IssueListEvent::Event(HangarEvent::IssueCreated(new_row)),
    );

    let todo: Vec<&str> =
        out.state.rows_in_column(IssueColumn::Todo).map(|r| r.id.as_str()).collect();
    assert_eq!(todo, vec!["i9"]);
}

/// A `TaskStarted` host event promotes its issue out of Todo into In Progress.
#[test]
fn event_task_started_promotes_issue_to_in_progress() {
    // Seed one Todo issue, plus a queued task that maps a task id to that issue.
    let mut s = IssueListState::with_rows(vec![row("i1", "open", Some("agent:claude"))]);
    s = reduce_issue_list(
        &s,
        IssueListEvent::Event(HangarEvent::TaskQueued {
            task_id: TaskId::from_str("t1").unwrap(),
            issue_id: IssueId::from_str("i1").unwrap(),
            agent_id: AgentId::from_str("claude").unwrap(),
        }),
    )
    .state;

    let out = reduce_issue_list(
        &s,
        IssueListEvent::Event(HangarEvent::TaskStarted {
            task_id: TaskId::from_str("t1").unwrap(),
            started_at: chrono::Utc::now(),
        }),
    );

    // i1 left Todo …
    assert_eq!(out.state.rows_in_column(IssueColumn::Todo).count(), 0);
    // … and now sits in In Progress.
    let in_prog: Vec<&str> = out
        .state
        .rows_in_column(IssueColumn::InProgress)
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(in_prog, vec!["i1"]);
}
