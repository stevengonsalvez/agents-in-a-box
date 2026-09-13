// ABOUTME: One behavioural test per effect kind: dispatching the intent that
// asks for host work returns exactly that effect, performs none of it, and
// moves exactly the section versions the reducer should.

use ainb_app::app::NoRenderer;
use ainb_app::app::screens::ids;
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

fn isolated_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("scratch home");
    std::env::set_var("HOME", home.path());
    home
}

#[test]
fn open_in_editor_returns_open_editor_for_the_selected_worktree() {
    let _home = isolated_home();
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
