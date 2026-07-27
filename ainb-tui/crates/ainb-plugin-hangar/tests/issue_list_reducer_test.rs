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
    FilterChip, IssueColumn, IssueListEvent, IssueListIntent, IssueListMode, IssueListState,
    WizardAgent, WizardKey, WizardRow, reduce_issue_list,
};

/// A wire `IssueRow` for tests. `state` drives column grouping (`open` → Todo,
/// `in_progress` → In Progress, `done` → Done); `assignee` drives the
/// member/agent filter chips.
fn row(id: &str, state: &str, assignee: Option<&str>) -> IssueRow {
    IssueRow {
        last_dispatch_reason: None,
        last_dispatch_detail: None,
        last_dispatch_at: None,
        origin_type: None,
        origin_id: None,
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
        repo_ref: None,
        agent: None,
        source_branch: None,
        target_branch: None,
        external_ref: None,
        run_count: 0,
        last_run_status: None,
        last_run_at: None,
        parent_id: None,
        child_total: 0,
        child_done: 0,
        acceptance_criteria: Vec::new(),
        acceptance: Vec::new(),
        context_refs: Vec::new(),
        dependencies: Vec::new(),
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
// Phase 5 — the single-form create wizard (Title / Repo / Source / Target /
// Agent all visible at once), which FORCES both a title and a repo (and always a
// default agent) before any create can commit. The old title-only quick-create
// (an inert `◇ None` card) is unreachable from this screen.
// ---------------------------------------------------------------------------

/// Fold one wizard key.
fn wiz(
    s: &IssueListState,
    k: WizardKey,
) -> ainb_plugin_hangar::screen::issue_list::IssueListReduction {
    reduce_issue_list(s, IssueListEvent::Wizard(k))
}

/// Type a string into the focused wizard text row, char by char.
fn type_str(mut s: IssueListState, text: &str) -> IssueListState {
    for ch in text.chars() {
        s = wiz(&s, WizardKey::Char(ch)).state;
    }
    s
}

/// Move focus to `row` by pressing Down until it lands there (bounded by the row
/// count). Position-INDEPENDENT on purpose: adding a wizard row must not
/// re-number every fixture in this file.
fn focus_row(mut s: IssueListState, row: WizardRow) -> IssueListState {
    for _ in 0..=WizardRow::ALL.len() {
        if s.wizard().expect("wizard is open").focus() == row {
            return s;
        }
        s = wiz(&s, WizardKey::Down).state;
    }
    panic!("focus never reached {row:?}");
}

/// Open a fresh wizard focused on the Title row (the `c` entry point).
fn open_wizard() -> IssueListState {
    reduce_issue_list(&seeded_state(), IssueListEvent::Key('c')).state
}

/// A wizard with a valid title AND a picked repo (scratch, via the `@` dropdown),
/// focus left on the Agent row — the "ready to create" fixture.
fn ready_to_create() -> IssueListState {
    let s = open_wizard();
    let s = type_str(s, "Fix login");
    // Move past Brief to Repo, open the dropdown (cursor at scratch), pick it.
    let s = focus_row(s, WizardRow::Repo);
    let s = wiz(&s, WizardKey::Char('@')).state;
    let s = wiz(&s, WizardKey::Enter).state; // pick scratch, dropdown closes
    // Land focus on the Agent row.
    let s = focus_row(s, WizardRow::Target);
    wiz(&s, WizardKey::Down).state // Target → Agent
}

/// `c` opens the create wizard as a fresh single form focused on Title, with the
/// branches prefilled `main`, no repo picked, and the default agent. No intent.
#[test]
fn c_opens_wizard_focused_on_title() {
    let s = seeded_state();
    assert_eq!(s.mode(), IssueListMode::Normal);

    let out = reduce_issue_list(&s, IssueListEvent::Key('c'));

    assert_eq!(out.state.mode(), IssueListMode::CreateInput);
    let w = out.state.wizard().expect("wizard opens");
    assert_eq!(w.focus(), WizardRow::Title);
    assert!(w.title().is_empty());
    assert_eq!(w.repo_ref(), None);
    assert_eq!(w.source_branch(), "main");
    assert_eq!(w.target_branch(), "main");
    assert_eq!(w.agent_cursor(), 0);
    assert!(out.intent.is_none());
}

/// ↓ / Tab advance the focused row, ↑ / Shift+Tab retreat, both wrapping around
/// EVERY row in [`WizardRow::ALL`] order (mirrors the host new-session Configure
/// form). Driven off `ALL` rather than a frozen literal list, so adding a row
/// extends the coverage instead of breaking the fixture.
#[test]
fn wizard_focus_moves_and_wraps() {
    let s = open_wizard();
    assert_eq!(s.wizard().unwrap().focus(), WizardRow::Title);
    assert_eq!(
        WizardRow::ALL.first(),
        Some(&WizardRow::Title),
        "a fresh wizard opens on the first row"
    );

    // Down / Tab walk the whole ring forward and land back on the first row.
    let mut s = s;
    for (i, expected) in WizardRow::ALL.iter().enumerate().skip(1) {
        // Alternate the two forward keys — they must be interchangeable.
        let key = if i % 2 == 0 {
            WizardKey::Tab
        } else {
            WizardKey::Down
        };
        s = wiz(&s, key).state;
        assert_eq!(
            s.wizard().unwrap().focus(),
            *expected,
            "row {i} of the forward walk"
        );
    }
    let s = wiz(&s, WizardKey::Down).state;
    assert_eq!(
        s.wizard().unwrap().focus(),
        WizardRow::Title,
        "forward wraps off the last row"
    );

    // Up walks the ring backward from Title, wrapping onto the last row first.
    let mut s = s;
    for expected in WizardRow::ALL.iter().rev() {
        s = wiz(&s, WizardKey::Up).state;
        assert_eq!(s.wizard().unwrap().focus(), *expected, "backward walk");
    }

    // Shift+Tab is the same retreat as ↑ (checked once, from the first row).
    let s = wiz(&s, WizardKey::BackTab).state;
    assert_eq!(
        s.wizard().unwrap().focus(),
        *WizardRow::ALL.last().unwrap(),
        "Shift+Tab retreats and wraps like ↑"
    );
}

/// ←/→ cycle the Agent picker through `AgentChip::ALL`, wrapping; the branch text
/// rows ignore ←/→ (they are not picker rows).
#[test]
fn wizard_left_right_cycles_agent_only() {
    // Move focus to the Agent row.
    let s = ready_to_create();
    assert_eq!(s.wizard().unwrap().focus(), WizardRow::Agent);
    assert_eq!(s.wizard().unwrap().agent_cursor(), 0); // claude

    let s = wiz(&s, WizardKey::Right).state;
    assert_eq!(s.wizard().unwrap().agent_cursor(), 1); // codex
    let s = wiz(&s, WizardKey::Right).state;
    assert_eq!(s.wizard().unwrap().agent_cursor(), 2); // copilot
    let s = wiz(&s, WizardKey::Right).state;
    assert_eq!(
        s.wizard().unwrap().agent_cursor(),
        0,
        "wraps back to claude"
    );
    let s = wiz(&s, WizardKey::Left).state;
    assert_eq!(
        s.wizard().unwrap().agent_cursor(),
        2,
        "left wraps to copilot"
    );

    // ←/→ on a text row is a no-op (Source keeps its prefill).
    let s = focus_row(s, WizardRow::Source);
    assert_eq!(s.wizard().unwrap().focus(), WizardRow::Source);
    let out = wiz(&s, WizardKey::Right);
    assert_eq!(out.state.wizard().unwrap().source_branch(), "main");
}

/// ←/→ on the Repo row cycles the roster (scratch first): from "none picked" →
/// lands on scratch, and is a full alternative to the `@` dropdown.
#[test]
fn wizard_left_right_cycles_repo() {
    let s = open_wizard();
    let s = type_str(s, "Fix");
    let s = focus_row(s, WizardRow::Repo);
    assert_eq!(s.wizard().unwrap().repo_ref(), None);

    // → picks the first candidate (scratch is always index 0).
    let out = wiz(&s, WizardKey::Right);
    assert_eq!(out.state.wizard().unwrap().repo_ref(), Some("scratch"));
}

/// Typing edits ONLY the focused text row; other rows are untouched.
#[test]
fn wizard_typing_edits_focused_text_row() {
    let s = open_wizard();
    let s = type_str(s, "Title text"); // Title row
    assert_eq!(s.wizard().unwrap().title(), "Title text");

    // Focus Source, clear the "main" prefill, type a custom ref.
    let s = focus_row(s, WizardRow::Source);
    let s = (0..4).fold(s, |s, _| wiz(&s, WizardKey::Backspace).state);
    let s = type_str(s, "feature/x");
    let w = s.wizard().unwrap();
    assert_eq!(w.source_branch(), "feature/x");
    assert_eq!(
        w.title(),
        "Title text",
        "Title untouched while editing Source"
    );
    assert_eq!(w.target_branch(), "main", "Target untouched");
}

/// `@` on the Repo row opens the fuzzy dropdown (cursor at scratch); subsequent
/// chars filter it.
#[test]
fn wizard_at_opens_repo_dropdown() {
    let s = open_wizard();
    let s = type_str(s, "Fix");
    let s = focus_row(s, WizardRow::Repo);
    assert_eq!(s.wizard().unwrap().repo_dropdown(), None);

    let s = wiz(&s, WizardKey::Char('@')).state;
    assert_eq!(s.wizard().unwrap().repo_dropdown(), Some(0));

    // A filter char stays in the dropdown and resets the cursor to the top.
    let s = wiz(&s, WizardKey::Char('z')).state;
    assert_eq!(s.wizard().unwrap().repo_dropdown(), Some(0));
    assert_eq!(s.wizard().unwrap().repo_query(), "z");
}

/// Tabbing away from an open repo dropdown commits the highlighted candidate
/// instead of discarding it — leaving the field with the pick made, dropdown
/// closed, focus advanced.
#[test]
fn wizard_tab_from_dropdown_commits_highlight() {
    let s = open_wizard();
    let s = type_str(s, "Fix");
    let s = focus_row(s, WizardRow::Repo);
    let s = wiz(&s, WizardKey::Char('@')).state; // open dropdown, cursor on scratch

    let out = wiz(&s, WizardKey::Tab);
    let w = out.state.wizard().expect("wizard still open");
    assert_eq!(
        w.repo_ref(),
        Some("scratch"),
        "highlight committed, not lost"
    );
    assert_eq!(w.repo_dropdown(), None, "dropdown closed on tab-away");
    assert_ne!(w.focus(), WizardRow::Repo, "focus advanced off Repo");
}

/// Enter with NO repo picked never creates — it jumps focus to the Repo row and
/// opens its dropdown (the REQUIRED guard for a repo-less create).
#[test]
fn wizard_enter_without_repo_focuses_repo() {
    let s = open_wizard();
    let s = type_str(s, "Fix login"); // title valid, but no repo picked

    let out = wiz(&s, WizardKey::Enter);

    assert!(out.intent.is_none(), "must not create repo-less");
    let w = out.state.wizard().expect("wizard still open");
    assert_eq!(w.focus(), WizardRow::Repo);
    assert_eq!(
        w.repo_dropdown(),
        Some(0),
        "dropdown opened to guide the user"
    );
}

/// Enter with a blank title never creates — it holds and focuses the Title row.
#[test]
fn wizard_enter_blank_title_focuses_title() {
    // Pick a repo first, then blank the title, then commit from the Agent row.
    let s = ready_to_create();
    // Move to Title and clear it.
    let s = wiz(&s, WizardKey::Down).state; // Agent → wraps to Title
    assert_eq!(s.wizard().unwrap().focus(), WizardRow::Title);
    let s = (0.."Fix login".len()).fold(s, |s, _| wiz(&s, WizardKey::Backspace).state);
    assert!(s.wizard().unwrap().title().is_empty());

    let out = wiz(&s, WizardKey::Enter);

    assert!(out.intent.is_none(), "blank title must not create");
    assert_eq!(out.state.wizard().unwrap().focus(), WizardRow::Title);
    assert_eq!(out.state.mode(), IssueListMode::CreateInput);
}

/// A complete form (title + repo + default agent) commits on Enter — from ANY
/// focused row — raising `CreateAndRun` with every collected field.
#[test]
fn wizard_complete_form_commits_create_and_run() {
    let s = ready_to_create();
    // Cycle the agent to codex to prove the picked value round-trips.
    let s = wiz(&s, WizardKey::Right).state;

    let out = wiz(&s, WizardKey::Enter);

    assert_eq!(
        out.intent,
        Some(IssueListIntent::CreateAndRun {
            title: "Fix login".to_string(),
            brief: None,
            external_ref: None,
            acceptance_criteria: Vec::new(),
            context_refs: Vec::new(),
            // An untouched Priority / Due / Labels row creates exactly what it
            // did before those rows existed (parity 28).
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            repo_ref: "scratch".to_string(),
            source_branch: Some("main".to_string()),
            target_branch: Some("main".to_string()),
            // No named-agent roster injected → provider-chip fallback (no assignee).
            agent: Some("codex".to_string()),
            assignee: None,
            // A plain `c` create carries no parent (0046).
            parent_issue_id: None,
        })
    );
    assert_eq!(out.state.mode(), IssueListMode::Normal);
    assert!(out.state.wizard().is_none());
}

/// Typing on the Brief row appends (including `/`, which must reach the agent
/// verbatim), and Backspace edits — the multi-line editor idiom.
#[test]
fn wizard_brief_appends_and_backspaces() {
    let s = open_wizard();
    let s = wiz(&s, WizardKey::Down).state; // Title → Brief
    assert_eq!(s.wizard().unwrap().focus(), WizardRow::Brief);

    let s = type_str(s, "run /fix now");
    assert_eq!(
        s.wizard().unwrap().brief(),
        "run /fix now",
        "printable chars (incl. `/`) append verbatim"
    );

    // Backspace deletes the last char.
    let s = (0..4).fold(s, |s, _| wiz(&s, WizardKey::Backspace).state);
    assert_eq!(s.wizard().unwrap().brief(), "run /fix");
}

/// Enter on the Brief inserts a NEWLINE and does NOT fire create — the wizard
/// stays open and no intent is raised (create commits only from the other rows).
#[test]
fn wizard_brief_enter_inserts_newline_not_create() {
    // Even a create-ready form (title + repo picked) must not commit from Brief.
    let s = ready_to_create();
    // Walk focus back to the Brief row (Agent → … → Brief).
    let s = focus_row(s, WizardRow::Brief);
    assert_eq!(s.wizard().unwrap().focus(), WizardRow::Brief);

    let s = type_str(s, "line one");
    let out = wiz(&s, WizardKey::Enter);

    assert!(out.intent.is_none(), "Enter on Brief must not create");
    assert_eq!(out.state.mode(), IssueListMode::CreateInput);
    let w = out.state.wizard().expect("wizard still open");
    assert_eq!(w.brief(), "line one\n", "Enter inserted a newline");
    assert_eq!(w.focus(), WizardRow::Brief, "focus stays on Brief");

    // A second line then appends after the newline.
    let s2 = type_str(out.state, "line two");
    assert_eq!(s2.wizard().unwrap().brief(), "line one\nline two");
}

/// The Link row is a single-line free-text field: printable chars append,
/// Backspace edits, and Enter COMMITS (unlike Brief, whose Enter inserts a
/// newline) — so a `owner/repo#123` typed there carries to
/// `CreateAndRun.external_ref` (trimmed; blank → `None`).
#[test]
fn wizard_link_carries_to_create_intent() {
    let s = open_wizard();
    let s = type_str(s, "Fix login");
    // Title → Brief → Link, then type the upstream ref.
    let s = focus_row(s, WizardRow::Link);
    assert_eq!(s.wizard().unwrap().focus(), WizardRow::Link);
    let s = type_str(s, "  acme/api#42  "); // surrounding whitespace is trimmed
    assert_eq!(s.wizard().unwrap().link(), "  acme/api#42  ");
    // Backspace edits the single line.
    let s = wiz(&s, WizardKey::Backspace).state; // drop a trailing space
    // Move to Repo, pick scratch, commit.
    let s = focus_row(s, WizardRow::Repo);
    let s = wiz(&s, WizardKey::Right).state; // pick scratch
    let out = wiz(&s, WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun {
            external_ref,
            title,
            ..
        }) => {
            assert_eq!(title, "Fix login");
            assert_eq!(
                external_ref,
                Some("acme/api#42".to_string()),
                "the linked ref carries trimmed onto the intent"
            );
        }
        other => panic!("expected CreateAndRun carrying the link, got {other:?}"),
    }
}

/// A blank Link row maps to `external_ref: None` on the intent (unlinked stubs
/// stay allowed).
#[test]
fn wizard_blank_link_is_none_on_intent() {
    let out = wiz(&ready_to_create(), WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun { external_ref, .. }) => {
            assert_eq!(external_ref, None, "an untouched Link creates unlinked");
        }
        other => panic!("expected CreateAndRun, got {other:?}"),
    }
}

