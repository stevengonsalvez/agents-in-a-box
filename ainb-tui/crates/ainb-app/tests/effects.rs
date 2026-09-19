// ABOUTME: One behavioural test per effect kind: dispatching the intent that
// asks for host work returns exactly that effect, performs none of it, and
// moves exactly the section versions the reducer should.

#[path = "support/home.rs"]
mod home;

use ainb_app::app::NoRenderer;
use ainb_app::app::screens::ids;
use ainb_app::app::{TerminalTarget, TmuxSessionName, ToolTerminal};
use ainb_app::models::{Session, Workspace};
use ainb_app::{AppState, CommandId, Effect, Intent, Keymap, SectionId, dispatch};

fn bumped(before: &[u64], after: &[u64]) -> Vec<SectionId> {
    SectionId::ALL
        .into_iter()
        .filter(|id| before[id.index()] != after[id.index()])
        .collect()
}

/// The editor effect for `path`, with the test home's (empty) preference.
fn open_editor(path: &str) -> Effect {
    Effect::OpenEditor {
        path: ainb_app::app::EditorPath::new(path).expect("absolute"),
        preferred_editor: None,
    }
}

fn command(name: &str) -> Intent {
    Intent::Command(CommandId::new(name), serde_json::Value::Null)
}

/// A session list with one selected session whose worktree is `path`.
fn session_list_with_selection(path: &str) -> AppState {
    let mut state = AppState::new();
    let mut workspace = Workspace::new("api".to_string(), "/parity/api".into());
    workspace.add_session(Session::new("feat-login".to_string(), path.to_string()));
    state.sessions.workspaces = vec![workspace];
    state.sessions.selected_workspace_index = Some(0);
    state.sessions.selected_session_index = Some(0);
    state.shell.current_screen = ids::SESSION_LIST.to_string();
    state
}

/// The home directory every test in this binary reads, taken once so no test
/// can move it under another.
fn isolated_home() -> &'static std::path::Path {
    home::shared()
}

#[test]
fn open_in_editor_returns_open_editor_for_the_selected_worktree() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with_selection("/parity/api/worktrees/feat-login");
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("session_list.editor"),
    );

    assert_eq!(
        effects,
        vec![open_editor("/parity/api/worktrees/feat-login")]
    );
    assert_eq!(bumped(&before, &state.versions()), Vec::<SectionId>::new());
    assert!(
        state.take_effects().is_empty(),
        "dispatch drained the outbox"
    );
}

#[test]
fn attach_on_a_session_returns_attach_terminal_for_that_session() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with_selection("/parity/api/worktrees/feat-login");
    state.sessions.workspaces[0].sessions[0].tmux_session_name = Some("tmux_api_feat".to_string());
    let session_id = state.sessions.workspaces[0].sessions[0].id;
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("session_list.attach_tmux"),
    );

    assert_eq!(
        effects,
        vec![Effect::AttachTerminal(TerminalTarget::Session {
            id: session_id,
            tmux_session: TmuxSessionName::new("tmux_api_feat").expect("valid name"),
        })]
    );
    assert_eq!(
        bumped(&before, &state.versions()),
        vec![SectionId::Sessions],
        "the reducer marks the session attached before the host attaches"
    );
    assert!(state.sessions.workspaces[0].sessions[0].is_attached);
    assert!(
        state.shell.pending_async_action.is_none(),
        "no async work queued for the attach"
    );
}

#[test]
fn attach_on_an_other_tmux_row_returns_attach_terminal_by_name() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = ids::SESSION_LIST.to_string();
    state.sessions.selected_workspace_index = None;
    state.tmux.other_tmux_sessions = vec![ainb_app::models::other_tmux::OtherTmuxSession::new(
        "scratch".to_string(),
        false,
        1,
    )];
    state.tmux.selected_other_tmux_index = Some(0);
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("session_list.attach_tmux"),
    );

    assert_eq!(
        effects,
        vec![Effect::AttachTerminal(TerminalTarget::Tmux(
            TmuxSessionName::new("scratch").expect("valid name")
        ))]
    );
    assert_eq!(bumped(&before, &state.versions()), Vec::<SectionId>::new());
}

