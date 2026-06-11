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
use ainb::app::state::{AppState, FocusedPane};
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

    // ── B5: 'l' enters → the live render shows the INTERACTIVE focus badge ──
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

/// B7: while interactive, the session list collapses and the embed expands to
/// near-full width. Driving the full LayoutComponent render resizes the embed to
/// the pane interior, so the embed's cell width reflects the expanded pane.
#[test]
fn interactive_embed_expands_to_near_full_width() {
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
    assert!(
        state.enter_interactive_pane(28, 80),
        "enter_interactive_pane"
    );

    let mut layout = LayoutComponent::new();
    let mut term = Terminal::new(TestBackend::new(120, 30)).expect("test terminal");
    // Render the full layout — the interactive branch collapses the list to a rail
    // and resizes the embed to the (now near-full-width) preview pane.
    term.draw(|f| layout.render(f, &mut state)).expect("draw");

    let (_, cols) = state.embed.as_ref().expect("embed").size();
    state.release_interactive_pane();
    kill_session(&session);

    // In a 120-col terminal the read-only split gives the preview ~60% (~70 cols);
    // expansion (list collapsed to a 5-col rail) should give it >100.
    assert!(
        cols > 100,
        "interactive pane should expand to near-full width in a 120-col terminal; got {cols}"
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