/// A brief typed in the wizard carries through to `CreateAndRun.brief` (which the
/// glue maps to `issue.description` → the `claude -p` prompt).
#[test]
fn wizard_brief_carries_to_create_intent() {
    let s = open_wizard();
    let s = type_str(s, "Fix login");
    let s = wiz(&s, WizardKey::Down).state; // Title → Brief
    let s = type_str(s, "Reproduce then patch");
    // Move to Repo, pick scratch, then commit from the Repo row.
    let s = focus_row(s, WizardRow::Repo);
    let s = wiz(&s, WizardKey::Right).state; // pick scratch

    let out = wiz(&s, WizardKey::Enter);

    match out.intent {
        Some(IssueListIntent::CreateAndRun { brief, title, .. }) => {
            assert_eq!(title, "Fix login");
            assert_eq!(brief, Some("Reproduce then patch".to_string()));
        }
        other => panic!("expected CreateAndRun carrying the brief, got {other:?}"),
    }
}

/// The brief reaches the intent VERBATIM: a leading `/name` skill line, internal
/// newlines, and a trailing newline all survive unchanged (no trim / escape /
/// normalise), so a `claude --print` skill invocation in the brief executes as
/// typed at dispatch.
#[test]
fn wizard_brief_preserved_verbatim_on_intent() {
    let s = open_wizard();
    let s = type_str(s, "Fix login");
    let s = wiz(&s, WizardKey::Down).state; // Title → Brief
    // Line 1: a skill command. Newline. Line 2: prose. Trailing newline.
    let s = type_str(s, "/graphify the auth flow");
    let s = wiz(&s, WizardKey::Enter).state; // newline (not create)
    let s = type_str(s, "then summarise findings");
    let s = wiz(&s, WizardKey::Enter).state; // trailing newline
    // Commit from the Repo row.
    let s = focus_row(s, WizardRow::Repo);
    let s = wiz(&s, WizardKey::Right).state; // pick scratch

    let out = wiz(&s, WizardKey::Enter);

    let expected = "/graphify the auth flow\nthen summarise findings\n";
    match out.intent {
        Some(IssueListIntent::CreateAndRun { brief, .. }) => {
            assert_eq!(
                brief,
                Some(expected.to_string()),
                "brief must reach the intent byte-for-byte (slashes + newlines intact)"
            );
        }
        other => panic!("expected CreateAndRun, got {other:?}"),
    }
}

