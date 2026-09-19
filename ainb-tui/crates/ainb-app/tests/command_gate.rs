#![allow(missing_docs)]

// ABOUTME: A named command stops at the topmost overlay and then reaches only
// the global rows. Checked over every parity fixture screen with each overlay
// `active_contexts` pushes, and by dispatching a command beneath an overlay
// and the overlay's own command.

#[path = "parity/support.rs"]
mod support;

#[path = "support/home.rs"]
mod home;

use std::path::Path;

use ainb_app::app::keymap::{KeyContext, SubContext, active_contexts, command_contexts};
use ainb_app::app::state::{ConfirmAction, ConfirmationDialog, FocusedPane};
use ainb_app::app::{NoRenderer, TmuxSessionName};
use ainb_app::{AppState, CommandId, Intent, Keymap, dispatch};
use support::ParityFixture;

fn fixtures() -> Vec<(String, ParityFixture)> {
    home::shared();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity");
    ParityFixture::all_in(&dir)
        .into_iter()
        .map(|(name, path)| {
            let fixture = ParityFixture::load(&path).unwrap_or_else(|error| panic!("{error}"));
            (name, fixture)
        })
        .collect()
}

fn confirm_dialog() -> ConfirmationDialog {
    ConfirmationDialog {
        title: "Stop session".to_string(),
        message: "Stop it?".to_string(),
        confirm_action: ConfirmAction::DismissNotifyPrompt,
        selected_option: false,
        warning: None,
        options: None,
        selected_index: 0,
    }
}

