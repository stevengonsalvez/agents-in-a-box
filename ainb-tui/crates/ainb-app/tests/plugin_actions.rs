// ABOUTME: A renderer runs a plugin's own action through `dispatch` and reads
// what it changed from the plugin's `ui.state` view, kept per plugin in the
// plugins-host section.

#[path = "support/home.rs"]
mod home;

use ainb_app::app::NoRenderer;
use ainb_app::app::plugin_action::{self, ids};
use ainb_app::wire::frame::HostId;
use ainb_app::{AppState, CommandId, Intent, Keymap, SectionId, dispatch};
use ainb_plugin_runtime::types::PluginId;

fn bumped(before: &[u64], after: &[u64]) -> Vec<SectionId> {
    SectionId::ALL
        .into_iter()
        .filter(|id| before[id.index()] != after[id.index()])
        .collect()
}

fn desktop() -> HostId {
    HostId::new("desktop")
}

fn phone() -> HostId {
    HostId::new("phone")
}

fn isolated_home() {
    home::shared();
}

#[test]
fn the_plugin_action_is_an_unbound_row_that_refuses_to_run_bare() {
    isolated_home();
    let keymap = Keymap::defaults();
    let row = keymap.command(&CommandId::new(ids::PLUGIN_ACTION)).expect("the row resolves");
    assert!(row.chord.is_none());

    let mut state = AppState::new();
    let before = state.versions();
    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        Intent::Command(CommandId::new(ids::PLUGIN_ACTION), serde_json::Value::Null),
    );
    assert!(effects.is_empty());
    assert!(bumped(&before, &state.versions()).is_empty());
}

/// The reducer never touches the plugin runtime: it hands the action to
/// the host as an effect, and whether a plugin was running to take it comes
/// back as a report.
#[test]
fn a_plugin_action_is_an_effect_for_the_host_not_a_runtime_call() {
    isolated_home();
    let mut state = AppState::new();
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &Keymap::defaults(),
        &mut NoRenderer,
        plugin_action::run(
            "hangar-tui",
            "board.open_card",
            serde_json::json!({ "id": "card-7" }),
        ),
    );

    assert_eq!(
        effects,
        vec![ainb_app::Effect::RunPluginAction {
            plugin: "hangar-tui".to_string(),
            action_id: "board.open_card".to_string(),
            payload: serde_json::json!({ "id": "card-7" }),
        }]
    );
    assert!(
        bumped(&before, &state.versions()).is_empty(),
        "queuing the action writes no section"
    );
    assert!(state.shell.notifications.is_empty());
}

#[test]
fn an_action_for_a_plugin_no_screen_owns_is_refused() {
    isolated_home();
    let mut state = AppState::new();
    let _ = dispatch(
        &mut state,
        &Keymap::defaults(),
        &mut NoRenderer,
        plugin_action::run("not-a-plugin", "anything", serde_json::Value::Null),
    );
    assert_eq!(state.shell.notifications.len(), 1);
}

fn publish(view: &str, version: u64, publisher: &str) -> Option<(bytes::Bytes, u64, PluginId)> {
    Some((
        bytes::Bytes::from(view.to_string()),
        version,
        PluginId::new(publisher),
    ))
}