/// A blank brief maps to `None` on the intent (title-only stubs stay allowed).
#[test]
fn wizard_blank_brief_is_none_on_intent() {
    let out = wiz(&ready_to_create(), WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun { brief, .. }) => {
            assert_eq!(brief, None, "an untouched Brief creates unchanged");
        }
        other => panic!("expected CreateAndRun, got {other:?}"),
    }
}

/// A blanked branch input means "unset" (`None` on the intent); a custom source
/// ref carries through.
#[test]
fn wizard_branch_edits_and_blanks_round_trip() {
    let s = ready_to_create();
    // Focus Source (Agent → up twice), retype it, then blank Target.
    let s = focus_row(s, WizardRow::Source);
    let s = (0..4).fold(s, |s, _| wiz(&s, WizardKey::Backspace).state);
    let s = type_str(s, "feature/x");
    let s = wiz(&s, WizardKey::Down).state; // Target
    let s = (0..4).fold(s, |s, _| wiz(&s, WizardKey::Backspace).state);
    assert!(s.wizard().unwrap().target_branch().is_empty());

    let out = wiz(&s, WizardKey::Enter); // commit (claude, cursor 0)

    assert_eq!(
        out.intent,
        Some(IssueListIntent::CreateAndRun {
            title: "Fix login".to_string(),
            brief: None,
            external_ref: None,
            acceptance_criteria: Vec::new(),
            context_refs: Vec::new(),
            // An untouched Priority / Due / Labels row creates exactly what it
            // did before those rows existed (parity 28).
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            repo_ref: "scratch".to_string(),
            source_branch: Some("feature/x".to_string()),
            target_branch: None,
            // No named-agent roster injected → provider-chip fallback (no assignee).
            agent: Some("claude".to_string()),
            assignee: None,
            // A plain `c` create carries no parent (0046).
            parent_issue_id: None,
        })
    );
}

