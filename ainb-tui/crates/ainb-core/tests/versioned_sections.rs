// ABOUTME: The contract Phase 2 exists to provide. A reducer event bumps the
// sections it actually touched and no others; a draw bumps nothing at all;
// `changed_since` reports exactly the difference.
//
// The draw case is the load-bearing one. Versioning is only meaningful because
// Phase 3 sealed the render path behind `&AppState`: if drawing could still
// mutate core state, every frame would bump every section and `changed_since`
// would answer "all nineteen" forever.

use ainb::app::events::{AppEvent, EventHandler};
use ainb::app::state::AppState;
use ainb::app::ui_state::UiState;
use ainb::app::versioned::{SectionId, SectionVersions};
use ainb::components::LayoutComponent;
use ainb::models::{Session, Workspace};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Drive one event and report which sections it bumped.
fn bumped_by(event: AppEvent) -> Vec<SectionId> {
    bumped_from(AppState::default(), event)
}

fn bumped_from(mut state: AppState, event: AppEvent) -> Vec<SectionId> {
    let seen: SectionVersions = state.versions();
    EventHandler::process_event(event, &mut state);
    state.changed_since(&seen)
}

/// A state with one workspace holding one session, with both cursors on it.
///
/// Several reducers are guarded on having a selection, and on a bare
/// `AppState::default()` they take no `&mut` at all. That is correct
/// behaviour, and it is asserted on its own below; here it would just make
/// the table vacuous.
fn state_with_a_selected_session() -> AppState {
    let mut state = AppState::default();
    let mut workspace = Workspace::new("demo".to_string(), "/tmp/demo".into());
    workspace.sessions.push(Session::new(
        "demo-session".to_string(),
        "/tmp/demo".to_string(),
    ));
    let sessions = state.sessions.get_mut();
    sessions.workspaces.push(workspace);
    sessions.selected_workspace_index = Some(0);
    sessions.selected_session_index = Some(0);
    state
}

#[test]
fn an_event_bumps_the_sections_it_touches_and_no_others() {
    // Each row is an event and the sections it is allowed to move. Read it as
    // the routing table it is: if an event starts bumping something new, that
    // is either a section boundary being crossed or a field in the wrong home.
    //
    // Events whose reducer spawns onto the tokio runtime (GoToSkills, the MCP
    // overlay's fetch, the other lazy loaders) are out of scope here: this test is synchronous and
    // ainb-core has no tokio dev-dependency to borrow a reactor from. They are
    // covered by the tripwires that drive those screens for real.
    let cases: Vec<(AppEvent, Vec<SectionId>)> = vec![
        (AppEvent::ToggleHelp, vec![SectionId::Shell]),
        (AppEvent::GoToHomeScreen, vec![SectionId::Shell]),
        (AppEvent::SessionTabNext, vec![SectionId::Shell]),
        (AppEvent::ToggleExpandAll, vec![SectionId::Sessions]),
        (
            AppEvent::QuickCommitCancel,
            vec![SectionId::GitView, SectionId::Shell],
        ),
    ];

    for (event, allowed) in cases {
        let label = format!("{event:?}");
        let bumped = bumped_from(state_with_a_selected_session(), event);
        // Subset alone would pass for an event that bumped nothing at all,
        // which is the failure mode this whole phase is about.
        assert!(
            !bumped.is_empty(),
            "{label} bumped no section; either it did nothing or its writes \
             are not going through a section"
        );
        for section in &bumped {
            assert!(
                allowed.contains(section),
                "{label} bumped {section:?}, which is not in its allowed set {allowed:?}"
            );
        }
    }
}

#[test]
fn an_event_that_changes_nothing_bumps_nothing() {
    // `changed_since` against its own snapshot, with no event in between.
    let state = AppState::default();
    let seen = state.versions();
    assert!(
        state.changed_since(&seen).is_empty(),
        "a state nobody touched reported changes"
    );
}

/// One whole frame, in the order `main.rs` runs it.
///
/// Driving only `render` would test the half of the frame that CANNOT mutate,
/// because Phase 3 sealed it behind `&AppState`. The mutation lives in the two
/// bookends, so a draw test that skips them proves nothing.
fn draw_one_frame(state: &mut AppState, ui: &mut UiState, layout: &mut LayoutComponent) {
    let mut terminal = Terminal::new(TestBackend::new(160, 48)).expect("test terminal");
    layout.tick_before_draw(state);
    terminal.draw(|frame| layout.render(frame, state, ui)).expect("draw");
    ainb::components::layout::publish_after_draw(state, ui);
}