#[test]
fn a_newer_ui_state_is_kept_per_plugin_and_bumps_only_the_plugins_host_section() {
    isolated_home();
    let mut state = AppState::new();

    let before = state.versions();
    state.record_plugin_ui_state(
        "hangar-tui",
        true,
        publish(r#"{"screen":"kanban"}"#, 3, "hangar-tui"),
    );
    assert_eq!(
        bumped(&before, &state.versions()),
        vec![SectionId::PluginsHost]
    );
    let kept = &state.plugins_host.plugin_ui_states["hangar-tui"];
    assert_eq!(kept.version, 3);
    assert_eq!(kept.view["screen"], "kanban");

    // The same publish read on the next tick changes nothing.
    let before = state.versions();
    state.record_plugin_ui_state(
        "hangar-tui",
        true,
        publish(r#"{"screen":"kanban"}"#, 3, "hangar-tui"),
    );
    state.record_plugin_ui_state("hangar-tui", true, None);
    assert!(bumped(&before, &state.versions()).is_empty());

    state.record_plugin_ui_state(
        "hangar-tui",
        true,
        publish(r#"{"screen":"issues"}"#, 4, "hangar-tui"),
    );
    assert_eq!(
        state.plugins_host.plugin_ui_states["hangar-tui"].view["screen"],
        "issues"
    );
}

#[test]
fn two_plugins_keep_their_own_views() {
    isolated_home();
    let mut state = AppState::new();

    state.record_plugin_ui_state(
        "hangar-tui",
        true,
        publish(r#"{"v":"hangar"}"#, 1, "hangar-tui"),
    );
    state.record_plugin_ui_state(
        "learnings",
        true,
        publish(r#"{"v":"learnings"}"#, 1, "learnings"),
    );

    assert_eq!(
        state.plugins_host.plugin_ui_states["hangar-tui"].view["v"],
        "hangar"
    );
    assert_eq!(
        state.plugins_host.plugin_ui_states["learnings"].view["v"],
        "learnings"
    );
}

#[test]
fn a_view_from_another_publisher_or_not_json_is_not_kept() {
    isolated_home();
    let mut state = AppState::new();
    let before = state.versions();

    state.record_plugin_ui_state(
        "hangar-tui",
        true,
        publish(r#"{"screen":"kanban"}"#, 1, "host"),
    );
    state.record_plugin_ui_state("hangar-tui", true, publish("not json", 2, "hangar-tui"));

    assert!(state.plugins_host.plugin_ui_states.is_empty());
    assert!(bumped(&before, &state.versions()).is_empty());
}

#[test]
fn a_stopped_plugin_loses_its_view() {
    isolated_home();
    let mut state = AppState::new();
    state.record_plugin_ui_state("hangar-tui", true, publish(r#"{"v":1}"#, 1, "hangar-tui"));

    state.record_plugin_ui_state("hangar-tui", false, publish(r#"{"v":1}"#, 1, "hangar-tui"));

    assert!(!state.plugins_host.plugin_ui_states.contains_key("hangar-tui"));
    // And a plugin that never had one bumps nothing when it is not running.
    let before = state.versions();
    state.record_plugin_ui_state("learnings", false, None);
    assert!(bumped(&before, &state.versions()).is_empty());
}

#[test]
fn a_view_over_the_size_cap_is_refused_and_drops_the_old_one() {
    isolated_home();
    let mut state = AppState::new();
    state.record_plugin_ui_state("hangar-tui", true, publish(r#"{"v":1}"#, 1, "hangar-tui"));
    let huge = format!(
        r#"{{"blob":"{}"}}"#,
        "x".repeat(ainb_app::app::state::MAX_PLUGIN_UI_STATE_BYTES)
    );

    state.record_plugin_ui_state("hangar-tui", true, publish(&huge, 2, "hangar-tui"));

    assert!(!state.plugins_host.plugin_ui_states.contains_key("hangar-tui"));
}

/// A plugin screen stays rendering while some host wants it: the terminal
/// showing it, or another host's watch. Without either, it stops being kicked.
#[test]
fn a_watched_plugin_screen_stays_wanted_while_the_terminal_shows_another() {
    use ainb_app::app::screens::ids as screen_ids;

    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    assert!(!state.plugin_screen_wanted(screen_ids::HANGAR));
    assert!(!state.plugin_screen_wanted(screen_ids::LEARNINGS));

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        plugin_action::watch_screen(screen_ids::HANGAR, &desktop(), true, 120, 40),
    );
    assert!(
        state.plugin_screen_wanted(screen_ids::HANGAR),
        "watched from another host"
    );
    assert!(
        !state.plugin_screen_wanted(screen_ids::LEARNINGS),
        "nobody wants learnings"
    );

    // The only watcher stopping ends the watch at once.
    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        plugin_action::watch_screen(screen_ids::HANGAR, &desktop(), false, 0, 0),
    );
    assert!(!state.plugin_screen_wanted(screen_ids::HANGAR));

    // A screen no plugin owns is not watchable.
    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        plugin_action::watch_screen(screen_ids::CONFIG, &desktop(), true, 120, 40),
    );
    assert!(state.plugins_host.watched_plugin_screens.is_empty());
}

/// Publish `view` on `plugin`'s own `ui.state` topic in a real snapshot store,
/// the way the runtime does for the plugin's process.
fn publish_to(store: &ainb_plugin_runtime::snapshot::SnapshotStore, plugin: &str, view: &str) {
    let topic = ainb_plugin_runtime::types::Topic::from(
        ainb_plugin_runtime::topics::ui_state_topic(plugin),
    );
    let _ = store.publish(
        topic,
        bytes::Bytes::from(view.to_string()),
        PluginId::new(plugin),
    );
}

fn read_from(
    store: &ainb_plugin_runtime::snapshot::SnapshotStore,
    plugin: &str,
) -> Option<(bytes::Bytes, u64, PluginId)> {
    store.get(&ainb_plugin_runtime::types::Topic::from(
        ainb_plugin_runtime::topics::ui_state_topic(plugin),
    ))
}

/// A crash and restart: until the new process publishes, the store may still
/// hold the dead one's view, and the host must not show it as live.
#[test]
fn a_restarted_plugin_does_not_show_the_view_its_last_process_published() {
    isolated_home();
    let store = ainb_plugin_runtime::snapshot::SnapshotStore::new();
    let mut state = AppState::new();

    publish_to(&store, "hangar-tui", r#"{"screen":"before the crash"}"#);
    state.record_plugin_ui_state("hangar-tui", true, read_from(&store, "hangar-tui"));
    assert!(state.plugins_host.plugin_ui_states.contains_key("hangar-tui"));

    state.record_plugin_ui_state("hangar-tui", false, read_from(&store, "hangar-tui"));
    state.record_plugin_ui_state("hangar-tui", true, read_from(&store, "hangar-tui"));
    assert!(
        !state.plugins_host.plugin_ui_states.contains_key("hangar-tui"),
        "the restarted plugin has not published; the old view is not its view"
    );

    publish_to(&store, "hangar-tui", r#"{"screen":"after the restart"}"#);
    state.record_plugin_ui_state("hangar-tui", true, read_from(&store, "hangar-tui"));
    assert_eq!(
        state.plugins_host.plugin_ui_states["hangar-tui"].view["screen"],
        "after the restart"
    );
}

/// A refused publish stays refused while it is the newest, so the tick does
/// not re-read and re-log it, and the next good publish is still taken.
#[test]
fn a_refused_view_is_not_reconsidered_until_the_plugin_publishes_again() {
    isolated_home();
    let store = ainb_plugin_runtime::snapshot::SnapshotStore::new();
    let mut state = AppState::new();

    publish_to(&store, "hangar-tui", "not json");
    let before = state.versions();
    for _ in 0..3 {
        state.record_plugin_ui_state("hangar-tui", true, read_from(&store, "hangar-tui"));
    }
    assert!(state.plugins_host.plugin_ui_states.is_empty());
    assert!(bumped(&before, &state.versions()).is_empty());

    publish_to(&store, "hangar-tui", r#"{"screen":"kanban"}"#);
    state.record_plugin_ui_state("hangar-tui", true, read_from(&store, "hangar-tui"));
    assert_eq!(
        state.plugins_host.plugin_ui_states["hangar-tui"].view["screen"],
        "kanban"
    );
}

/// A watch is a lease: a host that stops renewing it, or a plugin that is
/// gone for good, stops the screen being kept live.
#[test]
fn a_screen_watch_lapses_unless_renewed_and_goes_with_its_plugin() {
    use ainb_app::app::screens::ids as screen_ids;
    use std::time::{Duration, Instant};

    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let watch = |state: &mut AppState| {
        let _ = dispatch(
            state,
            &keymap,
            &mut NoRenderer,
            plugin_action::watch_screen(screen_ids::HANGAR, &desktop(), true, 120, 40),
        );
    };
    let lease = AppState::PLUGIN_SCREEN_WATCH_LEASE;

    watch(&mut state);
    state.release_plugin_screen_watches(Instant::now() + lease / 2, |_| false);
    assert!(
        state.plugin_screen_wanted(screen_ids::HANGAR),
        "within its lease"
    );
    state.release_plugin_screen_watches(Instant::now() + lease + Duration::from_secs(1), |_| false);
    assert!(
        !state.plugin_screen_wanted(screen_ids::HANGAR),
        "not renewed"
    );

    watch(&mut state);
    state.release_plugin_screen_watches(Instant::now(), |plugin| plugin == "hangar-tui");
    assert!(
        !state.plugin_screen_wanted(screen_ids::HANGAR),
        "its plugin is gone"
    );
}

/// Hosts watching one screen at different sizes get one render at the largest
/// width and the largest height any live request asked for; a request with no
/// viewport is refused, and a lapsed request no longer counts.
#[test]
fn a_watched_screen_renders_at_the_largest_size_a_live_watch_asked_for() {
    use ainb_app::app::screens::ids as screen_ids;
    use std::time::{Duration, Instant};

    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let mut watch = |state: &mut AppState, host: HostId, width, height| {
        let _ = dispatch(
            state,
            &keymap,
            &mut NoRenderer,
            plugin_action::watch_screen(screen_ids::HANGAR, &host, true, width, height),
        );
    };

    watch(&mut state, desktop(), 0, 24);
    assert!(
        !state.plugin_screen_wanted(screen_ids::HANGAR),
        "no viewport"
    );
    assert_eq!(state.watched_viewport(screen_ids::HANGAR), None);

    watch(&mut state, desktop(), 200, 30);
    let wide_at = Instant::now();
    std::thread::sleep(Duration::from_millis(20));
    watch(&mut state, phone(), 90, 60);
    assert_eq!(state.watched_viewport(screen_ids::HANGAR), Some((200, 60)));

    // The wide request lapses first; the tall one alone sets the size.
    let lease = AppState::PLUGIN_SCREEN_WATCH_LEASE;
    state.release_plugin_screen_watches(wide_at + lease + Duration::from_millis(10), |_| false);
    assert_eq!(state.watched_viewport(screen_ids::HANGAR), Some((90, 60)));
}

/// Two hosts watch one screen at different sizes. One stopping, or one
/// renewing far more often than the other, leaves the other's request live,
/// and a host's smaller size replaces its larger one at once (#1046).
#[test]
fn one_host_stopping_or_resizing_leaves_another_hosts_watch_alone() {
    use ainb_app::app::screens::ids as screen_ids;

    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let mut send = |state: &mut AppState, host: HostId, watching, width, height| {
        let _ = dispatch(
            state,
            &keymap,
            &mut NoRenderer,
            plugin_action::watch_screen(screen_ids::HANGAR, &host, watching, width, height),
        );
    };

    send(&mut state, desktop(), true, 200, 60);
    for _ in 0..17 {
        send(&mut state, phone(), true, 90, 30);
    }
    assert_eq!(
        state.plugins_host.watched_plugin_screens[screen_ids::HANGAR].requests.len(),
        2,
        "one request per host"
    );
    assert_eq!(state.watched_viewport(screen_ids::HANGAR), Some((200, 60)));

    send(&mut state, phone(), false, 0, 0);
    assert_eq!(
        state.watched_viewport(screen_ids::HANGAR),
        Some((200, 60)),
        "the phone stopping leaves the desktop's watch"
    );
    send(&mut state, desktop(), true, 120, 40);
    assert_eq!(
        state.watched_viewport(screen_ids::HANGAR),
        Some((120, 40)),
        "the desktop's smaller size replaces its larger one at once"
    );
    send(&mut state, phone(), true, 90, 30);
    send(&mut state, desktop(), false, 0, 0);
    assert_eq!(state.watched_viewport(screen_ids::HANGAR), Some((90, 30)));
    send(&mut state, phone(), false, 0, 0);
    assert!(!state.plugin_screen_wanted(screen_ids::HANGAR));
}

/// A host that disconnects drops its requests on every screen at once, and
/// another host's watch on the same screen stays (#1046).
#[test]
fn a_disconnected_host_drops_its_watches_without_waiting_for_the_lease() {
    use ainb_app::app::screens::ids as screen_ids;

    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    for (screen, host) in [
        (screen_ids::HANGAR, desktop()),
        (screen_ids::LEARNINGS, desktop()),
        (screen_ids::HANGAR, phone()),
    ] {
        let _ = dispatch(
            &mut state,
            &keymap,
            &mut NoRenderer,
            plugin_action::watch_screen(screen, &host, true, 120, 40),
        );
    }

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        ainb_app::app::reports::host_disconnected(&desktop()),
    );

    assert!(!state.plugin_screen_wanted(screen_ids::LEARNINGS));
    assert!(
        state.plugin_screen_wanted(screen_ids::HANGAR),
        "the phone still watches"
    );
    assert_eq!(
        state.plugins_host.watched_plugin_screens[screen_ids::HANGAR]
            .requests
            .keys()
            .collect::<Vec<_>>(),
        vec![&phone()]
    );
}

/// Past the host cap, the least recently renewed host's request goes.
#[test]
fn the_host_cap_drops_the_stalest_request() {
    use ainb_app::app::screens::ids as screen_ids;

    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    for n in 0..17 {
        let _ = dispatch(
            &mut state,
            &keymap,
            &mut NoRenderer,
            plugin_action::watch_screen(
                screen_ids::HANGAR,
                &HostId::new(format!("host-{n}")),
                true,
                120,
                40,
            ),
        );
    }
    let requests = &state.plugins_host.watched_plugin_screens[screen_ids::HANGAR].requests;
    assert_eq!(requests.len(), 16);
    assert!(!requests.contains_key(&HostId::new("host-0")));
    assert!(requests.contains_key(&HostId::new("host-16")));
}

/// A watch cannot ask a plugin to render past the viewport ceiling, and a
/// watch or stop that names no host is refused.
#[test]
fn a_watch_is_clamped_to_the_viewport_ceiling_and_a_hostless_stop_is_refused() {
    use ainb_app::app::screens::ids as screen_ids;
    use ainb_app::app::sections::ScreenWatch;

    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        plugin_action::watch_screen(screen_ids::HANGAR, &desktop(), true, u16::MAX, u16::MAX),
    );
    assert_eq!(
        state.watched_viewport(screen_ids::HANGAR),
        Some(ScreenWatch::MAX_VIEWPORT)
    );

    let hostless_stop = Intent::Command(
        CommandId::new(ids::WATCH_SCREEN),
        serde_json::json!({ "screen": screen_ids::HANGAR, "watching": false }),
    );
    let before = state.versions();
    let effects = dispatch(&mut state, &keymap, &mut NoRenderer, hostless_stop);
    assert!(effects.is_empty());
    assert!(state.plugin_screen_wanted(screen_ids::HANGAR));
    assert_eq!(state.versions(), before);

    let hostless_watch = Intent::Command(
        CommandId::new(ids::WATCH_SCREEN),
        serde_json::json!({
            "screen": screen_ids::LEARNINGS,
            "watching": true,
            "width": 120,
            "height": 40,
        }),
    );
    let effects = dispatch(&mut state, &keymap, &mut NoRenderer, hostless_watch);
    assert!(effects.is_empty());
    assert!(!state.plugin_screen_wanted(screen_ids::LEARNINGS));
    assert_eq!(state.versions(), before);
}

/// A renewal with no viewport is that host's stop, and a renewal that moves
/// the render size moves the section.
#[test]
fn a_zero_size_renewal_stops_and_a_resize_moves_the_section() {
    use ainb_app::app::screens::ids as screen_ids;

    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.shell.current_screen = screen_ids::SESSION_LIST.to_string();
    let mut watch = |state: &mut AppState, host: HostId, width, height| {
        let _ = dispatch(
            state,
            &keymap,
            &mut NoRenderer,
            plugin_action::watch_screen(screen_ids::HANGAR, &host, true, width, height),
        );
    };

    watch(&mut state, desktop(), 120, 40);
    watch(&mut state, phone(), 90, 30);
    let before = state.versions();
    watch(&mut state, desktop(), 120, 40);
    assert!(
        bumped(&before, &state.versions()).is_empty(),
        "a renewal at the same size moves nothing a frame carries"
    );
    watch(&mut state, desktop(), 160, 50);
    assert_eq!(
        bumped(&before, &state.versions()),
        vec![SectionId::PluginsHost],
        "a renewal that moves the render size moves the section"
    );

    watch(&mut state, desktop(), 0, 50);
    assert_eq!(
        state.watched_viewport(screen_ids::HANGAR),
        Some((90, 30)),
        "the desktop's zero-width renewal is its stop; the phone's watch stays"
    );
}
