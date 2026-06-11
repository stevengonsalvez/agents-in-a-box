//! P4.7 RED — settings reducer + key-material redaction behaviour.
//!
//! The settings screen (hotkey `,`) has four j/k-navigable sections: daemon
//! connection, providers, LLM keys, and workspace switch. These tests pin the
//! pure reducer contract: section navigation, the key-entry modal flow (with the
//! crucial regression that the entered key never leaks into a `Debug` repr or a
//! log), the workspace-switch confirm modal, and the daemon-disconnect status
//! flip.

use ainb_hangar_proto::settings::{HealthSnapshot, KeyRow, ProviderRow, WorkspaceRow};
use ainb_plugin_hangar::screen::settings::{
    reduce_settings, ConnectionStatus, KeyMaterial, SettingsEvent, SettingsIntent, SettingsSection,
    SettingsState,
};

fn health() -> HealthSnapshot {
    HealthSnapshot {
        socket_path: "/tmp/hangar.sock".into(),
        pid: 4242,
        uptime_secs: 3600,
        version: "0.1.0".into(),
        connected: true,
    }
}

fn providers() -> Vec<ProviderRow> {
    vec![
        ProviderRow {
            name: "claude".into(),
            online: true,
        },
        ProviderRow {
            name: "codex".into(),
            online: false,
        },
    ]
}

fn keys() -> Vec<KeyRow> {
    vec![KeyRow {
        provider: "claude".into(),
        masked: "sk-…abcd".into(),
    }]
}

fn workspaces() -> Vec<WorkspaceRow> {
    vec![
        WorkspaceRow {
            id: "ws1".into(),
            slug: "acme".into(),
            name: "Acme".into(),
            current: true,
            default: true,
        },
        WorkspaceRow {
            id: "ws2".into(),
            slug: "globex".into(),
            name: "Globex".into(),
            current: false,
            default: false,
        },
    ]
}

fn state() -> SettingsState {
    SettingsState::new(health(), providers(), keys(), workspaces())
}

/// j/k cycle through the five settings sections.
#[test]
fn j_k_navigates_settings_sections() {
    let s = state();
    assert_eq!(s.section(), SettingsSection::Daemon);
    let s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    assert_eq!(s.section(), SettingsSection::Providers);
    let s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    assert_eq!(s.section(), SettingsSection::Keys);
    let s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    assert_eq!(s.section(), SettingsSection::Workspaces);
    let s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    assert_eq!(s.section(), SettingsSection::Members);
    // Clamps at the bottom (Members, e38.11).
    let s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    assert_eq!(s.section(), SettingsSection::Members);
    let s = reduce_settings(&s, SettingsEvent::Key('k')).state;
    assert_eq!(s.section(), SettingsSection::Workspaces);
}

/// `n` on the keys section opens the key-entry modal.
#[test]
fn n_on_keys_section_opens_key_entry_modal() {
    let mut s = state();
    // Navigate to the Keys section.
    s = reduce_settings(&s, SettingsEvent::Key('j')).state; // Providers
    s = reduce_settings(&s, SettingsEvent::Key('j')).state; // Keys
    let out = reduce_settings(&s, SettingsEvent::Key('n'));
    assert!(out.state.key_entry_open());
    // `n` on a non-keys section does nothing.
    let on_daemon = reduce_settings(&state(), SettingsEvent::Key('n'));
    assert!(!on_daemon.state.key_entry_open());
}

/// Entering a key in the modal and pressing Enter emits a keychain-write intent
/// whose log form is masked (no raw key material).
#[test]
fn key_entry_enter_emits_keychain_write_intent_with_masked_log() {
    let mut s = state();
    s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    s = reduce_settings(&s, SettingsEvent::Key('j')).state; // Keys
    s = reduce_settings(&s, SettingsEvent::Key('n')).state; // open modal
    for c in "sk-secret-value-1234".chars() {
        s = reduce_settings(&s, SettingsEvent::Key(c)).state;
    }
    let out = reduce_settings(&s, SettingsEvent::Key('\n'));
    match out.intent {
        Some(SettingsIntent::KeychainWrite { provider: _, key }) => {
            // The intent's loggable form must be masked, not the raw value.
            let logged = format!("{key:?}");
            assert!(
                !logged.contains("sk-secret-value-1234"),
                "raw key leaked: {logged}"
            );
            assert!(logged.contains("REDACTED") || logged.contains("***") || logged.contains('…'));
            // But the real value is still retrievable for the actual keychain write.
            assert_eq!(key.expose(), "sk-secret-value-1234");
        }
        other => panic!("expected keychain write intent, got {other:?}"),
    }
    assert!(!out.state.key_entry_open());
}

