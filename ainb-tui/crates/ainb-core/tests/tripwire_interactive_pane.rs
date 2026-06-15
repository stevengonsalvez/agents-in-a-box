// ABOUTME: Capstone tripwire for the interactive in-place tmux pane (goal validation
// B5/B6/B8). Drives the REAL render path (TmuxPreviewPane::render_interactive) against a
// REAL tmux session via AppState::enter_interactive_pane, asserting on the rendered
// ratatui buffer — the user-visible output — rather than internal state alone.
//
// REAL tmux — creates + destroys its own named session (kill-session by exact name only,
// never kill-server/wildcard, per the tmux safety rule).

use std::process::Command;
use std::time::{Duration, Instant};

use ainb::app::events::{AppEvent, EventHandler};
use ainb::app::state::{AppState, FocusedPane, PREVIEW_EMBED_DEBOUNCE};
use ainb::components::{LayoutComponent, TmuxPreviewPane};
use ainb::models::OtherTmuxSession;
use ainb::tmux::{encode_key_event, encode_mouse_event};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn new_session(tag: &str) -> String {
    let name = format!("ainb-itw-{}-{}", tag, std::process::id());
    let _ = Command::new("tmux").args(["kill-session", "-t", &name]).output();
    let ok = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &name,
            "-x",
            "100",
            "-y",
            "26",
            "sh",
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "failed to create tmux session {name}");
    name
}

fn kill_session(name: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", name]).output();
}

