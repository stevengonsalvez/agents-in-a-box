// ABOUTME: Report commands: a host tells the reducer what it measured or how
// something it ran ended, and the reducer, not the host, changes state.

#[path = "support/home.rs"]
mod home;

use ainb_app::app::NoRenderer;
use ainb_app::app::reports;
use ainb_app::{AppState, Keymap, SectionId, dispatch};

fn bumped(before: &[u64], after: &[u64]) -> Vec<SectionId> {
    SectionId::ALL
        .into_iter()
        .filter(|id| before[id.index()] != after[id.index()])
        .collect()
}

/// One scratch `HOME` for the binary, so the migration's save lands nowhere
/// real.
fn isolated_home() {
    home::shared();
}

#[test]
fn the_width_report_turns_saved_column_counts_into_fractions_of_the_host() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.config.app_config.ui_preferences.home_sidebar_width = Some(40);
    state.config.app_config.ui_preferences.skill_manager_sources_width = Some(32);
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        reports::migrate_layout_widths(160),
    );

    // The migrated widths are written by the host, after this step.
    assert_eq!(
        effects,
        vec![ainb_app::Effect::Persist(
            ainb_app::app::Persist::AppConfig {
                config: ainb_app::app::Snapshot(state.config.app_config.clone()),
                keys: [
                    "ui_preferences.home_sidebar_fraction",
                    "ui_preferences.home_sidebar_width",
                    "ui_preferences.sessions_sidebar_fraction",
                    "ui_preferences.sessions_sidebar_width",
                    "ui_preferences.skill_manager_sources_fraction",
                    "ui_preferences.skill_manager_sources_width",
                ]
                .map(String::from)
                .to_vec(),
            }
        )]
    );
    let prefs = &state.config.app_config.ui_preferences;
    assert_eq!(prefs.home_sidebar_fraction, Some(0.25));
    assert_eq!(prefs.skill_manager_sources_fraction, Some(0.2));
    assert_eq!(
        (prefs.home_sidebar_width, prefs.skill_manager_sources_width),
        (None, None)
    );
    assert_eq!(bumped(&before, &state.versions()), vec![SectionId::Config]);
}

#[test]
fn the_width_report_with_nothing_to_migrate_changes_nothing() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.config.app_config.ui_preferences.home_sidebar_width = None;
    state.config.app_config.ui_preferences.skill_manager_sources_width = None;
    let before = state.versions();

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        reports::migrate_layout_widths(160),
    );

    assert!(bumped(&before, &state.versions()).is_empty());
}

use ainb_app::app::reports::{AttachOutcome, AttachedTo, EditorOutcome, ShellCd, ShellOutcome};
use ainb_app::app::state::{AsyncAction, NotificationType};
use ainb_app::models::{Session, SessionStatus, Workspace};

/// One workspace at `/parity/api` holding one attached session with a tmux
/// session.
fn attached_session() -> (AppState, uuid::Uuid) {
    let mut state = AppState::new();
    let mut workspace = Workspace::new("api".to_string(), "/parity/api".into());
    let mut session = Session::new("feat-login".to_string(), "/parity/api/wt".to_string());
    session.tmux_session_name = Some("tmux_api_feat".to_string());
    session.set_status(SessionStatus::Running);
    session.mark_attached();
    let id = session.id;
    workspace.add_session(session);
    state.sessions.workspaces = vec![workspace];
    (state, id)
}

/// Dispatch `report` and return the sections it moved. Reports queue no
/// effects of their own.
fn report(state: &mut AppState, report: ainb_app::Intent) -> Vec<SectionId> {
    isolated_home();
    let before = state.versions();
    let effects = dispatch(state, &Keymap::defaults(), &mut NoRenderer, report);
    assert!(effects.is_empty(), "a report asks the host for nothing");
    bumped(&before, &state.versions())
}

fn notices(state: &AppState, kind: &NotificationType) -> Vec<String> {
    state
        .shell
        .notifications
        .iter()
        .filter(|n| n.notification_type == *kind)
        .map(|n| n.message.clone())
        .collect()
}

#[test]
fn a_session_attach_that_ended_marks_the_session_detached() {
    let (mut state, id) = attached_session();

    let moved = report(
        &mut state,
        reports::attach_finished(&AttachedTo::Session(id), &AttachOutcome::Detached),
    );

    assert!(!state.sessions.workspaces[0].sessions[0].is_attached);
    assert!(notices(&state, &NotificationType::Error).is_empty());
    assert_eq!(moved, vec![SectionId::Sessions, SectionId::Shell]);
}

