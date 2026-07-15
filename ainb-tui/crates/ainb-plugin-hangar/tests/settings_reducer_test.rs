//! P4.7 RED — settings reducer + key-material redaction behaviour.
//!
//! The settings screen (hotkey `,`) has four j/k-navigable sections: daemon
//! connection, providers, LLM keys, and workspace switch. These tests pin the
//! pure reducer contract: section navigation, the key-entry modal flow (with the
//! crucial regression that the entered key never leaks into a `Debug` repr or a
//! log), the workspace-switch confirm modal, and the daemon-disconnect status
//! flip.

use ainb_hangar_proto::settings::{HealthSnapshot, KeyRow, ProviderRow, WorkspaceRow};
use ainb_hangar_proto::snapshots::NotifyRuleWireRow;
use ainb_hangar_proto::{Channel, ChannelSet};
use ainb_plugin_hangar::screen::settings::{
    ConnectionStatus, KeyMaterial, NotifyScope, SettingsEvent, SettingsIntent, SettingsSection,
    SettingsState, reduce_settings,
};

/// The seeded routing grid: ask → web+os, error → os, waiting → board-only.
fn notify_rules() -> Vec<NotifyRuleWireRow> {
    vec![
        NotifyRuleWireRow {
            kind: "ask_user_question".into(),
            channels: ChannelSet::from_channels([Channel::Web, Channel::Os]),
            overridden: false,
        },
        NotifyRuleWireRow {
            kind: "error".into(),
            channels: ChannelSet::from_channels([Channel::Os]),
            overridden: false,
        },
        NotifyRuleWireRow {
            kind: "waiting".into(),
            channels: ChannelSet::NONE,
            overridden: false,
        },
    ]
}

/// A state parked on the Notifications section with the grid loaded.
fn notify_state() -> SettingsState {
    let mut s = state();
    s.set_notify_rules(notify_rules());
    // Navigate Daemon → … → Notifications (six `j`s reach + clamp at the bottom).
    for _ in 0..6 {
        s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    }
    assert_eq!(s.section(), SettingsSection::Notifications);
    s
}

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

/// `a` on the Daemon section flips the auto-standup toggle (D13): the local value
/// inverts AND a `ToggleAutostandup` intent carries the NEW value for the glue to
/// persist. A second `a` flips it back.
#[test]
fn a_on_daemon_section_toggles_autostandup_and_emits_intent() {
    let s = state(); // lands on the Daemon section, toggle defaults OFF
    assert!(!s.autostandup_enabled());
    let out = reduce_settings(&s, SettingsEvent::Key('a'));
    assert!(out.state.autostandup_enabled(), "optimistic flip to ON");
    assert_eq!(out.intent, Some(SettingsIntent::ToggleAutostandup(true)));
    // Toggling again flips back to OFF and emits the matching intent.
    let out2 = reduce_settings(&out.state, SettingsEvent::Key('a'));
    assert!(!out2.state.autostandup_enabled());
    assert_eq!(out2.intent, Some(SettingsIntent::ToggleAutostandup(false)));
}

/// The auto-standup toggle is Daemon-section-scoped: `a` on another section never
/// flips it (no accidental daemon-config write from the Keys/Workspaces panes).
#[test]
fn a_off_the_daemon_section_does_not_toggle_autostandup() {
    let mut s = state();
    s = reduce_settings(&s, SettingsEvent::Key('j')).state; // Providers
    let out = reduce_settings(&s, SettingsEvent::Key('a'));
    assert!(!out.state.autostandup_enabled());
    assert_eq!(out.intent, None);
}

/// A `hangar/daemon_config_get` snapshot sets the live toggle value the pane shows.
#[test]
fn set_autostandup_enabled_reflects_live_value() {
    let mut s = state();
    assert!(!s.autostandup_enabled());
    s.set_autostandup_enabled(true);
    assert!(s.autostandup_enabled());
}

/// j/k cycle through the six settings sections.
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
    let s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    assert_eq!(s.section(), SettingsSection::Notifications);
    // Clamps at the bottom (Notifications, tcp T5).
    let s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    assert_eq!(s.section(), SettingsSection::Notifications);
    let s = reduce_settings(&s, SettingsEvent::Key('k')).state;
    assert_eq!(s.section(), SettingsSection::Members);
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
    assert!(reduce_settings(&s, SettingsEvent::Key('s')).intent.is_none());
    assert!(reduce_settings(&s, SettingsEvent::Key('d')).intent.is_none());
    assert!(reduce_settings(&s, SettingsEvent::Key('r')).intent.is_none());
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

/// tcp T5: `J`/`K` move the kind row and `h`/`l` move the channel column on the
/// Notifications grid, both clamped to their bounds.
#[test]
fn notify_grid_cursor_navigates_kinds_and_channels() {
    let s = notify_state();
    assert_eq!(s.notify_cursor(), (0, 0));

    // J moves down the kinds; K back up; clamps at the ends.
    let s = reduce_settings(&s, SettingsEvent::Key('J')).state;
    assert_eq!(s.notify_cursor(), (1, 0));
    let s = reduce_settings(&s, SettingsEvent::Key('J')).state;
    assert_eq!(s.notify_cursor(), (2, 0));
    let s = reduce_settings(&s, SettingsEvent::Key('J')).state;
    assert_eq!(
        s.notify_cursor(),
        (2, 0),
        "kind cursor clamps at the last row"
    );

    // l moves right across the four channels; h back; clamps at 0..3.
    let s = reduce_settings(&s, SettingsEvent::Key('l')).state;
    assert_eq!(s.notify_cursor(), (2, 1));
    let s = reduce_settings(&s, SettingsEvent::Key('l')).state;
    let s = reduce_settings(&s, SettingsEvent::Key('l')).state;
    let s = reduce_settings(&s, SettingsEvent::Key('l')).state;
    assert_eq!(s.notify_cursor(), (2, 3), "channel cursor clamps at atc");
    let s = reduce_settings(&s, SettingsEvent::Key('h')).state;
    assert_eq!(s.notify_cursor(), (2, 2));
}