type Overlay = (&'static str, fn(&mut AppState) -> bool);

/// Each overlay `active_contexts` pushes, as a name and a setter that opens it
/// on a state and says whether this screen can show it.
fn overlays() -> Vec<Overlay> {
    use ainb_app::app::screens::ids;
    vec![
        ("confirm dialog", |state| {
            state.shell.confirmation_dialog = Some(confirm_dialog());
            true
        }),
        ("help", |state| {
            state.shell.help_visible = true;
            true
        }),
        ("other tmux rename", |state| {
            state.tmux.other_tmux_rename_mode = true;
            true
        }),
        ("ssh rename", |state| {
            state.ssh.ssh_session_rename_mode = true;
            true
        }),
        ("session rename", |state| {
            state.session_labels.session_label_rename_mode = true;
            true
        }),
        ("config popup", |state| {
            state.config.config_popup_state.show_popup = true;
            true
        }),
        ("auth provider popup", |state| {
            state.onboarding.auth_provider_popup_state.show_popup = true;
            true
        }),
        ("session recovery overlay", |state| {
            state.recovery.session_recovery_state.recovery_overlay =
                Some(ainb_app::components::session_recovery::RecoveryOverlay {
                    title: "Cleanup".to_string(),
                    results: Vec::new(),
                    scroll_offset: 0,
                });
            state.shell.current_screen == ids::SESSION_RECOVERY
        }),
        ("skill manager sync confirm", |state| {
            state.skills.skill_manager_state.sync_confirm = Some(
                ainb_app::components::skill_manager_screen::SyncConfirmState {
                    target: "claude".to_string(),
                    label: "sync".to_string(),
                    plan: Vec::new(),
                    scroll: 0,
                },
            );
            state.shell.current_screen == ids::SKILL_MANAGER
        }),
        ("setup menu confirm", |state| {
            state.onboarding.setup_menu_state.showing_confirmation = true;
            state.shell.current_screen == ids::SETUP_MENU
        }),
        ("daemons overlay", |state| {
            state.hangar.daemons_state.error_open =
                Some(ainb_app::fleet::daemons::DaemonKind::HangarDaemon);
            state.shell.current_screen == ids::DAEMONS
        }),
        ("live terminal pane", |state| {
            state.tmux.embed_session = TmuxSessionName::new("ainb-gate");
            state.shell.focused_pane = FocusedPane::Preview;
            state.shell.current_screen == ids::SESSION_LIST
        }),
    ]
}

/// Whether `contexts` holds a context of the screen the state shows.
fn has_screen_context(contexts: &[KeyContext], state: &AppState) -> bool {
    contexts.iter().any(|context| {
        matches!(context, KeyContext::Screen(screen, SubContext::None) if *screen == state.shell.current_screen)
    })
}

#[test]
fn every_overlay_on_every_fixture_screen_cuts_commands_to_itself_and_global() {
    let mut checked = 0;
    for (name, fixture) in fixtures() {
        let base = fixture.build();
        let open = command_contexts(&base);
        if open != active_contexts(&base) {
            // A fixture that opens an overlay itself (help): already cut.
            assert!(!has_screen_context(&open, &base), "{name}: {open:?}");
            assert_eq!(open.last(), Some(&KeyContext::Global), "{name}");
            checked += 1;
            continue;
        }
        // A screen whose keys a component owns (the repo picker) has no table
        // rows to reach, so there is nothing beneath an overlay to cut.
        if !has_screen_context(&open, &base) {
            continue;
        }

        for (overlay, open_it) in overlays() {
            let mut state = fixture.build();
            if !open_it(&mut state) {
                continue;
            }
            let active = active_contexts(&state);
            let commands = command_contexts(&state);
            assert!(
                has_screen_context(&active, &state),
                "{name} + {overlay}: keys still reach the screen"
            );
            assert!(
                !has_screen_context(&commands, &state),
                "{name} + {overlay}: a command reached the screen beneath: {commands:?}"
            );
            assert_eq!(
                commands.last(),
                Some(&KeyContext::Global),
                "{name} + {overlay}: global rows stay reachable"
            );
            assert!(commands.len() < active.len(), "{name} + {overlay}");
            checked += 1;
        }
    }
    assert!(
        checked >= 10 * 7,
        "every fixture with every screen-wide overlay, got {checked}"
    );
}

fn command(state: &mut AppState, id: &str) -> Vec<ainb_app::Effect> {
    dispatch(
        state,
        &Keymap::defaults(),
        &mut NoRenderer,
        Intent::Command(CommandId::new(id), serde_json::Value::Null),
    )
}

/// A global palette row still runs by name under an open dialog.
#[test]
fn a_global_command_runs_under_a_confirmation_dialog() {
    let (_, fixture) = fixtures().into_iter().next().expect("a fixture");
    let mut state = fixture.build();
    state.shell.confirmation_dialog = Some(confirm_dialog());
    assert_ne!(
        state.shell.current_screen,
        ainb_app::app::screens::ids::LEARNINGS
    );

    let _ = command(&mut state, "global.open_learnings");

    assert_eq!(
        state.shell.current_screen,
        ainb_app::app::screens::ids::LEARNINGS
    );
}

/// With the live terminal pane focused, a session list command beneath it
/// changes nothing and the pane's own release still runs.
#[test]
fn the_live_terminal_pane_blocks_the_list_beneath_but_not_its_release() {
    use ainb_app::app::screens::ids;

    let mut state = fixtures()
        .into_iter()
        .map(|(_, fixture)| fixture.build())
        .find(|state| state.shell.current_screen == ids::SESSION_LIST)
        .expect("a session list fixture");
    state.tmux.embed_session = TmuxSessionName::new("ainb-gate");
    state.shell.focused_pane = FocusedPane::Preview;
    assert!(state.is_interactive_pane());
    let before = state.versions();

    let _ = dispatch(
        &mut state,
        &Keymap::defaults(),
        &mut NoRenderer,
        ainb_app::app::pointer::focus_session_pane(&FocusedPane::Sessions),
    );
    assert_eq!(state.versions(), before, "the list beneath did not move");
    assert!(state.is_interactive_pane());

    let effects = command(&mut state, "embed_interactive.detach");
    assert_eq!(
        effects,
        vec![ainb_app::Effect::Detach],
        "the pane's own release ran and asks the host to detach"
    );
}
