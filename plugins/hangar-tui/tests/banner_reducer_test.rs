//! P4.8 RED — frosted progress-banner reducer + cross-screen async UX.
//!
//! The active-task banner is pinned across every screen: it appears on
//! `TaskStarted`, ticks its elapsed clock at 1Hz, hides on `TaskFinished`, and
//! its capital-`X` cancel propagates from *any* screen. These tests pin the pure
//! banner reducer + the render's truncate-message-not-glyphs width behaviour.

use ainb_hangar_core::ids::{AgentId, IssueId, TaskId};
use ainb_hangar_proto::events::{HangarEvent, MessageKind, TaskResult};
use ainb_plugin_hangar::screen::banner_state::{
    reduce_banner, BannerEvent, BannerIntent, BannerState,
};
use ainb_plugin_sdk::WireBuffer;
use chrono::{TimeZone, Utc};

fn task() -> TaskId {
    TaskId::from_str("t1").unwrap()
}

fn started() -> HangarEvent {
    HangarEvent::TaskStarted {
        task_id: task(),
        started_at: Utc.timestamp_opt(0, 0).unwrap(),
    }
}

fn queued() -> HangarEvent {
    HangarEvent::TaskQueued {
        task_id: task(),
        issue_id: IssueId::from_str("i1").unwrap(),
        agent_id: AgentId::from_str("claude-agent").unwrap(),
    }
}

/// `TaskStarted` makes the banner appear.
#[test]
fn banner_appears_on_task_started_event() {
    let s = BannerState::default();
    assert!(s.banner().is_none());
    // Queue first so the banner knows the agent label.
    let s = reduce_banner(&s, BannerEvent::Event(queued())).state;
    let s = reduce_banner(&s, BannerEvent::Event(started())).state;
    assert!(s.banner().is_some());
    assert_eq!(s.banner().unwrap().agent_label, "claude-agent");
}

/// The elapsed clock increments on each 1Hz tick.
#[test]
fn banner_elapsed_increments_on_tick() {
    let mut s = BannerState::default();
    s = reduce_banner(&s, BannerEvent::Event(queued())).state;
    s = reduce_banner(&s, BannerEvent::Event(started())).state;
    let e0 = s.banner().unwrap().elapsed_secs;
    s = reduce_banner(&s, BannerEvent::Tick).state;
    s = reduce_banner(&s, BannerEvent::Tick).state;
    assert_eq!(s.banner().unwrap().elapsed_secs, e0 + 2);
}

/// `TaskFinished` hides the banner.
#[test]
fn banner_hides_on_task_finished_event() {
    let mut s = BannerState::default();
    s = reduce_banner(&s, BannerEvent::Event(queued())).state;
    s = reduce_banner(&s, BannerEvent::Event(started())).state;
    assert!(s.banner().is_some());
    s = reduce_banner(
        &s,
        BannerEvent::Event(HangarEvent::TaskFinished {
            task_id: task(),
            result: TaskResult::Success,
            ended_at: Utc.timestamp_opt(10, 0).unwrap(),
        }),
    )
    .state;
    assert!(s.banner().is_none());
}

/// A capital-`X` keystroke emits a cancel intent regardless of the originating
/// screen (the banner cancel is global). When no task is active it is a no-op.
#[test]
fn banner_x_key_propagates_through_any_screen() {
    // No active task: X does nothing.
    let idle = BannerState::default();
    assert!(reduce_banner(&idle, BannerEvent::Key('X')).intent.is_none());

    // Active task: X emits the cancel intent (carrying the running task id).
    let mut s = BannerState::default();
    s = reduce_banner(&s, BannerEvent::Event(queued())).state;
    s = reduce_banner(&s, BannerEvent::Event(started())).state;
    let out = reduce_banner(&s, BannerEvent::Key('X'));
    assert_eq!(out.intent, Some(BannerIntent::CancelTask(task())));
}

/// A progress event updates the tool-call count shown on the banner.
#[test]
fn banner_progress_updates_tool_count() {
    let mut s = BannerState::default();
    s = reduce_banner(&s, BannerEvent::Event(queued())).state;
    s = reduce_banner(&s, BannerEvent::Event(started())).state;
    s = reduce_banner(
        &s,
        BannerEvent::Event(HangarEvent::TaskProgress {
            task_id: task(),
            tool_calls: 14,
            elapsed_ms: 1000,
        }),
    )
    .state;
    assert_eq!(s.banner().unwrap().tool_calls, 14);
}

/// At 80 columns the banner truncates the *message* line, never the glyphs /
/// agent label / `[X]` control.
#[test]
fn banner_render_at_80_cols_truncates_message_not_glyphs() {
    use ainb_plugin_hangar::widgets::frosted_banner::render_frosted_banner;
    let mut s = BannerState::default();
    s = reduce_banner(&s, BannerEvent::Event(queued())).state;
    s = reduce_banner(&s, BannerEvent::Event(started())).state;
    // A very long latest message.
    s = reduce_banner(
        &s,
        BannerEvent::Event(HangarEvent::TaskMessage {
            task_id: task(),
            kind: MessageKind::Agent,
            body: "Analyzing middleware structure ".repeat(20),
        }),
    )
    .state;

    let mut buf = WireBuffer::new(80, 24);
    render_frosted_banner(&mut buf, 80, 21, s.banner().unwrap(), s.latest_message());

    // The agent label and the [X] control survive at the floor width.
    let header = row_text(&buf, 21, 80);
    assert!(header.contains("claude-agent"), "agent label dropped: {header:?}");
    assert!(header.contains("[X]"), "cancel control dropped: {header:?}");
    // No cell painted past column 80.
    assert!(buf.cells.iter().all(|(c, _)| c.x < 80));
}

fn row_text(buf: &WireBuffer, row: u16, width: u16) -> String {
    let mut out = vec![' '; width as usize];
    for (coord, cell) in &buf.cells {
        if coord.y == row && coord.x < width {
            if let Some(ch) = cell.symbol.chars().next() {
                out[coord.x as usize] = ch;
            }
        }
    }
    out.into_iter().collect()
}