#[test]
fn a_failed_session_attach_with_its_target_alive_keeps_the_session_running() {
    let (mut state, id) = attached_session();

    let moved = report(
        &mut state,
        reports::attach_finished(
            &AttachedTo::Session(id),
            &AttachOutcome::Failed("exit code: Some(1)".to_string()),
        ),
    );

    let session = &state.sessions.workspaces[0].sessions[0];
    assert!(!session.is_attached);
    assert_eq!(session.status, SessionStatus::Running);
    let errors = notices(&state, &NotificationType::Error);
    assert!(
        errors.len() == 1 && errors[0].contains("tmux_api_feat") && errors[0].contains("Some(1)"),
        "{errors:?}"
    );
    assert!(state.shell.pending_async_action.is_none());
    assert_eq!(moved, vec![SectionId::Sessions, SectionId::Shell]);
}

#[test]
fn a_session_attach_whose_target_is_gone_stops_the_session_and_reloads_the_rows() {
    let (mut state, id) = attached_session();

    let moved = report(
        &mut state,
        reports::attach_finished(
            &AttachedTo::Session(id),
            &AttachOutcome::TargetMissing("can't find session".to_string()),
        ),
    );

    assert_eq!(
        state.sessions.workspaces[0].sessions[0].status,
        SessionStatus::Stopped
    );
    assert_eq!(notices(&state, &NotificationType::Error).len(), 1);
    assert!(matches!(
        state.shell.pending_async_action,
        Some(AsyncAction::RefreshWorkspaces)
    ));
    // The dropped tmux session handle is host-only state, so no tmux section
    // moves.
    assert_eq!(moved, vec![SectionId::Sessions, SectionId::Shell]);
}

#[test]
fn an_other_tmux_attach_reloads_the_other_tmux_rows_either_way() {
    for outcome in [
        AttachOutcome::Detached,
        AttachOutcome::Failed("nested".to_string()),
    ] {
        let mut state = AppState::new();
        let moved = report(
            &mut state,
            reports::attach_finished(&AttachedTo::Tmux("scratch".to_string()), &outcome),
        );
        assert!(matches!(
            state.shell.pending_async_action,
            Some(AsyncAction::ReloadOtherTmuxSessions)
        ));
        let errors = notices(&state, &NotificationType::Error);
        assert_eq!(
            errors.len(),
            usize::from(outcome != AttachOutcome::Detached),
            "{errors:?}"
        );
        assert_eq!(moved, vec![SectionId::Shell]);
    }
}

#[test]
fn a_tool_that_would_not_start_says_how_to_install_it() {
    for (tool, name) in [(AttachedTo::Witr, "witr"), (AttachedTo::Abtop, "abtop")] {
        let mut state = AppState::new();
        let moved = report(
            &mut state,
            reports::attach_finished(&tool, &AttachOutcome::NotInstalled),
        );
        let errors = notices(&state, &NotificationType::Error);
        assert!(
            errors.len() == 1 && errors[0].contains(&format!("`{name}` installed")),
            "{errors:?}"
        );
        assert_eq!(moved, vec![SectionId::Shell]);

        let mut state = AppState::new();
        let moved = report(
            &mut state,
            reports::attach_finished(&tool, &AttachOutcome::Detached),
        );
        assert!(notices(&state, &NotificationType::Error).is_empty());
        assert_eq!(moved, vec![SectionId::Shell]);
    }
}

/// The workspace at `/parity/api` with a shell record, as the reducer leaves
/// it when it queues the shell's attach.
fn workspace_with_shell() -> AppState {
    let mut state = AppState::new();
    let mut workspace = Workspace::new("api".to_string(), "/parity/api".into());
    workspace.set_shell_session(ainb_app::models::ShellSession::new_workspace_shell(
        "/parity/api".into(),
        "api",
    ));
    state.sessions.workspaces = vec![workspace];
    state
}

#[test]
fn a_prepared_shell_that_moved_records_its_directory_and_announces_a_new_shell() {
    let mut state = workspace_with_shell();

    let moved = report(
        &mut state,
        reports::shell_prepared(
            "/parity/api".as_ref(),
            &ShellOutcome::Ready {
                created: true,
                cd: ShellCd::Moved("/parity/api/wt".into()),
            },
        ),
    );

    let shell = state.sessions.workspaces[0].shell_session.as_ref().expect("shell");
    assert_eq!(
        shell.working_dir,
        std::path::PathBuf::from("/parity/api/wt")
    );
    let successes = notices(&state, &NotificationType::Success);
    assert!(
        successes.len() == 1 && successes[0].contains("api"),
        "{successes:?}"
    );
    assert_eq!(moved, vec![SectionId::Sessions, SectionId::Shell]);
}