/// The REQUIRED guard is exhaustive: no key sequence can create a title-only or
/// repo-less issue. Only a form with BOTH a title AND a repo yields the intent,
/// and it always carries a real provider agent.
#[test]
fn wizard_required_guard_blocks_incomplete_creates() {
    // Title only, no repo → Enter never creates.
    let s = type_str(open_wizard(), "Only a title");
    assert!(wiz(&s, WizardKey::Enter).intent.is_none());

    // Repo only, no title → Enter never creates.
    let s = open_wizard();
    let s = focus_row(s, WizardRow::Repo);
    let s = wiz(&s, WizardKey::Right).state; // pick scratch
    assert_eq!(s.wizard().unwrap().repo_ref(), Some("scratch"));
    assert!(wiz(&s, WizardKey::Enter).intent.is_none());

    // Both present → the ONLY path that creates. With no named-agent roster the
    // Agent row falls back to a real provider token (and carries no assignee).
    let out = wiz(&ready_to_create(), WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun {
            agent, assignee, ..
        }) => {
            let agent = agent.expect("provider-chip fallback carries a token");
            assert!(["claude", "codex", "copilot"].contains(&agent.as_str()));
            assert!(assignee.is_none(), "no named agent targeted → no assignee");
        }
        other => panic!("complete form must commit CreateAndRun, got {other:?}"),
    }
}