#[test]
fn witr_returns_attach_terminal_for_the_witr_tool() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with_selection("/parity/api");
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("session_list.witr"),
    );

    assert_eq!(
        effects,
        vec![Effect::AttachTerminal(TerminalTarget::Tool(
            ToolTerminal::Witr
        ))]
    );
    assert_eq!(bumped(&before, &state.versions()), Vec::<SectionId>::new());
}

#[test]
fn quick_shell_returns_attach_terminal_for_the_workspace_shell_at_the_worktree() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with_selection("/parity/api/worktrees/feat-login");
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("session_list.quick_shell"),
    );
    let shell_name = state.sessions.workspaces[0]
        .shell_session
        .as_ref()
        .expect("the reducer records the shell it asks the host to open")
        .tmux_session_name
        .clone();

    assert_eq!(
        effects,
        vec![Effect::AttachTerminal(TerminalTarget::WorkspaceShell {
            workspace_path: "/parity/api".into(),
            tmux_session: TmuxSessionName::new(shell_name).expect("valid name"),
            new_shell: true,
            target_dir: Some("/parity/api/worktrees/feat-login".into()),
        })]
    );
    assert_eq!(
        bumped(&before, &state.versions()),
        vec![SectionId::Sessions],
        "the reducer records the workspace shell; the host only creates its tmux session"
    );
}

#[test]
fn attach_interactive_returns_attach_terminal_in_place() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with_selection("/parity/api/worktrees/feat-login");
    state.sessions.workspaces[0].sessions[0].tmux_session_name = Some("tmux_api_feat".to_string());
    let show_menu_bar = state.config.app_config.ui_preferences.show_session_menu_bar;
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("session_list.attach_interactive"),
    );

    assert_eq!(
        effects,
        vec![Effect::AttachTerminal(TerminalTarget::InPlace {
            tmux_session: TmuxSessionName::new("tmux_api_feat").expect("valid name"),
            show_menu_bar,
        })]
    );
    assert_eq!(bumped(&before, &state.versions()), Vec::<SectionId>::new());
    assert!(
        !state.is_interactive_pane(),
        "the reducer attached nothing itself"
    );
}

#[test]
fn detach_while_interactive_returns_detach_and_leaves_the_pane_to_the_host() {
    isolated_home();
    let session = "ainb-effects-detach".to_string();

    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = ids::SESSION_LIST.to_string();
    state.sessions.selected_workspace_index = None;
    state.tmux.other_tmux_sessions = vec![ainb_app::models::other_tmux::OtherTmuxSession::new(
        session.clone(),
        false,
        1,
    )];
    state.tmux.selected_other_tmux_index = Some(0);
    // The host's side of `A`: run the effect the key returns and report the
    // client it opened. The client stays with the host, so none is needed here.
    for effect in dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("session_list.attach_interactive"),
    ) {
        if let Effect::AttachTerminal(TerminalTarget::InPlace { tmux_session, .. }) = effect {
            let report = ainb_app::app::reports::in_place_opened(tmux_session.as_str());
            let _ = dispatch(&mut state, &keymap, &mut NoRenderer, report);
        }
    }
    let attached = state.is_interactive_pane();
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("embed_interactive.detach"),
    );
    let after = state.versions();
    let still_interactive = state.is_interactive_pane();

    assert!(attached, "the host attach under test needs a live pane");
    assert_eq!(effects, vec![Effect::Detach]);
    assert_eq!(bumped(&before, &after), Vec::<SectionId>::new());
    assert!(
        still_interactive,
        "the reducer left the release to the host"
    );
}