#[test]
fn a_draw_bumps_no_section() {
    let mut state = AppState::default();
    let mut ui = UiState::default();
    let mut layout = LayoutComponent::new();

    // The first frame is allowed to move things: rects have never been
    // published, tabs have never been reconciled. It is the steady state that
    // has to be quiet, because that is every frame after the first.
    draw_one_frame(&mut state, &mut ui, &mut layout);

    let seen = state.versions();
    draw_one_frame(&mut state, &mut ui, &mut layout);
    draw_one_frame(&mut state, &mut ui, &mut layout);

    let bumped = state.changed_since(&seen);
    assert!(
        bumped.is_empty(),
        "two settled frames bumped {bumped:?}; the draw path is writing unconditionally again"
    );
}

#[test]
fn a_draw_on_the_session_list_bumps_no_section() {
    // The session list is where the frame does its real work: the tab
    // reconcile, the answer worker fold, the rect publishes.
    let mut state = state_with_a_selected_session();
    state.shell.current_screen = ainb::app::screens::ids::SESSION_LIST.to_string();
    let mut ui = UiState::default();
    let mut layout = LayoutComponent::new();

    draw_one_frame(&mut state, &mut ui, &mut layout);

    let seen = state.versions();
    draw_one_frame(&mut state, &mut ui, &mut layout);
    draw_one_frame(&mut state, &mut ui, &mut layout);

    let bumped = state.changed_since(&seen);
    assert!(
        bumped.is_empty(),
        "two settled session-list frames bumped {bumped:?}"
    );
}

#[test]
fn changed_since_reports_only_what_moved_since_the_snapshot() {
    let mut state = AppState::default();

    EventHandler::process_event(AppEvent::ToggleHelp, &mut state);
    let seen = state.versions();

    // Nothing since the snapshot.
    assert!(state.changed_since(&seen).is_empty());

    // One more event, and only its section is reported: the earlier bump is
    // already inside `seen`.
    EventHandler::process_event(AppEvent::ToggleExpandAll, &mut state);
    let bumped = state.changed_since(&seen);
    assert!(
        bumped.contains(&SectionId::Sessions),
        "expected Sessions in {bumped:?}"
    );
    assert!(
        !bumped.contains(&SectionId::GitView),
        "GitView was never touched but {bumped:?} names it"
    );
}

#[test]
fn every_section_has_a_distinct_slot() {
    let state = AppState::default();
    assert_eq!(state.versions().len(), SectionId::COUNT);
    for (i, id) in SectionId::ALL.iter().enumerate() {
        assert_eq!(id.index(), i);
    }
}

#[test]
fn a_guarded_no_op_does_not_bump_its_section() {
    // `move_quick_commit_cursor_left` is guarded on `cursor > 0`, and on a
    // fresh state the cursor is 0, so the reducer takes no `&mut` at all.
    //
    // This is the boundary the coarse-bump design sits on. Over-bumping is
    // allowed and costs a surface one redundant send; a bump for an event that
    // provably wrote nothing would make `changed_since` useless on the idle
    // loop, where most events are guarded like this one.
    assert!(
        bumped_by(AppEvent::QuickCommitCursorLeft).is_empty(),
        "a guarded no-op bumped its section"
    );
}

#[test]
fn a_selection_driven_rename_bumps_its_own_section() {
    // With a selection in place the guard passes and the write lands, which is
    // the other half of the no-op case above.
    let bumped = bumped_from(
        state_with_a_selected_session(),
        AppEvent::SessionLabelStartRename,
    );
    assert!(
        bumped.contains(&SectionId::SessionLabels),
        "expected SessionLabels in {bumped:?}"
    );
}