/// Esc cancels the WHOLE wizard in ONE press — from any focused row and with the
/// `@` dropdown open — back to normal navigation, nothing retained, no intent.
#[test]
fn wizard_esc_cancels_whole_overlay() {
    // From each focused row (Agent, then walking focus forward each step).
    let mut s = ready_to_create();
    for _ in 0..WizardRow::ALL.len() {
        let out = wiz(&s, WizardKey::Esc);
        assert_eq!(out.state.mode(), IssueListMode::Normal);
        assert!(out.state.wizard().is_none());
        assert!(out.intent.is_none());
        s = wiz(&s, WizardKey::Down).state; // advance focus for the next iteration
    }
    // With the dropdown open.
    let s = open_wizard();
    let s = wiz(&s, WizardKey::Down).state;
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

// ---------------------------------------------------------------------------
// V3-F3: the create wizard's Agent row targets a NAMED workspace agent.
// ---------------------------------------------------------------------------

/// A `ready_to_create` wizard with a NAMED-agent roster injected, focus on the
/// Agent row (so ←/→ cycles the roster and Enter commits).
fn ready_to_create_with_agents(agents: Vec<WizardAgent>) -> IssueListState {
    let mut s = ready_to_create();
    s.set_agents(agents);
    s
}

fn agent(actor_ref: &str, label: &str) -> WizardAgent {
    WizardAgent {
        actor_ref: actor_ref.to_string(),
        label: label.to_string(),
    }
}

/// With a named-agent roster injected, Enter commits `CreateAndRun` carrying the
/// picked agent's `agent:<id>` ref as the assignee (and NO provider token — the
/// named agent's own provider drives the run).
#[test]
fn wizard_targets_named_agent_as_assignee() {
    let s = ready_to_create_with_agents(vec![
        agent("agent:dev-id", "dev"),
        agent("agent:reviewer-id", "reviewer"),
    ]);
    // Cursor starts at 0 (the first named agent, "dev").
    let out = wiz(&s, WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun {
            agent, assignee, ..
        }) => {
            assert_eq!(
                assignee,
                Some("agent:dev-id".to_string()),
                "targets the named agent"
            );
            assert!(agent.is_none(), "a named target carries no provider token");
        }
        other => panic!("expected CreateAndRun targeting a named agent, got {other:?}"),
    }
}

/// ←/→ on the Agent row cycles the NAMED roster (not the provider chips) when a
/// roster is injected, and the picked agent round-trips to the assignee.
#[test]
fn wizard_agent_row_cycles_named_roster() {
    let s = ready_to_create_with_agents(vec![
        agent("agent:dev-id", "dev"),
        agent("agent:reviewer-id", "reviewer"),
    ]);
    // → advances from "dev" (0) to "reviewer" (1).
    let s = wiz(&s, WizardKey::Right).state;
    let out = wiz(&s, WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun { assignee, .. }) => {
            assert_eq!(assignee, Some("agent:reviewer-id".to_string()));
        }
        other => panic!("expected CreateAndRun, got {other:?}"),
    }

    // → wraps the two-agent roster back to "dev".
    let s = ready_to_create_with_agents(vec![
        agent("agent:dev-id", "dev"),
        agent("agent:reviewer-id", "reviewer"),
    ]);
    let s = wiz(&s, WizardKey::Right).state; // reviewer
    let s = wiz(&s, WizardKey::Right).state; // wrap → dev
    let out = wiz(&s, WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun { assignee, .. }) => {
            assert_eq!(
                assignee,
                Some("agent:dev-id".to_string()),
                "two-agent roster wraps"
            );
        }
        other => panic!("expected CreateAndRun, got {other:?}"),
    }
}

/// With an EMPTY named-agent roster the Agent row falls back to the provider
/// chips: Enter commits a provider token and NO assignee (the pre-fix behaviour,
/// which dispatches deterministically to the workspace's first agent).
#[test]
fn wizard_agent_row_falls_back_to_provider_chips_when_roster_empty() {
    // ready_to_create injects no agents, so the roster is empty.
    let out = wiz(&ready_to_create(), WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun {
            agent, assignee, ..
        }) => {
            assert_eq!(
                agent,
                Some("claude".to_string()),
                "provider-chip fallback token"
            );
            assert!(assignee.is_none(), "no named target → no assignee");
        }
        other => panic!("expected CreateAndRun, got {other:?}"),
    }

    // And ←/→ cycles the PROVIDER chips (claude → codex), not a named roster.
    let s = wiz(&ready_to_create(), WizardKey::Right).state;
    let out = wiz(&s, WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun {
            agent, assignee, ..
        }) => {
            assert_eq!(agent, Some("codex".to_string()));
            assert!(assignee.is_none());
        }
        other => panic!("expected CreateAndRun, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 0046 sub-issues: `s` (add sub-issue, parent pre-bound) + `d` (mark done).
// ---------------------------------------------------------------------------

/// `s` opens the create wizard as an "add sub-issue" with the HIGHLIGHTED row
/// pre-bound as the parent: the wizard exposes the parent's wire id + a display
/// for the read-only banner, and is otherwise a normal Title-focused create.
#[test]
fn s_key_opens_subissue_wizard_with_parent_prebound() {
    let s = seeded_state(); // selection defaults to the first row (i1)
    let parent_id = s.selected_row().unwrap().id.as_str().to_string();

    let out = reduce_issue_list(&s, IssueListEvent::Key('s'));

    assert!(out.intent.is_none(), "opening the wizard raises no intent");
    assert_eq!(out.state.mode(), IssueListMode::CreateInput);
    let w = out.state.wizard().expect("wizard is open");
    assert_eq!(
        w.focus(),
        WizardRow::Title,
        "focus starts on Title like `c`"
    );
    assert_eq!(
        w.parent_issue_id(),
        Some(parent_id.as_str()),
        "the highlighted row is pre-bound as the parent"
    );
    assert!(
        w.parent_display().is_some_and(|d| d.contains("Issue i1")),
        "the banner display names the parent row"
    );
}

/// A complete sub-issue form commits `CreateAndRun` carrying the pre-bound
/// `parent_issue_id`, so the daemon links the new issue as a child.
#[test]
fn subissue_wizard_commit_carries_parent_issue_id() {
    let s = seeded_state();
    let parent_id = s.selected_row().unwrap().id.as_str().to_string();
    // Open via `s`, then fill exactly like `ready_to_create` (title + repo + agent).
    let s = reduce_issue_list(&s, IssueListEvent::Key('s')).state;
    let s = type_str(s, "Fix login");
    let s = focus_row(s, WizardRow::Repo);
    let s = wiz(&s, WizardKey::Char('@')).state;
    let s = wiz(&s, WizardKey::Enter).state; // pick scratch
    let s = focus_row(s, WizardRow::Agent);

    let out = wiz(&s, WizardKey::Enter);

    match out.intent {
        Some(IssueListIntent::CreateAndRun {
            parent_issue_id, ..
        }) => assert_eq!(
            parent_issue_id,
            Some(parent_id),
            "the sub-issue create threads the pre-bound parent id"
        ),
        other => panic!("expected CreateAndRun carrying the parent, got {other:?}"),
    }
}

/// The plain `c` create carries NO parent (regression guard for the 0046 field).
#[test]
fn plain_create_carries_no_parent() {
    let out = wiz(&ready_to_create(), WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun {
            parent_issue_id, ..
        }) => assert_eq!(parent_issue_id, None),
        other => panic!("expected CreateAndRun, got {other:?}"),
    }
}

