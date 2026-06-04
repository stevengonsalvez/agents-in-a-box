// ABOUTME: Capstone tripwire for the interactive in-place tmux pane (goal validation
// B5/B6/B8). Drives the REAL render path (TmuxPreviewPane::render_interactive) against a
// REAL tmux session via AppState::enter_interactive_pane, asserting on the rendered
// ratatui buffer — the user-visible output — rather than internal state alone.
//
// REAL tmux — creates + destroys its own named session (kill-session by exact name only,
// never kill-server/wildcard, per the tmux safety rule).

use std::process::Command;
use std::time::{Duration, Instant};

use ainb::app::state::AppState;
use ainb::components::TmuxPreviewPane;
use ainb::models::OtherTmuxSession;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().map(|o| o.status.success()).unwrap_or(false)
}

fn new_session(tag: &str) -> String {
    let name = format!("ainb-itw-{}-{}", tag, std::process::id());
    let _ = Command::new("tmux").args(["kill-session", "-t", &name]).output();
    let ok = Command::new("tmux")
        .args(["new-session", "-d", "-s", &name, "-x", "100", "-y", "26", "sh"])
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
    // path `a`/`i` use). selected_tmux_name() must resolve to it.
    let mut state = AppState::new();
    state.other_tmux_sessions = vec![OtherTmuxSession::new(session.clone(), false, 1)];
    state.selected_other_tmux_index = Some(0);
    assert_eq!(
        state.selected_tmux_name().as_deref(),
        Some(session.as_str()),
        "selection should resolve to the tmux session name"
    );

    // ── B5: 'i' enters → the live render shows the INTERACTIVE focus badge ──
    assert!(state.enter_interactive_pane(26, 100), "enter_interactive_pane should attach");
    assert!(state.is_interactive_pane(), "should be interactive after enter");

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

    assert!(found, "typed input never rendered live in the embed pane:\n{last_frame}");
    assert!(released, "release_interactive_pane should drop focus + the embed client");
    assert!(alive, "releasing the embed must NOT kill the tmux session (it survives)");
}