fn session_alive(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn buffer_text(term: &Terminal<TestBackend>) -> String {
    term.backend().buffer().content().iter().map(|c| c.symbol()).collect()
}

#[test]
fn interactive_embed_renders_badge_and_live_input_then_release_keeps_session() {
    if !tmux_available() {
        eprintln!("SKIP: tmux unavailable");
        return;
    }

    let session = new_session("render");

    // Select the real tmux session as an "other tmux" row (the same resolution
    // path `a`/`l` use). selected_tmux_name() must resolve to it.
    let mut state = AppState::new();
    state.other_tmux_sessions = vec![OtherTmuxSession::new(session.clone(), false, 1)];
    state.selected_other_tmux_index = Some(0);
    assert_eq!(
        state.selected_tmux_name().as_deref(),
        Some(session.as_str()),
        "selection should resolve to the tmux session name"
    );

    // ── B5: 'A' (in-pane attach) enters → the live render shows the INTERACTIVE focus badge ──
    assert!(
        state.enter_interactive_pane(26, 100),
        "enter_interactive_pane should attach"
    );
    assert!(
        state.is_interactive_pane(),
        "should be interactive after enter"
    );

    let pane = TmuxPreviewPane::new();
    let mut term = Terminal::new(TestBackend::new(100, 26)).expect("test terminal");
    term.draw(|f| pane.render_interactive(f, f.area(), &state)).expect("draw");
    let badge_frame = buffer_text(&term);
    assert!(
        badge_frame.contains("INTERACTIVE"),
        "interactive focus badge not rendered:\n{badge_frame}"
    );

    // ── B6: typed input reaches the session and renders live in the pane ──
    state
        .embed
        .as_ref()
        .expect("embed")
        .write_input(b"printf 'TRIPWIRE_OK\\n'\n")
        .expect("write input");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut found = false;
    while Instant::now() < deadline {
        term.draw(|f| pane.render_interactive(f, f.area(), &state)).expect("draw");
        if buffer_text(&term).contains("TRIPWIRE_OK") {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let last_frame = buffer_text(&term);

    // ── B8: Ctrl+Q release reverts out of interactive AND the session survives ──
    state.release_interactive_pane();
    let released = !state.is_interactive_pane() && state.embed.is_none();
    let alive = session_alive(&session);

    kill_session(&session);

    assert!(
        found,
        "typed input never rendered live in the embed pane:\n{last_frame}"
    );
    assert!(
        released,
        "release_interactive_pane should drop focus + the embed client"
    );
    assert!(
        alive,
        "releasing the embed must NOT kill the tmux session (it survives)"
    );
}

/// B7 (amended 2026-06-12): the embed honors the user's sidebar instead of
/// forcing a collapse. With the default 40-col sidebar the embed gets the
/// pane next to it; pre-collapsing via `B` (the rail) hands it near-full
/// width. Driving the full LayoutComponent render resizes the embed to the
/// pane interior, so the embed's cell width reflects the user's layout.
#[test]
fn interactive_embed_width_follows_the_sidebar_state() {
    if !tmux_available() {
        eprintln!("SKIP: tmux unavailable");
        return;
    }
    let session = new_session("expand");
    let mut state = AppState::new();
    // session_list is a split-pane (non-registry) screen, so layout takes the
    // split path that renders the preview/embed pane.
    state.current_screen = "session_list".to_string();
    state.other_tmux_sessions = vec![OtherTmuxSession::new(session.clone(), false, 1)];
    state.selected_other_tmux_index = Some(0);
    // Pin the sidebar to a known width: AppState::new() restores the
    // developer's persisted preference from the real config, which would make
    // the expected interior widths env-dependent.
    state.sessions_pane_state.restore(Some(40), false);
    assert!(
        state.enter_interactive_pane(28, 80),
        "enter_interactive_pane"
    );

    let mut layout = LayoutComponent::new();
    let mut term = Terminal::new(TestBackend::new(120, 30)).expect("test terminal");

    // 40-col sidebar: the embed gets the remaining pane interior —
    // 120 − 40 − 2 (border) = 78 — NOT a forced near-full-width expansion.
    term.draw(|f| layout.render(f, &mut state)).expect("draw");
    let (_, cols_with_sidebar) = state.embed.as_ref().expect("embed").size();

    // Pre-collapsed rail (what `B` toggles): near-full width — 120 − 5 − 2.
    state.sessions_pane_state.collapsed = true;
    term.draw(|f| layout.render(f, &mut state)).expect("draw");
    let (_, cols_with_rail) = state.embed.as_ref().expect("embed").size();

    state.release_interactive_pane();
    kill_session(&session);

    assert_eq!(
        cols_with_sidebar, 78,
        "embed must honor the default 40-col sidebar (120 − 40 − 2)"
    );
    assert_eq!(
        cols_with_rail, 113,
        "embed must take near-full width once the sidebar is the collapsed rail (120 − 5 − 2)"
    );
}

/// Re-target: pressing the attach key on a DIFFERENT row while an embed is
/// live must release the stale client and attach to the new target — not
/// silently refocus the old one (which would render session X under a row
/// selecting session Y). Both sessions must survive the swap (release kills
/// clients, never sessions).
#[test]
fn reentering_on_a_different_row_retargets_the_embed() {
    if !tmux_available() {
        eprintln!("SKIP: tmux unavailable");
        return;
    }
    let first = new_session("retarget-a");
    let second = new_session("retarget-b");

    let mut state = AppState::new();
    state.current_screen = "session_list".to_string();
    state.other_tmux_sessions = vec![
        OtherTmuxSession::new(first.clone(), false, 1),
        OtherTmuxSession::new(second.clone(), false, 1),
    ];
    state.selected_other_tmux_index = Some(0);
    assert!(state.enter_interactive_pane(26, 100), "attach to first");
    let initial_target = state.embed_session.clone();

    // Same row again = self-healing no-op, embed target unchanged.
    assert!(state.enter_interactive_pane(26, 100), "same-row re-entry");
    let same_row_target = state.embed_session.clone();

    // Different row: must swap the embed onto the newly selected session.
    state.selected_other_tmux_index = Some(1);
    assert!(state.enter_interactive_pane(26, 100), "re-target to second");
    let swapped_target = state.embed_session.clone();
    let interactive_after = state.is_interactive_pane();

    state.release_interactive_pane();
    let both_alive = session_alive(&first) && session_alive(&second);

    kill_session(&first);
    kill_session(&second);

    assert_eq!(initial_target.as_deref(), Some(first.as_str()));
    assert_eq!(
        same_row_target.as_deref(),
        Some(first.as_str()),
        "same-row re-entry must not re-attach"
    );
    assert_eq!(
        swapped_target.as_deref(),
        Some(second.as_str()),
        "different-row re-entry must release the stale embed and attach to the selected session"
    );
    assert!(interactive_after, "still interactive after the swap");
    assert!(
        both_alive,
        "re-targeting kills only the ephemeral client — both tmux sessions survive"
    );
}

/// Mode-boundary tripwire: while the embed is interactive, host mouse handling
/// never runs (clicks/wheel don't break the mode), ':' reaches the PTY instead
/// of opening the slash palette, and after release the host owns the mouse
/// again. Drives the REAL state-level handlers (EventHandler::handle_mouse_event,
/// encode_key_event/encode_mouse_event + write_input — exactly what the event
/// loop calls) against a REAL tmux session.
#[test]
fn mode_boundary_holds_for_mouse_and_palette_keys_until_release() {
    if !tmux_available() {
        eprintln!("SKIP: tmux unavailable");
        return;
    }
    let session = new_session("boundary");
    let mut state = AppState::new();
    // session_list: the split-pane screen the embed lives on (poll_embed_exit
    // releases on any other screen) and the screen whose mouse handler owns
    // pane focus.
    state.current_screen = "session_list".to_string();
    state.other_tmux_sessions = vec![OtherTmuxSession::new(session.clone(), false, 1)];
    state.selected_other_tmux_index = Some(0);
    assert!(
        state.enter_interactive_pane(26, 100),
        "enter_interactive_pane"
    );

    // One full layout render publishes embed_pane_area + the sessions/preview
    // rects the mouse handler consults.
    let mut layout = LayoutComponent::new();
    let mut term = Terminal::new(TestBackend::new(120, 30)).expect("test terminal");
    term.draw(|f| layout.render(f, &mut state)).expect("draw");
    let inner = state
        .embed_pane_area
        .expect("interactive render must publish the embed pane interior");

    // A point inside the embed interior under the interactive layout AND
    // inside the preview pane under the normal layout (for the post-release
    // check) — middle of the right pane.
    let (px, py) = (80u16, 10u16);
    assert!(
        px > inner.x && px < inner.x + inner.width && py > inner.y && py < inner.y + inner.height,
        "test point must be inside the embed interior {inner:?}"
    );

    // ── (a) mouse click through the real state-level handler: swallowed ──
    let click = EventHandler::handle_mouse_event(AppEvent::MouseClick { x: px, y: py }, &mut state);
    let click_swallowed = click.is_none();
    let still_interactive_after_click = state.is_interactive_pane() && state.embed.is_some();

    // ── (b) ':' through the interactive key path reaches the PTY ──
    // Runs BEFORE the wheel check: a forwarded wheel-up legitimately puts
    // tmux into copy-mode (that's the scrollback feature), where ':' opens
    // the goto-line prompt instead of echoing in the shell.
    // The slash palette lives in main.rs's loop AFTER the interactive
    // intercept, so it can never see this key; here we pin the encode+write
    // path the intercept uses and that the byte lands in the live session.
    let marker = format!("TRIPWIRE_BOUNDARY_{}", std::process::id());
    let pre_frame = {
        term.draw(|f| layout.render(f, &mut state)).expect("draw");
        buffer_text(&term)
    };
    assert!(
        !pre_frame.contains(&marker),
        "negative placeholder: marker must not pre-exist in the pane"
    );
    let colon = KeyEvent {
        code: KeyCode::Char(':'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    let colon_bytes = encode_key_event(&colon).expect("':' must encode");
    assert_eq!(colon_bytes, b":".to_vec());
    let embed = state.embed.as_ref().expect("embed");
    embed.write_input(&colon_bytes).expect("write ':'");
    // `: <marker>` — the shell no-op builtin; the echoed input line carries
    // the marker into the rendered pane.
    embed.write_input(format!(" {marker}\n").as_bytes()).expect("write marker");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut colon_reached_pty = false;
    while Instant::now() < deadline {
        term.draw(|f| layout.render(f, &mut state)).expect("draw");
        if buffer_text(&term).contains(&marker) {
            colon_reached_pty = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let last_frame = buffer_text(&term);
    let still_interactive_after_colon = state.is_interactive_pane();

    // ── (c) wheel over the pane: encodes + forwards, mode still holds ──
    let wheel = MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: px,
        row: py,
        modifiers: KeyModifiers::NONE,
    };
    let wheel_bytes = encode_mouse_event(&wheel, inner).expect("wheel inside the pane must encode");
    state
        .embed
        .as_ref()
        .expect("embed")
        .write_input(&wheel_bytes)
        .expect("forward wheel");
    let still_interactive_after_wheel = state.is_interactive_pane();

    // ── (d) after release, host mouse handling works again ──
    state.release_interactive_pane();
    // Next frame re-lays-out the normal split; (80,10) sits in the preview
    // pane, so a click there must move focus to LiveLogs.
    term.draw(|f| layout.render(f, &mut state)).expect("draw");
    let _ = EventHandler::handle_mouse_event(AppEvent::MouseClick { x: px, y: py }, &mut state);
    let host_mouse_back = state.focused_pane == FocusedPane::LiveLogs;

    kill_session(&session);

    assert!(
        click_swallowed,
        "host mouse handler must not act while interactive"
    );
    assert!(
        still_interactive_after_click,
        "a click inside the pane must not break interactive mode"
    );
    assert!(
        still_interactive_after_wheel,
        "a wheel over the pane must not break interactive mode"
    );
    assert!(
        colon_reached_pty,
        "':' never reached the live session (palette boundary broken?):\n{last_frame}"
    );
    assert!(
        still_interactive_after_colon,
        "typing ':' must not break interactive mode"
    );
    assert!(
        host_mouse_back,
        "after release, a preview click must move focus to LiveLogs again"
    );
}

/// Helper: drive `sync_preview_embed` past its debounce so a READ-ONLY embed
/// attaches for the current selection. The first sync arms the debounce window;
/// the second (after it elapses) does the attach.
fn settle_readonly_embed(state: &mut AppState, rows: u16, cols: u16) {
    let _ = state.sync_preview_embed(rows, cols);
    std::thread::sleep(PREVIEW_EMBED_DEBOUNCE + Duration::from_millis(60));
    assert!(
        state.sync_preview_embed(rows, cols),
        "read-only preview embed should attach after the debounce window"
    );
}

/// Capstone for the EAGER read-only live preview (goal: exact-tmux-attach-preview).
/// Selecting a tmux row spins a READ-ONLY live mirror (byte-exact `tmux attach`,
/// not the lossy capture): it streams the session's live output, renders the LIVE
/// (not INTERACTIVE) badge, `A` ARMS the SAME client in place, and Ctrl+Q's
/// `disarm_interactive_pane` returns to the read-only mirror WITHOUT tearing the
/// client down — the session survives throughout.
#[test]
fn readonly_live_mirror_streams_then_arms_and_disarms_keeping_session() {
    if !tmux_available() {
        eprintln!("SKIP: tmux unavailable");
        return;
    }
    let session = new_session("ro-mirror");

    let mut state = AppState::new();
    state.current_screen = "session_list".to_string();
    state.other_tmux_sessions = vec![OtherTmuxSession::new(session.clone(), false, 1)];
    state.selected_other_tmux_index = Some(0);

    // ── eager attach: selecting the row spins a read-only (NOT armed) embed ──
    settle_readonly_embed(&mut state, 26, 100);
    let has_embed = state.has_preview_embed();
    let read_only = !state.is_interactive_pane();
    let mirrors_session = state.embed_session.as_deref() == Some(session.as_str());

    let pane = TmuxPreviewPane::new();
    let mut term = Terminal::new(TestBackend::new(100, 26)).expect("test terminal");

    // ── live proof: output injected into the SESSION must stream into the
    //    read-only mirror (a capture render could not show a post-attach line) ──
    let marker = format!("RO_LIVE_{}", std::process::id());
    let pre_frame = {
        term.draw(|f| pane.render_live_readonly(f, f.area(), &state)).expect("draw");
        buffer_text(&term)
    };
    assert!(
        !pre_frame.contains(&marker),
        "negative placeholder: marker must not pre-exist:\n{pre_frame}"
    );
    let _ = Command::new("tmux")
        .args([
            "send-keys",
            "-t",
            &session,
            &format!("printf '{marker}\\n'"),
            "Enter",
        ])
        .status();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut streamed = false;
    let mut live_frame = String::new();
    while Instant::now() < deadline {
        term.draw(|f| pane.render_live_readonly(f, f.area(), &state)).expect("draw");
        live_frame = buffer_text(&term);
        if live_frame.contains(&marker) {
            streamed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let shows_live_badge = live_frame.contains("LIVE") && !live_frame.contains("INTERACTIVE");

    // ── A arms the SAME client in place (read-only -> interactive) ──
    let session_before_arm = state.embed_session.clone();
    let armed = state.enter_interactive_pane(26, 100);
    let armed_same_client = state.embed_session == session_before_arm;
    term.draw(|f| pane.render_interactive(f, f.area(), &state)).expect("draw");
    let interactive_badge = buffer_text(&term).contains("INTERACTIVE");

    // ── Ctrl+Q -> disarm: back to read-only mirror, client KEPT ──
    state.disarm_interactive_pane();
    let back_to_readonly = !state.is_interactive_pane() && state.has_preview_embed();
    term.draw(|f| pane.render_live_readonly(f, f.area(), &state)).expect("draw");
    let live_badge_again = buffer_text(&term).contains("LIVE");

    let alive_before_release = session_alive(&session);
    state.release_interactive_pane();
    let alive_after_release = session_alive(&session);
    kill_session(&session);

    assert!(
        has_embed,
        "selecting a tmux row must spin a read-only embed"
    );
    assert!(
        read_only,
        "eager preview embed must NOT be armed (read-only)"
    );
    assert!(mirrors_session, "embed must mirror the selected session");
    assert!(
        streamed,
        "read-only mirror never streamed live session output:\n{live_frame}"
    );
    assert!(
        shows_live_badge,
        "read-only mirror must show LIVE (not INTERACTIVE):\n{live_frame}"
    );
    assert!(armed, "A must arm the existing read-only embed");
    assert!(
        armed_same_client,
        "A must arm in place, not re-attach a new client"
    );
    assert!(
        interactive_badge,
        "armed pane must show the INTERACTIVE badge"
    );
    assert!(
        back_to_readonly,
        "Ctrl+Q must disarm to a kept read-only embed, not release it"
    );
    assert!(
        live_badge_again,
        "after disarm the pane must render the read-only LIVE mirror again"
    );
    assert!(
        alive_before_release,
        "session must be alive while previewed"
    );
    assert!(
        alive_after_release,
        "releasing the embed must NOT kill the session"
    );
}

/// Moving the selection to a DIFFERENT tmux row tears down the old read-only
/// client immediately (so the wrong session is never shown live), then attaches
/// a fresh one for the new selection after the debounce — and BOTH sessions
/// survive (teardown kills the ephemeral client, never the session).
#[test]
fn selection_change_tears_down_readonly_embed_and_both_sessions_survive() {
    if !tmux_available() {
        eprintln!("SKIP: tmux unavailable");
        return;
    }
    let first = new_session("ro-sel-a");
    let second = new_session("ro-sel-b");

    let mut state = AppState::new();
    state.current_screen = "session_list".to_string();
    state.other_tmux_sessions = vec![
        OtherTmuxSession::new(first.clone(), false, 1),
        OtherTmuxSession::new(second.clone(), false, 1),
    ];
    state.selected_other_tmux_index = Some(0);

    settle_readonly_embed(&mut state, 26, 100);
    let first_target = state.embed_session.clone();

    // Move selection to the second row. The first sync detects the change and
    // drops the stale client immediately (returns true = layout changed).
    state.selected_other_tmux_index = Some(1);
    let changed = state.sync_preview_embed(26, 100);
    let torn_down_immediately = !state.has_preview_embed();

    // After the debounce, a fresh read-only embed attaches for the second row.
    std::thread::sleep(PREVIEW_EMBED_DEBOUNCE + Duration::from_millis(60));
    let reattached = state.sync_preview_embed(26, 100);
    let second_target = state.embed_session.clone();

    state.release_interactive_pane();
    let both_alive = session_alive(&first) && session_alive(&second);
    kill_session(&first);
    kill_session(&second);

    assert_eq!(
        first_target.as_deref(),
        Some(first.as_str()),
        "first mirror"
    );
    assert!(changed, "selection change must report a layout change");
    assert!(
        torn_down_immediately,
        "stale read-only client must drop on selection change"
    );
    assert!(
        reattached,
        "a fresh read-only embed must attach for the new selection"
    );
    assert_eq!(
        second_target.as_deref(),
        Some(second.as_str()),
        "re-targeted mirror"
    );
    assert!(
        both_alive,
        "selection-change teardown must NOT kill either session"
    );
}

/// Additive invariant: while the preview embed is READ-ONLY (not armed), host
/// input is unchanged — a preview-pane click is handled by the host (moves
/// focus) exactly as it would WITHOUT an embed, NOT swallowed the way the armed
/// interactive pane swallows it. Proves the eager mirror doesn't regress mouse.
#[test]
fn readonly_preview_does_not_swallow_host_mouse() {
    if !tmux_available() {
        eprintln!("SKIP: tmux unavailable");
        return;
    }
    let session = new_session("ro-mouse");
    let mut state = AppState::new();
    state.current_screen = "session_list".to_string();
    state.other_tmux_sessions = vec![OtherTmuxSession::new(session.clone(), false, 1)];
    state.selected_other_tmux_index = Some(0);
    state.sessions_pane_state.restore(Some(40), false);

    settle_readonly_embed(&mut state, 28, 80);
    let read_only = !state.is_interactive_pane() && state.has_preview_embed();

    // Lay out the normal split (read-only branch), then click the preview pane.
    let mut layout = LayoutComponent::new();
    let mut term = Terminal::new(TestBackend::new(120, 30)).expect("test terminal");
    term.draw(|f| layout.render(f, &mut state)).expect("draw");
    let _ = EventHandler::handle_mouse_event(AppEvent::MouseClick { x: 80, y: 10 }, &mut state);
    let host_handled_mouse = state.focused_pane == FocusedPane::LiveLogs;

    state.release_interactive_pane();
    let alive = session_alive(&session);
    kill_session(&session);

    assert!(read_only, "embed must be read-only for this invariant");
    assert!(
        host_handled_mouse,
        "a read-only preview must NOT swallow the mouse — host focus handling must still run"
    );
    assert!(alive, "session survives");
}