/// `s` with no selectable row (empty board) is a no-op: a sub-issue needs a
/// parent row to hang from.
#[test]
fn s_key_on_empty_board_is_noop() {
    let s = IssueListState::with_rows(Vec::new());
    let out = reduce_issue_list(&s, IssueListEvent::Key('s'));
    assert!(out.intent.is_none());
    assert_eq!(out.state.mode(), IssueListMode::Normal, "no wizard opens");
    assert!(out.state.wizard().is_none());
}

/// `d` raises `MarkDone` for the highlighted row: the keyboard mark-done that
/// routes through `hangar/issue_update{state:"done"}` and fires the cascade.
#[test]
fn d_key_marks_highlighted_row_done() {
    let s = seeded_state();
    let target = s.selected_row().unwrap().id.clone();

    let out = reduce_issue_list(&s, IssueListEvent::Key('d'));

    assert_eq!(out.intent, Some(IssueListIntent::MarkDone(target)));
}

/// `d` on an empty board is a no-op (nothing selected → no intent).
#[test]
fn d_key_on_empty_board_is_noop() {
    let s = IssueListState::with_rows(Vec::new());
    let out = reduce_issue_list(&s, IssueListEvent::Key('d'));
    assert!(out.intent.is_none());
}

// ---------------------------------------------------------------------------
// Faceted filtering (multica-gap #10): structured status / priority / label /
// assignee / due facets with per-value drill-down counts, OR-within / AND-across.
// ---------------------------------------------------------------------------

use ainb_hangar_proto::lifecycle::IssueLifecycle;
use ainb_plugin_hangar::screen::issue_list::{AssigneeFacet, FacetKind, FacetValue, PanelKey};

/// A wire `IssueRow` with explicit priority + labels for facet tests.
/// `priority` is the wire scalar (`3` = P0, `1` = P2, `2` = P1, `0` = P3).
fn facet_row(id: &str, state: &str, priority: i64, labels: &[&str]) -> IssueRow {
    let mut r = row(id, state, Some("agent:claude"));
    r.title = format!("{id} title");
    r.priority = priority;
    r.labels = labels.iter().map(|l| (*l).to_string()).collect();
    r
}

/// The discriminating 5-row matrix from the gap-10 spec: exactly ONE row is
/// `todo + P0 + bug` (the sole survivor of the 3-facet AND filter).
fn facet_matrix() -> IssueListState {
    IssueListState::with_rows(vec![
        facet_row("target", "todo", 3, &["bug"]), // todo P0 bug  ← survivor
        facet_row("d_nolbl", "todo", 3, &[]),     // todo P0 (no label)
        facet_row("d_p2bug", "todo", 1, &["bug"]), // todo P2 bug
        facet_row("d_progbug", "in_progress", 3, &["bug"]), // prog P0 bug
        facet_row("d_chore", "done", 2, &["chore"]), // done P1 chore
    ])
}

/// Collect the visible row ids (daemon order) for assertion.
fn visible_ids(s: &IssueListState) -> Vec<String> {
    s.visible_rows().map(|r| r.id.as_str().to_string()).collect()
}

/// Toggling status=todo AND priority=P0 AND label=bug narrows to the SOLE target
/// row, and the column counts reflect it (Todo (1), In Progress (0)).
#[test]
fn three_facet_and_narrows_to_single_row() {
    let s = facet_matrix();
    // Baseline: all five visible.
    assert_eq!(visible_ids(&s).len(), 5);

    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(FacetKind::Status, FacetValue::Status(IssueLifecycle::Todo)),
    )
    .state;
    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(FacetKind::Priority, FacetValue::Priority(3)),
    )
    .state;
    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(FacetKind::Label, FacetValue::Label("bug".to_string())),
    )
    .state;

    assert_eq!(visible_ids(&s), vec!["target".to_string()]);
    assert_eq!(s.column_count(IssueColumn::Todo), 1);
    assert_eq!(s.column_count(IssueColumn::InProgress), 0);
    assert_eq!(s.column_count(IssueColumn::Done), 0);
}