#[test]
fn every_section_moves_its_own_slot_and_only_its_own() {
    // One writer per section, so all nineteen slots are exercised rather than
    // the four a handful of events happen to touch. A slot wired to the wrong
    // field, or two sections sharing one counter, fails here.
    #[allow(clippy::type_complexity)]
    let writers: Vec<(SectionId, Box<dyn Fn(&mut AppState)>)> = vec![
        (
            SectionId::Sessions,
            Box::new(|s: &mut AppState| s.sessions.shell_selected = true),
        ),
        (
            SectionId::SessionLabels,
            Box::new(|s: &mut AppState| s.session_labels.session_label_rename_mode = true),
        ),
        (
            SectionId::Tmux,
            Box::new(|s: &mut AppState| s.tmux.other_tmux_expanded = true),
        ),
        (
            SectionId::Ssh,
            Box::new(|s: &mut AppState| s.ssh.ssh_sessions_expanded = true),
        ),
        (
            SectionId::GitView,
            Box::new(|s: &mut AppState| s.git_view.is_current_dir_git_repo = true),
        ),
        (
            SectionId::WorkspaceLoad,
            Box::new(|s: &mut AppState| s.workspace_load.is_loading_workspaces = true),
        ),
        (
            SectionId::NewSession,
            Box::new(|s: &mut AppState| s.new_session.branch_refresh_seq += 1),
        ),
        (
            SectionId::Logs,
            Box::new(|s: &mut AppState| s.log_streams.last_logs_session_id = None),
        ),
        (
            SectionId::ClaudeChat,
            Box::new(|s: &mut AppState| s.claude_chat.claude_chat_visible = true),
        ),
        (
            SectionId::Fleet,
            Box::new(|s: &mut AppState| s.fleet.attention_elsewhere = 7),
        ),
        (
            SectionId::Hangar,
            Box::new(|s: &mut AppState| s.hangar.hangar_daemon_config_loaded = true),
        ),
        (
            SectionId::McpPool,
            Box::new(|s: &mut AppState| s.mcp_pool.mcp_overlay = None),
        ),
        // InboxSection has no fields yet, so an explicit `get_mut` is the only
        // thing that can move it. That is what the assertion is for: the slot
        // is wired, and nothing else shares it.
        (
            SectionId::Inbox,
            Box::new(|s: &mut AppState| {
                let _ = s.inbox.get_mut();
            }),
        ),
        (
            SectionId::PluginsHost,
            Box::new(|s: &mut AppState| {
                s.plugins_host.plugin_captures_text.insert("demo".to_string(), true);
            }),
        ),
        (
            SectionId::Config,
            Box::new(|s: &mut AppState| s.config.app_config.ui_preferences.show_git_status = true),
        ),
        (
            SectionId::Skills,
            Box::new(|s: &mut AppState| s.skills.skills_state.loading = true),
        ),
        (
            SectionId::Recovery,
            Box::new(|s: &mut AppState| s.recovery.session_recovery_state.loading = true),
        ),
        (
            SectionId::Onboarding,
            Box::new(|s: &mut AppState| s.onboarding.setup_menu_state.selected_index = 1),
        ),
        (
            SectionId::Shell,
            Box::new(|s: &mut AppState| s.shell.should_quit = true),
        ),
    ];

    assert_eq!(
        writers.len(),
        SectionId::COUNT,
        "every section needs a writer here, or its slot is untested"
    );

    for (id, write) in writers {
        let mut state = AppState::default();
        let seen = state.versions();
        write(&mut state);
        assert_eq!(
            state.changed_since(&seen),
            vec![id],
            "writing {id:?} did not move exactly its own slot"
        );
    }
}

#[test]
fn a_daemon_publish_bumps_fleet_even_though_it_arrives_through_an_arc() {
    // The poller writes into `Arc<Mutex<..>>` cells the render path only reads
    // by `&`, so without the folded generation a new ASK would never move the
    // fleet version and a subscriber would never hear about it.
    use std::sync::atomic::Ordering;

    let mut state = AppState::default();
    let seen = state.versions();

    assert!(
        !state.refresh_daemon_attention_generation(),
        "folding an unchanged generation bumped"
    );
    assert!(state.changed_since(&seen).is_empty());

    state.fleet.daemon_attention_generation.fetch_add(1, Ordering::Release);
    assert!(
        state.refresh_daemon_attention_generation(),
        "a publish the section had not seen reported no change"
    );
    assert_eq!(state.changed_since(&seen), vec![SectionId::Fleet]);

    let settled = state.versions();
    assert!(!state.refresh_daemon_attention_generation());
    assert!(state.changed_since(&settled).is_empty());
}