/// tcp T5: `space` toggles the selected cell and emits a `SetNotifyRule` intent
/// carrying the FULL new channel set for that kind; the local grid flips
/// optimistically.
#[test]
fn notify_grid_space_toggles_cell_and_emits_set_intent() {
    // On the ASK row (web+os), toggle the phone column (col 0) ON.
    let s = notify_state();
    let out = reduce_settings(&s, SettingsEvent::Key(' '));
    match out.intent {
        Some(SettingsIntent::SetNotifyRule { ref kind, channels }) => {
            assert_eq!(kind, "ask_user_question");
            assert!(channels.contains(Channel::Phone), "phone toggled on");
            assert!(channels.contains(Channel::Web), "web preserved");
            assert!(channels.contains(Channel::Os), "os preserved");
        }
        other => panic!("expected SetNotifyRule, got {other:?}"),
    }
    // The local grid reflects the toggle optimistically.
    assert!(out.state.notify_rules()[0].channels.contains(Channel::Phone));

    // Toggling the same cell again turns it back off (idempotent flip).
    let out2 = reduce_settings(&out.state, SettingsEvent::Key(' '));
    match out2.intent {
        Some(SettingsIntent::SetNotifyRule { channels, .. }) => {
            assert!(!channels.contains(Channel::Phone), "phone toggled back off");
        }
        other => panic!("expected SetNotifyRule, got {other:?}"),
    }
}

/// tcp T5: the Notifications keys are section-scoped — a `space` on the Daemon
/// section never emits a rule change, and `j`/`k` still leave the grid.
#[test]
fn notify_keys_are_section_scoped_and_j_k_still_navigate() {
    // space on Daemon does nothing.
    assert!(reduce_settings(&state(), SettingsEvent::Key(' ')).intent.is_none());
    // j/k move between sections even from the grid.
    let s = notify_state();
    let up = reduce_settings(&s, SettingsEvent::Key('k')).state;
    assert_eq!(up.section(), SettingsSection::Members);
    // A toggle with no rules loaded (empty grid) is a no-op, never a bogus intent.
    let mut empty = state();
    for _ in 0..6 {
        empty = reduce_settings(&empty, SettingsEvent::Key('j')).state;
    }
    assert!(reduce_settings(&empty, SettingsEvent::Key(' ')).intent.is_none());
}

/// agents-in-a-box-cqh: `g` flips the grid scope global⇄workspace, clears the
/// loaded rows (so a stale-scope cell can't be toggled before the re-list lands),
/// and emits a RefreshNotifyRules intent so the glue re-fetches the new scope. The
/// grid defaults to GLOBAL — the scope hook-raised attentions actually resolve, so
/// "what you edit is what applies" for the common ASK/error routing.
#[test]
fn notify_grid_g_toggles_scope_and_requests_refresh() {
    let s = notify_state();
    assert_eq!(
        s.notify_scope(),
        NotifyScope::Global,
        "grid defaults to global scope"
    );
    assert!(
        !s.notify_rules().is_empty(),
        "precondition: the grid is loaded"
    );

    let out = reduce_settings(&s, SettingsEvent::Key('g'));
    assert_eq!(
        out.state.notify_scope(),
        NotifyScope::Workspace,
        "g flips to workspace"
    );
    assert!(
        out.state.notify_rules().is_empty(),
        "rows cleared until the scoped re-list lands (no stale-scope toggle)"
    );
    assert_eq!(
        out.intent,
        Some(SettingsIntent::RefreshNotifyRules),
        "the flip requests a re-list for the new scope"
    );

    // Flipping back returns to global and re-requests a refresh.
    let back = reduce_settings(&out.state, SettingsEvent::Key('g'));
    assert_eq!(back.state.notify_scope(), NotifyScope::Global);
    assert_eq!(back.intent, Some(SettingsIntent::RefreshNotifyRules));
}

/// Scope-echo drop (agents-in-a-box-cqh): a `notify_rules_list` reply is applied
/// only when the scope it echoes still matches the grid's CURRENT edit scope. A
/// reply for the scope the grid already left (an in-flight old-scope reply landing
/// after a `g` toggle) is DROPPED, so it can't briefly repopulate the wrong rows.
#[test]
fn stale_scope_notify_reply_is_dropped() {
    use ainb_plugin_hangar::screen::settings::notify_reply_matches_scope;
    let ws = "ws-1";

    // Global scope answers the global (workspace_id=None) reply, drops a workspace one.
    assert!(notify_reply_matches_scope(NotifyScope::Global, ws, None));
    assert!(
        !notify_reply_matches_scope(NotifyScope::Global, ws, Some(ws)),
        "a workspace-scoped reply is stale once the grid is back on global"
    );

    // Workspace scope answers its own workspace reply, drops the global one.
    assert!(notify_reply_matches_scope(
        NotifyScope::Workspace,
        ws,
        Some(ws)
    ));
    assert!(
        !notify_reply_matches_scope(NotifyScope::Workspace, ws, None),
        "a global reply is stale once the grid flipped to workspace"
    );
    assert!(
        !notify_reply_matches_scope(NotifyScope::Workspace, ws, Some("other-ws")),
        "a different workspace's reply is stale"
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