/// OR-within a facet: adding P2 to the {P0} priority set re-admits the P2 decoy
/// (P0 OR P2), still AND-ed with status=todo AND label=bug.
#[test]
fn or_within_priority_readmits_decoy() {
    let s = facet_matrix();
    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(FacetKind::Status, FacetValue::Status(IssueLifecycle::Todo)),
    )
    .state;
    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(FacetKind::Label, FacetValue::Label("bug".to_string())),
    )
    .state;
    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(FacetKind::Priority, FacetValue::Priority(3)),
    )
    .state;
    // todo + bug + P0 → only target.
    assert_eq!(visible_ids(&s), vec!["target".to_string()]);

    // Add P2 (OR within priority): the todo+bug+P2 decoy joins.
    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(FacetKind::Priority, FacetValue::Priority(1)),
    )
    .state;
    let mut ids = visible_ids(&s);
    ids.sort();
    assert_eq!(ids, vec!["d_p2bug".to_string(), "target".to_string()]);
}

/// Facet counts are drill-down: with status=todo + priority=P0 applied, the LABEL
/// counts are against status+priority (NOT the label facet), so `bug` shows 1 and
/// `chore` is absent (zero-count omitted).
#[test]
fn label_counts_are_drilldown_against_other_facets() {
    let s = facet_matrix();
    // No facets: label counts over all rows → bug 3, chore 1.
    let all = s.facet_counts(FacetKind::Label);
    assert_eq!(
        all,
        vec![
            (FacetValue::Label("bug".to_string()), 3),
            (FacetValue::Label("chore".to_string()), 1),
        ]
    );

    // Apply status=todo + priority=P0.
    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(FacetKind::Status, FacetValue::Status(IssueLifecycle::Todo)),
    )
    .state;
    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(FacetKind::Priority, FacetValue::Priority(3)),
    )
    .state;
    // Drill-down label counts (todo + P0 scope, excluding label facet): only the
    // target (bug) qualifies; d_nolbl has no label; chore is zero → omitted.
    assert_eq!(
        s.facet_counts(FacetKind::Label),
        vec![(FacetValue::Label("bug".to_string()), 1)]
    );
}

/// Priority counts render most-urgent-first (P0 before P2), and no-facet status
/// counts omit the zero-count Backlog value (only present values appear).
#[test]
fn priority_counts_order_p0_first_and_omit_zero() {
    let s = facet_matrix();
    let prio = s.facet_counts(FacetKind::Priority);
    // P0(value 3) = 3 rows, P1(value 2) = 1, P2(value 1) = 1 → P0 first.
    assert_eq!(
        prio,
        vec![
            (FacetValue::Priority(3), 3),
            (FacetValue::Priority(2), 1),
            (FacetValue::Priority(1), 1),
        ]
    );

    let status = s.facet_counts(FacetKind::Status);
    // Only Todo / In Progress / Done present; Backlog + In Review omitted.
    assert_eq!(
        status,
        vec![
            (FacetValue::Status(IssueLifecycle::Todo), 3),
            (FacetValue::Status(IssueLifecycle::InProgress), 1),
            (FacetValue::Status(IssueLifecycle::Done), 1),
        ]
    );
}

/// The `f` key opens the panel; Space toggles the first Status value (Todo, the
/// first present); Right moves to Priority; Space toggles P0 (first, most-urgent);
/// Right to Label; Space toggles bug — narrowing to the single target, exactly as
/// the tmux acceptance drives it.
#[test]
fn panel_key_journey_narrows_to_target() {
    let s = facet_matrix();
    let s = reduce_issue_list(&s, IssueListEvent::Key('f')).state;
    assert_eq!(s.mode(), IssueListMode::FilterPanel);
    // Status section, cursor 0 → Todo (first present value).
    let s = reduce_issue_list(&s, IssueListEvent::Panel(PanelKey::Toggle)).state;
    // → Priority section (cursor resets to 0 = P0).
    let s = reduce_issue_list(&s, IssueListEvent::Panel(PanelKey::Right)).state;
    let s = reduce_issue_list(&s, IssueListEvent::Panel(PanelKey::Toggle)).state;
    // → Label section (cursor 0 = bug, the only in-scope label).
    let s = reduce_issue_list(&s, IssueListEvent::Panel(PanelKey::Right)).state;
    let s = reduce_issue_list(&s, IssueListEvent::Panel(PanelKey::Toggle)).state;

    assert_eq!(visible_ids(&s), vec!["target".to_string()]);
    assert!(s.facets().contains(&FacetValue::Status(IssueLifecycle::Todo)));
    assert!(s.facets().contains(&FacetValue::Priority(3)));
    assert!(s.facets().contains(&FacetValue::Label("bug".to_string())));

    // Esc closes the panel but the facets REMAIN applied (still one row).
    let s = reduce_issue_list(&s, IssueListEvent::Panel(PanelKey::Close)).state;
    assert_eq!(s.mode(), IssueListMode::Normal);
    assert_eq!(visible_ids(&s), vec!["target".to_string()]);
}

/// `ClearFacets` (the panel's `C`) drops every selection and re-widens the board.
#[test]
fn clear_facets_rewidens_the_board() {
    let s = facet_matrix();
    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(FacetKind::Status, FacetValue::Status(IssueLifecycle::Todo)),
    )
    .state;
    assert_eq!(visible_ids(&s).len(), 3);
    let s = reduce_issue_list(&s, IssueListEvent::ClearFacets).state;
    assert_eq!(visible_ids(&s).len(), 5);
    assert!(s.facets().is_empty());
}

/// The active-facet summary chip reflects the applied facets (fail-visible).
#[test]
fn facet_summary_reports_applied_facets() {
    let s = facet_matrix();
    assert!(s.facets().summary().is_none());
    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(FacetKind::Status, FacetValue::Status(IssueLifecycle::Todo)),
    )
    .state;
    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(FacetKind::Priority, FacetValue::Priority(3)),
    )
    .state;
    let summary = s.facets().summary().expect("summary present");
    assert!(summary.contains("status:todo"), "summary: {summary}");
    assert!(summary.contains("priority:P0"), "summary: {summary}");
}