/// The `KeyMaterial` newtype's `Debug` impl redacts the value (regression test).
#[test]
fn key_entry_value_never_appears_in_debug_repr() {
    let km = KeyMaterial::new("sk-super-secret-0000".to_string());
    let dbg = format!("{km:?}");
    assert!(
        !dbg.contains("sk-super-secret-0000"),
        "Debug leaked key: {dbg}"
    );
    // And it round-trips through expose for the real write.
    assert_eq!(km.expose(), "sk-super-secret-0000");
}

/// P5.5: `s` on the Workspace pane sets the SELECTED workspace active and emits
/// a `SwitchWorkspace` intent carrying the row's stable ULID id (never the
/// slug). No confirm modal — it is a single keystroke.
#[test]
fn s_sets_selected_workspace_active() {
    let mut s = state();
    s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    s = reduce_settings(&s, SettingsEvent::Key('j')).state; // Workspaces
                                                            // Select the non-current workspace (ws2) then press `s`.
    s = reduce_settings(&s, SettingsEvent::Key('J')).state;
    let out = reduce_settings(&s, SettingsEvent::Key('s'));
    match out.intent {
        Some(SettingsIntent::SwitchWorkspace(id)) => {
            assert_eq!(id, "ws2", "switch must carry the ULID id, not the slug");
        }
        other => panic!("expected SwitchWorkspace intent, got {other:?}"),
    }
}

/// P5.5: `d` toggles the default for the selected workspace; `n` opens the
/// new-workspace flow; `r` renames the selected one. All scoped to the
/// Workspace pane.
#[test]
fn d_n_r_emit_workspace_intents() {
    let mut s = state();
    s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    s = reduce_settings(&s, SettingsEvent::Key('j')).state; // Workspaces (ws1 selected)

    let d = reduce_settings(&s, SettingsEvent::Key('d'));
    assert!(matches!(
        d.intent,
        Some(SettingsIntent::ToggleDefault(ref id)) if id == "ws1"
    ));

    let n = reduce_settings(&s, SettingsEvent::Key('n'));
    assert!(matches!(n.intent, Some(SettingsIntent::NewWorkspace)));

    let r = reduce_settings(&s, SettingsEvent::Key('r'));
    assert!(matches!(
        r.intent,
        Some(SettingsIntent::RenameWorkspace(ref id)) if id == "ws1"
    ));
}

/// The Workspace pane keys are section-scoped: `s`/`d`/`r` do nothing on the
/// Daemon section (no accidental switch from another pane).
#[test]
fn workspace_keys_inert_outside_workspace_pane() {
    let s = state(); // Daemon section
    assert!(reduce_settings(&s, SettingsEvent::Key('s'))
        .intent
        .is_none());
    assert!(reduce_settings(&s, SettingsEvent::Key('d'))
        .intent
        .is_none());
    assert!(reduce_settings(&s, SettingsEvent::Key('r'))
        .intent
        .is_none());
}

/// A daemon-disconnected event flips the connection section status to red.
#[test]
fn event_daemon_disconnected_flips_connection_section_status_to_red() {
    let s = state();
    assert_eq!(s.connection_status(), ConnectionStatus::Connected);
    let out = reduce_settings(&s, SettingsEvent::DaemonDisconnected);
    assert_eq!(
        out.state.connection_status(),
        ConnectionStatus::Disconnected
    );
}

/// Esc aborts the key-entry modal without emitting an intent.
#[test]
fn esc_aborts_key_entry_modal() {
    let mut s = state();
    s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    s = reduce_settings(&s, SettingsEvent::Key('j')).state; // Keys
    s = reduce_settings(&s, SettingsEvent::Key('n')).state; // open
    let out = reduce_settings(&s, SettingsEvent::Esc);
    assert!(!out.state.key_entry_open());
    assert!(out.intent.is_none());
}