#[test]
fn a_shell_cd_that_may_have_failed_warns_and_keeps_the_old_directory() {
    let mut state = workspace_with_shell();
    let before_dir = state.sessions.workspaces[0]
        .shell_session
        .as_ref()
        .expect("shell")
        .working_dir
        .clone();

    let _ = report(
        &mut state,
        reports::shell_prepared(
            "/parity/api".as_ref(),
            &ShellOutcome::Ready {
                created: false,
                cd: ShellCd::MaybeFailed("/parity/api/wt".into()),
            },
        ),
    );

    let shell = state.sessions.workspaces[0].shell_session.as_ref().expect("shell");
    assert_eq!(shell.working_dir, before_dir);
    assert_eq!(notices(&state, &NotificationType::Warning).len(), 1);
    assert!(
        notices(&state, &NotificationType::Success).is_empty(),
        "a reused shell is not announced"
    );

    let _ = report(
        &mut state,
        reports::shell_prepared(
            "/parity/api".as_ref(),
            &ShellOutcome::Ready {
                created: false,
                cd: ShellCd::Failed("no tmux".to_string()),
            },
        ),
    );
    assert_eq!(notices(&state, &NotificationType::Error).len(), 1);
}

#[test]
fn a_shell_that_could_not_be_created_says_so_and_changes_no_session() {
    let mut state = workspace_with_shell();

    let moved = report(
        &mut state,
        reports::shell_prepared(
            "/parity/api".as_ref(),
            &ShellOutcome::Failed("duplicate session".to_string()),
        ),
    );

    let errors = notices(&state, &NotificationType::Error);
    assert!(
        errors.len() == 1 && errors[0].contains("duplicate session"),
        "{errors:?}"
    );
    assert_eq!(moved, vec![SectionId::Shell]);
}

#[test]
fn the_abtop_setup_report_announces_either_outcome() {
    let mut state = AppState::new();
    let _ = report(&mut state, reports::abtop_setup_finished(true));
    assert_eq!(notices(&state, &NotificationType::Info).len(), 1);

    let mut state = AppState::new();
    let _ = report(&mut state, reports::abtop_setup_finished(false));
    assert_eq!(notices(&state, &NotificationType::Error).len(), 1);
}

#[test]
fn an_in_place_client_that_would_not_open_says_why() {
    let mut state = AppState::new();

    let moved = report(
        &mut state,
        reports::in_place_failed("tmux_api_feat", "no server running", false),
    );

    let errors = notices(&state, &NotificationType::Error);
    assert!(
        errors.len() == 1 && errors[0].contains("tmux_api_feat") && errors[0].contains("no server"),
        "{errors:?}"
    );
    assert!(!state.is_interactive_pane());
    assert_eq!(moved, vec![SectionId::Shell]);
}

/// A report for a client the preview no longer wants names nothing, so the
/// host closes the client rather than the pane showing it.
#[test]
fn an_in_place_report_for_a_row_that_is_not_selected_opens_nothing() {
    let mut state = AppState::new();
    state.shell.current_screen = ainb_app::app::screens::ids::SESSION_LIST.to_string();

    let moved = report(&mut state, reports::in_place_opened("tmux_api_feat"));

    assert!(!state.is_interactive_pane());
    assert!(state.embed_session_name().is_none());
    assert!(moved.is_empty(), "{moved:?}");
}

#[test]
fn a_plugin_action_the_runtime_could_not_deliver_says_so() {
    let mut state = AppState::new();

    let _ = report(
        &mut state,
        reports::plugin_action_undelivered("hangar-tui", "board.open_card"),
    );

    let errors = notices(&state, &NotificationType::Error);
    assert!(
        errors.len() == 1
            && errors[0].contains("hangar-tui")
            && errors[0].contains("board.open_card"),
        "{errors:?}"
    );
}

#[test]
fn a_detach_report_with_no_live_terminal_changes_nothing() {
    let mut state = AppState::new();
    assert!(report(&mut state, reports::detached()).is_empty());
}

#[test]
fn the_editor_report_announces_each_outcome() {
    for (outcome, kind, needle) in [
        (
            EditorOutcome::Opened("code".to_string()),
            NotificationType::Success,
            "code",
        ),
        (
            EditorOutcome::NoneFound,
            NotificationType::Error,
            "No editor found",
        ),
        (
            EditorOutcome::Failed("denied".to_string()),
            NotificationType::Error,
            "denied",
        ),
    ] {
        let mut state = AppState::new();
        let moved = report(&mut state, reports::editor_finished(&outcome));
        let found = notices(&state, &kind);
        assert!(
            found.len() == 1 && found[0].contains(needle),
            "{outcome:?}: {found:?}"
        );
        assert_eq!(moved, vec![SectionId::Shell], "{outcome:?}");
    }
}