/// The assignee facet buckets a row by its actor kind (member / agent /
/// unassigned), AND-ed with the other facets.
#[test]
fn assignee_facet_filters_by_actor_kind() {
    // Give one todo row a member assignee, leave the rest agent-assigned.
    let mut rows: Vec<IssueRow> = facet_matrix().visible_rows().cloned().collect();
    rows[1].assignee = Some("member:alice".to_string()); // d_nolbl → member
    let s = IssueListState::with_rows(rows);
    let s = reduce_issue_list(
        &s,
        IssueListEvent::ToggleFacet(
            FacetKind::Assignee,
            FacetValue::Assignee(AssigneeFacet::Member),
        ),
    )
    .state;
    assert_eq!(visible_ids(&s), vec!["d_nolbl".to_string()]);
}

// ---------------------------------------------------------------------------
// Parity 28: the wizard authors priority / due date / labels
// ---------------------------------------------------------------------------

/// `2026-08-01` at UTC midnight in epoch milliseconds.
const DUE_2026_08_01: i64 = 1_785_542_400_000;

/// Driving REAL keys through the wizard — ←/→ on Priority, typing on Due and
/// Labels — carries all three onto the raised `CreateAndRun`, with the labels
/// comma-split, trimmed, blank-dropped and deduped (first-seen order kept).
#[test]
fn wizard_priority_due_labels_carry_to_create_intent() {
    let s = open_wizard();
    let s = type_str(s, "Fix login");

    // Priority is a PICKER: → three times walks 0 → 3 (P3 → P0).
    let s = focus_row(s, WizardRow::Priority);
    let s = wiz(&s, WizardKey::Right).state;
    let s = wiz(&s, WizardKey::Right).state;
    let s = wiz(&s, WizardKey::Right).state;
    assert_eq!(s.wizard().unwrap().priority(), 3, "→×3 reaches P0");

    // Due + Labels are single-line TEXT rows.
    let s = focus_row(s, WizardRow::Due);
    let s = type_str(s, "2026-08-01");
    let s = focus_row(s, WizardRow::Labels);
    let s = type_str(s, "bug, p0, ,bug");

    let s = focus_row(s, WizardRow::Repo);
    let s = wiz(&s, WizardKey::Right).state; // pick scratch

    let out = wiz(&s, WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun {
            priority,
            due_date,
            labels,
            ..
        }) => {
            assert_eq!(priority, 3, "the picked urgency reaches the intent");
            assert_eq!(
                due_date,
                Some(DUE_2026_08_01),
                "the typed calendar day is parsed at UTC midnight"
            );
            assert_eq!(
                labels,
                vec!["bug".to_string(), "p0".to_string()],
                "blank dropped, duplicate dropped, first-seen order kept"
            );
        }
        other => panic!("expected CreateAndRun, got {other:?}"),
    }
}

/// Priority wraps like every other picker ring: ← from the default 0 lands on 3.
#[test]
fn wizard_priority_ring_wraps_and_ignores_typing() {
    let s = focus_row(open_wizard(), WizardRow::Priority);
    assert_eq!(s.wizard().unwrap().priority(), 0, "default is P3");

    let s = wiz(&s, WizardKey::Left).state;
    assert_eq!(s.wizard().unwrap().priority(), 3, "← wraps 0 → 3");

    // A picker row ignores text: typing and Backspace leave it alone.
    let s = type_str(s, "9");
    let s = wiz(&s, WizardKey::Backspace).state;
    assert_eq!(
        s.wizard().unwrap().priority(),
        3,
        "typing never edits a picker row"
    );
}

/// An unparseable due date NEVER silently drops the deadline: Enter refuses to
/// create, the wizard stays open, and focus lands on the offending row.
#[test]
fn wizard_invalid_due_blocks_create_and_focuses_the_row() {
    let s = open_wizard();
    let s = type_str(s, "Fix login");
    let s = focus_row(s, WizardRow::Due);
    let s = type_str(s, "31-12-2026"); // valid-looking, wrong order
    let s = focus_row(s, WizardRow::Repo);
    let s = wiz(&s, WizardKey::Right).state; // pick scratch — form is otherwise complete

    let out = wiz(&s, WizardKey::Enter);

    assert!(out.intent.is_none(), "a bad due date must not create");
    assert_eq!(out.state.mode(), IssueListMode::CreateInput);
    assert_eq!(
        out.state.wizard().expect("wizard still open").focus(),
        WizardRow::Due,
        "focus guides the user to the offending row"
    );

    // Correcting it unblocks the create (the guard is not a dead end).
    let s = out.state;
    let s = (0.."31-12-2026".len()).fold(s, |s, _| wiz(&s, WizardKey::Backspace).state);
    let s = type_str(s, "2026-08-01");
    let out = wiz(&s, WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun { due_date, .. }) => {
            assert_eq!(due_date, Some(DUE_2026_08_01));
        }
        other => panic!("expected CreateAndRun after the fix, got {other:?}"),
    }
}

/// A blank Due row is NOT an error — it simply means "no deadline".
#[test]
fn wizard_blank_due_creates_without_a_deadline() {
    let out = wiz(&ready_to_create(), WizardKey::Enter);
    match out.intent {
        Some(IssueListIntent::CreateAndRun {
            priority,
            due_date,
            labels,
            ..
        }) => {
            assert_eq!(priority, 0, "untouched Priority stays P3");
            assert_eq!(due_date, None, "blank Due means no deadline");
            assert!(labels.is_empty(), "blank Labels means no labels");
        }
        other => panic!("expected CreateAndRun, got {other:?}"),
    }
}