#[test]
fn ctrl_v_on_the_repo_picker_returns_paste_clipboard_and_the_text_lands_in_the_filter() {
    use ainb_app::app::state::{NewSessionState, NewSessionStep};
    use ainb_app::components::new_session::pick_repo::PickRepoState;

    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = ids::NEW_SESSION.to_string();
    state.new_session.new_session_state = Some(NewSessionState {
        step: NewSessionStep::PickRepo,
        pick_repo_state: Some(PickRepoState::from_disk_no_locals()),
        ..NewSessionState::default()
    });
    let filter = |state: &AppState| {
        state
            .new_session
            .new_session_state
            .as_ref()
            .and_then(|ns| ns.pick_repo_state.as_ref())
            .map(|pick| pick.filter.clone())
    };
    let ctrl_v = Intent::Key(ainb_app::Chord::parse("ctrl+v").expect("valid chord"));
    let before = state.versions();

    let effects = dispatch(&mut state, &keymap, &mut NoRenderer, ctrl_v);

    assert_eq!(effects, vec![Effect::PasteClipboard]);
    assert_eq!(
        filter(&state).as_deref(),
        Some(""),
        "the reducer read no clipboard"
    );
    let asked = state.versions();
    // The picker's key handler runs against its section even when all it
    // does is ask for the paste.
    assert_eq!(bumped(&before, &asked), vec![SectionId::NewSession]);

    // What the host does with the clipboard's text: a Text intent.
    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Text("owner/repo".to_string()),
    );
    assert!(effects.is_empty());
    assert_eq!(filter(&state).as_deref(), Some("owner/repo"));
    assert_eq!(
        bumped(&asked, &state.versions()),
        vec![SectionId::NewSession]
    );
}

#[test]
fn ctrl_v_in_a_config_text_popup_returns_paste_clipboard_and_changes_nothing() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = ids::CONFIG.to_string();
    state
        .config
        .config_popup_state
        .open_text("Branch prefix", "", "branch_prefix", "");
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Key(ainb_app::Chord::parse("ctrl+v").expect("valid chord")),
    );

    assert_eq!(effects, vec![Effect::PasteClipboard]);
    assert_eq!(bumped(&before, &state.versions()), Vec::<SectionId>::new());
}

/// `ClaudeLogin` is emitted from the tick once Docker is ready, so the part a
/// test can pin without Docker is how the reducer takes the host's report.
#[test]
fn the_oauth_login_report_succeeds_only_with_credentials_written() {
    use ainb_app::app::state::{AuthMethod, AuthSetupState};

    let home = isolated_home();
    let auth_dir = home.join("oauth-report");
    std::fs::create_dir_all(&auth_dir).expect("auth dir");
    let setup = || AuthSetupState {
        selected_method: AuthMethod::OAuth,
        api_key_input: String::new(),
        is_processing: true,
        error_message: None,
        show_cursor: false,
    };

    // A clean exit that wrote nothing is still a failure.
    let mut state = AppState::new();
    state.onboarding.auth_setup_state = Some(setup());
    assert!(!state.finish_oauth_login(&auth_dir, true));
    let auth = state.onboarding.auth_setup_state.as_ref().expect("still on the auth menu");
    assert!(!auth.is_processing);
    assert!(
        auth.error_message
            .as_deref()
            .is_some_and(|m| m.contains("Authentication failed"))
    );

    // Credentials and a clean exit land on the session list.
    std::fs::write(auth_dir.join(".credentials.json"), "{}").expect("credentials");
    let mut state = AppState::new();
    state.onboarding.auth_setup_state = Some(setup());
    assert!(
        !state.finish_oauth_login(&auth_dir, false),
        "a failed exit fails"
    );
    assert!(state.finish_oauth_login(&auth_dir, true));
    assert!(state.onboarding.auth_setup_state.is_none());
    assert_eq!(state.shell.current_screen, ids::SESSION_LIST);
}

/// The run loop's top-of-loop drain exists for effects queued outside a
/// dispatch: the host re-queues what a clipboard paste returns, and a tick
/// that errors leaves what it queued. They wait on the outbox, and the next
/// hand-over (a dispatch or a drain) returns them first, in order.
#[test]
fn effects_queued_outside_dispatch_wait_for_the_next_hand_over() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with_selection("/parity/api/worktrees/feat-login");
    state.emit(Effect::Detach);

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("session_list.editor"),
    );

    assert_eq!(
        effects,
        vec![
            Effect::Detach,
            open_editor("/parity/api/worktrees/feat-login"),
        ]
    );
    state.emit(Effect::Detach);
    assert_eq!(state.take_effects(), vec![Effect::Detach]);
    assert!(state.take_effects().is_empty());
}