#[test]
fn the_clipboard_report_names_the_error() {
    let mut state = AppState::new();
    let moved = report(&mut state, reports::clipboard_failed("no display"));
    let errors = notices(&state, &NotificationType::Error);
    assert!(
        errors.len() == 1 && errors[0].contains("no display"),
        "{errors:?}"
    );
    assert_eq!(moved, vec![SectionId::Shell]);
}

#[test]
fn the_login_report_lands_on_the_session_list_only_with_credentials() {
    use ainb_app::app::screens::ids;
    use ainb_app::app::state::{AuthMethod, AuthSetupState};

    let auth_dir = tempfile::tempdir().expect("auth dir");
    let setup = || AuthSetupState {
        selected_method: AuthMethod::OAuth,
        api_key_input: String::new(),
        is_processing: true,
        error_message: None,
        show_cursor: false,
    };

    let mut state = AppState::new();
    state.onboarding.auth_setup_state = Some(setup());
    let _ = report(&mut state, reports::login_finished(auth_dir.path(), true));
    assert!(
        state
            .onboarding
            .auth_setup_state
            .as_ref()
            .is_some_and(|auth| !auth.is_processing),
        "a clean exit that wrote nothing stays on the auth menu"
    );

    std::fs::write(auth_dir.path().join(".credentials.json"), "{}").expect("credentials");
    let mut state = AppState::new();
    state.onboarding.auth_setup_state = Some(setup());
    let moved = report(&mut state, reports::login_finished(auth_dir.path(), true));
    assert!(state.onboarding.auth_setup_state.is_none());
    assert_eq!(state.shell.current_screen, ids::SESSION_LIST);
    assert!(moved.contains(&SectionId::Onboarding), "{moved:?}");
}

/// The Codex pairing code is a short-lived credential. The report that
/// carries it can be dispatched to a remote reducer, so its serialised form
/// holds a handle, and only the reducer of the process that ran `pair` shows
/// the code.
#[test]
fn a_pairing_code_never_leaves_the_process_in_the_serialised_report() {
    use ainb_app::cli::daemon::Action;
    use ainb_app::fleet::daemons::probe::DaemonKind;

    const CODE: &str = "PAIR-7Q4X-K2M9";
    let mut state = AppState::new();
    let daemons = &mut state.hangar.daemons_state;
    daemons.dispatch(DaemonKind::HangarDaemon, Action::Pair);
    let [request] = daemons.take_action_requests()[..] else {
        panic!("one pair request");
    };

    let sealed = reports::DaemonActionReport {
        daemon: DaemonKind::HangarDaemon.id().to_string(),
        verb: Action::Pair.id().to_string(),
        generation: 0,
        ok: true,
        summary: CODE.to_string(),
        detail: format!("cmd: ainb daemon hangar-daemon pair\n\nstdout:\n{CODE}"),
        local: None,
    }
    .sealed(request.generation);
    let intent = reports::daemon_action_finished(&sealed);
    let wire = serde_json::to_string(&intent).expect("the intent serialises");
    assert!(
        !wire.contains(CODE),
        "the code left in the serialised report: {wire}"
    );

    // A reducer in another process gets the handle and nothing to redeem it
    // for; this one shows the code on the row.
    let _ = report(&mut state, intent);
    let outcome = &state.hangar.daemons_state.outcomes[DaemonKind::HangarDaemon.id()];
    assert_eq!(outcome.summary, CODE);
    let value: serde_json::Value = serde_json::from_str(&wire).expect("json");
    let args = value.pointer("/Command/1/report").expect("the report args");
    assert!(
        args["local"].is_string(),
        "the serialised report carries a handle"
    );
    assert!(args["summary"].as_str().is_some_and(|s| s.contains("pairing code ready")));

    // The redeemed code is on the row, and a mirror frame of that row still
    // never carries it.
    assert!(outcome.local_only);
    let frame = ainb_app::wire::section_json(
        &state,
        SectionId::Hangar,
        &ainb_app::wire::frame::HostId::local(),
    )
    .to_string();
    assert!(
        !frame.contains(CODE),
        "the redeemed code reached a frame: {frame}"
    );
    assert!(frame.contains(ainb_app::components::daemons::LOCAL_ONLY_SUMMARY));
}
