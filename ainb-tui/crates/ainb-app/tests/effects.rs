// ABOUTME: One behavioural test per effect kind: dispatching the intent that
// asks for host work returns exactly that effect, performs none of it, and
// moves exactly the section versions the reducer should.

use ainb_app::app::NoRenderer;
use ainb_app::app::screens::ids;
use ainb_app::app::{TerminalTarget, ToolTerminal};
use ainb_app::models::{Session, Workspace};
use ainb_app::{AppState, CommandId, Effect, Intent, Keymap, SectionId, dispatch};

fn bumped(before: &[u64], after: &[u64]) -> Vec<SectionId> {
    SectionId::ALL
        .into_iter()
        .filter(|id| before[id.index()] != after[id.index()])
        .collect()
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

/// One scratch `HOME` for the whole binary, set once before any test reads
/// the environment, so parallel tests never swap it under each other.
fn isolated_home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("scratch home");
        std::env::set_var("HOME", home.path());
        home
    })
    .path()
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
        vec![Effect::OpenEditor(
            "/parity/api/worktrees/feat-login".into()
        )]
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
        vec![Effect::AttachTerminal(TerminalTarget::Session(session_id))]
    );
    assert_eq!(bumped(&before, &state.versions()), Vec::<SectionId>::new());
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
            "scratch".to_string()
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

    assert_eq!(
        effects,
        vec![Effect::AttachTerminal(TerminalTarget::WorkspaceShell {
            workspace_index: 0,
            target_dir: Some("/parity/api/worktrees/feat-login".into()),
        })]
    );
    assert_eq!(bumped(&before, &state.versions()), Vec::<SectionId>::new());
}

#[test]
fn attach_interactive_returns_attach_terminal_in_place() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = session_list_with_selection("/parity/api/worktrees/feat-login");
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("session_list.attach_interactive"),
    );

    assert_eq!(
        effects,
        vec![Effect::AttachTerminal(TerminalTarget::InPlace)]
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
    let tmux_available = std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|output| output.status.success());
    if !tmux_available {
        eprintln!("SKIP: tmux unavailable");
        return;
    }
    let session = format!("ainb-effects-detach-{}", std::process::id());
    let created = std::process::Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "sh"])
        .status()
        .is_ok_and(|status| status.success());
    assert!(created, "failed to create tmux session");

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
    let attached = state.enter_interactive_pane(24, 80);
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        command("embed_interactive.detach"),
    );
    let after = state.versions();
    let still_interactive = state.is_interactive_pane();
    state.release_interactive_pane();
    let _ = std::process::Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .status();

    assert!(attached, "the host attach under test needs a live pane");
    assert_eq!(effects, vec![Effect::Detach]);
    assert_eq!(bumped(&before, &after), Vec::<SectionId>::new());
    assert!(
        still_interactive,
        "the reducer left the release to the host"
    );
}
