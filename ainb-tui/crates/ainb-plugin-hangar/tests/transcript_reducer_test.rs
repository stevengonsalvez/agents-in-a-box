//! P4.4 RED — task-detail / transcript reducer behaviour.
//!
//! These tests pin the pure reducer contract for the task-detail screen
//! ([`ainb_plugin_hangar::screen::task_detail`]): streaming append, sticky-bottom
//! auto-scroll, the retry / cancel intents gated on task lifecycle, the
//! cancel-confirm modal abort path, and collapsible grouping of long thinking
//! runs. No IO — every transition is folded through `reduce_task_detail`.

use ainb_hangar_core::ids::{CommentId, IssueId, TaskId};
use ainb_hangar_proto::events::{
    CommentRow, HangarEvent, IssueRow, MessageKind, PresenceState, TaskResult,
};
use ainb_plugin_hangar::screen::task_detail::{
    TaskDetailEvent, TaskDetailIntent, TaskDetailState, TaskLifecycle, TranscriptEntry,
    reduce_task_detail,
};
use chrono::{TimeZone, Utc};

fn task() -> TaskId {
    TaskId::from_str("t1").unwrap()
}

fn issue_row() -> IssueRow {
    IssueRow {
        subscriber_count: 0,
        subscribed: false,
        reactions: Vec::new(),
        last_dispatch_reason: None,
        last_dispatch_detail: None,
        last_dispatch_at: None,
        origin_type: None,
        origin_id: None,
        id: IssueId::from_str("i1").unwrap(),
        display_id: None,
        workspace_id: "ws".into(),
        title: "Refactor API".into(),
        description: Some("desc".into()),
        state: "in_progress".into(),
        assignee: Some("agent:claude-agent".into()),
        creator: "member:alice".into(),
        created_at: 0,
        priority: 0,
        due_date: None,
        labels: Vec::new(),
        pr_url: None,
        branch: None,
        repo_ref: None,
        agent: None,
        source_branch: None,
        target_branch: None,
        external_ref: None,
        run_count: 0,
        last_run_status: None,
        last_run_at: None,
        parent_id: None,
        child_total: 0,
        child_done: 0,
        acceptance_criteria: Vec::new(),
        acceptance: Vec::new(),
        context_refs: Vec::new(),
        dependencies: Vec::new(),
    }
}

/// A fresh task-detail state bound to task `t1` for issue `i1`.
fn state_for_task() -> TaskDetailState {
    TaskDetailState::new(task(), issue_row())
}

fn message_event(kind: MessageKind, body: &str) -> HangarEvent {
    HangarEvent::TaskMessage {
        task_id: task(),
        kind,
        body: body.into(),
    }
}

/// A `TaskMessage` event for the bound task lands at the bottom of the transcript.
#[test]
fn task_message_event_appends_to_transcript() {
    let s = state_for_task();
    let out = reduce_task_detail(
        &s,
        TaskDetailEvent::Event(message_event(MessageKind::Agent, "hello")),
    );
    let lines: Vec<&str> = out.state.transcript().map(TranscriptEntry::body).collect();
    assert_eq!(lines, vec!["hello"]);

    let out2 = reduce_task_detail(
        &out.state,
        TaskDetailEvent::Event(message_event(MessageKind::ToolCall, "ls")),
    );
    let lines2: Vec<&str> = out2.state.transcript().map(TranscriptEntry::body).collect();
    assert_eq!(lines2, vec!["hello", "ls"]);
    assert!(out2.intent.is_none());
}

/// While stuck to the bottom, each appended message advances the scroll offset
/// so the newest line stays visible.
#[test]
fn auto_scroll_sticks_to_bottom_when_at_bottom() {
    let mut s = state_for_task();
    assert!(s.is_stuck_to_bottom());
    for i in 0..10 {
        s = reduce_task_detail(
            &s,
            TaskDetailEvent::Event(message_event(MessageKind::Agent, &format!("line {i}"))),
        )
        .state;
    }
    // Still sticky, and the offset tracks the tail (last index = len - 1).
    assert!(s.is_stuck_to_bottom());
    assert_eq!(s.scroll_offset(), s.transcript_len().saturating_sub(1));
}

/// Scrolling up releases sticky-bottom; new messages no longer move the
/// viewport off the user's position.
#[test]
fn auto_scroll_releases_when_user_scrolls_up() {
    let mut s = state_for_task();
    for i in 0..10 {
        s = reduce_task_detail(
            &s,
            TaskDetailEvent::Event(message_event(MessageKind::Agent, &format!("line {i}"))),
        )
        .state;
    }
    // User scrolls up: sticky releases and the offset moves up.
    let scrolled = reduce_task_detail(&s, TaskDetailEvent::Key('k')).state;
    assert!(!scrolled.is_stuck_to_bottom());
    let offset_before = scrolled.scroll_offset();
    // A new message arrives — offset must NOT jump to the tail.
    let after = reduce_task_detail(
        &scrolled,
        TaskDetailEvent::Event(message_event(MessageKind::Agent, "newest")),
    )
    .state;
    assert!(!after.is_stuck_to_bottom());
    assert_eq!(after.scroll_offset(), offset_before);
}

/// `R` emits a retry intent only after the task reached a terminal state.
#[test]
fn r_key_emits_retry_intent_only_when_task_finished_or_failed() {
    // Running: no retry.
    let mut running = state_for_task();
    running = reduce_task_detail(
        &running,
        TaskDetailEvent::Event(HangarEvent::TaskStarted {
            task_id: task(),
            started_at: Utc.timestamp_opt(0, 0).unwrap(),
        }),
    )
    .state;
    assert_eq!(running.lifecycle(), TaskLifecycle::Running);
    assert!(reduce_task_detail(&running, TaskDetailEvent::Key('R')).intent.is_none());

    // Finished failure: retry allowed.
    let failed = reduce_task_detail(
        &running,
        TaskDetailEvent::Event(HangarEvent::TaskFinished {
            task_id: task(),
            result: TaskResult::Failure,
            ended_at: Utc.timestamp_opt(10, 0).unwrap(),
        }),
    )
    .state;
    assert_eq!(failed.lifecycle(), TaskLifecycle::Failed);
    let retry = reduce_task_detail(&failed, TaskDetailEvent::Key('R'));
    assert_eq!(retry.intent, Some(TaskDetailIntent::RetryTask(task())));

    // Finished success: retry also allowed (re-run).
    let done = reduce_task_detail(
        &running,
        TaskDetailEvent::Event(HangarEvent::TaskFinished {
            task_id: task(),
            result: TaskResult::Success,
            ended_at: Utc.timestamp_opt(10, 0).unwrap(),
        }),
    )
    .state;
    assert_eq!(done.lifecycle(), TaskLifecycle::Succeeded);
    assert_eq!(
        reduce_task_detail(&done, TaskDetailEvent::Key('R')).intent,
        Some(TaskDetailIntent::RetryTask(task()))
    );
}

/// `X` opens the cancel-confirm modal only while the task is running; pressing
/// Enter in the modal emits the cancel intent.
#[test]
fn x_key_emits_cancel_intent_only_when_task_running() {
    // Not running yet (queued default): X is a no-op, no modal.
    let queued = state_for_task();
    let noop = reduce_task_detail(&queued, TaskDetailEvent::Key('X'));
    assert!(noop.intent.is_none());
    assert!(!noop.state.cancel_modal_open());

    // Running: X opens the confirm modal (no intent yet).
    let running = reduce_task_detail(
        &queued,
        TaskDetailEvent::Event(HangarEvent::TaskStarted {
            task_id: task(),
            started_at: Utc.timestamp_opt(0, 0).unwrap(),
        }),
    )
    .state;
    let opened = reduce_task_detail(&running, TaskDetailEvent::Key('X'));
    assert!(opened.state.cancel_modal_open());
    assert!(opened.intent.is_none());

    // Enter confirms → cancel intent.
    let confirmed = reduce_task_detail(&opened.state, TaskDetailEvent::Key('\n'));
    assert_eq!(confirmed.intent, Some(TaskDetailIntent::CancelTask(task())));
    assert!(!confirmed.state.cancel_modal_open());
}

/// Opening the cancel modal then pressing Esc aborts it without emitting a
/// cancel intent.
#[test]
fn x_key_then_esc_aborts_cancel_modal() {
    let running = reduce_task_detail(
        &state_for_task(),
        TaskDetailEvent::Event(HangarEvent::TaskStarted {
            task_id: task(),
            started_at: Utc.timestamp_opt(0, 0).unwrap(),
        }),
    )
    .state;
    let opened = reduce_task_detail(&running, TaskDetailEvent::Key('X')).state;
    assert!(opened.cancel_modal_open());
    let aborted = reduce_task_detail(&opened, TaskDetailEvent::Esc);
    assert!(!aborted.state.cancel_modal_open());
    assert!(aborted.intent.is_none());
}

/// `x` opens the delete-confirm modal (no intent yet) regardless of lifecycle;
/// Enter confirms → a `DeleteIssue` intent for the bound issue, closing the modal.
#[test]
fn x_key_opens_delete_modal_then_enter_deletes_bound_issue() {
    // Queued (never ran): `x` still opens the confirm — delete is not
    // lifecycle-gated (the daemon guards active tasks and surfaces a note).
    let s = state_for_task();
    let opened = reduce_task_detail(&s, TaskDetailEvent::Key('x'));
    assert!(opened.state.delete_modal_open(), "x opens the delete modal");
    assert!(opened.intent.is_none(), "opening emits no intent");

    // Enter confirms → delete intent for the bound issue (i1).
    let confirmed = reduce_task_detail(&opened.state, TaskDetailEvent::Key('\n'));
    assert_eq!(
        confirmed.intent,
        Some(TaskDetailIntent::DeleteIssue(
            IssueId::from_str("i1").unwrap()
        )),
        "Enter confirms the delete for the bound issue"
    );
    assert!(
        !confirmed.state.delete_modal_open(),
        "confirming closes the delete modal"
    );
}

/// Opening the delete modal then pressing Esc aborts it without emitting a
/// delete intent.
#[test]
fn x_key_then_esc_aborts_delete_modal() {
    let opened = reduce_task_detail(&state_for_task(), TaskDetailEvent::Key('x')).state;
    assert!(opened.delete_modal_open());
    let aborted = reduce_task_detail(&opened, TaskDetailEvent::Esc);
    assert!(!aborted.state.delete_modal_open(), "Esc closes the modal");
    assert!(aborted.intent.is_none(), "Esc emits no delete intent");
}

/// While the delete modal is open every non-Enter/Esc key is swallowed (the
/// modal is total), so a stray key never fires the delete or leaks to scroll.
#[test]
fn delete_modal_captures_other_keys() {
    let opened = reduce_task_detail(&state_for_task(), TaskDetailEvent::Key('x')).state;
    let after = reduce_task_detail(&opened, TaskDetailEvent::Key('j'));
    assert!(
        after.state.delete_modal_open(),
        "a stray key keeps the modal open"
    );
    assert!(after.intent.is_none(), "a stray key fires nothing");
}

/// A long consecutive run of `Thinking` lines collapses under a single
/// collapsible group entry rather than rendering all of them.
#[test]
fn transcript_groups_thinking_blocks_under_collapsible_when_long() {
    let mut s = state_for_task();
    // One agent line, then a long thinking run, then a tool call.
    s = reduce_task_detail(
        &s,
        TaskDetailEvent::Event(message_event(MessageKind::Agent, "start")),
    )
    .state;
    for i in 0..12 {
        s = reduce_task_detail(
            &s,
            TaskDetailEvent::Event(message_event(MessageKind::Thinking, &format!("think {i}"))),
        )
        .state;
    }
    s = reduce_task_detail(
        &s,
        TaskDetailEvent::Event(message_event(MessageKind::ToolCall, "run")),
    )
    .state;

    // The rendered (visible) view groups the 12 thinking lines into one
    // collapsed entry, so the visible run length is far shorter than raw.
    let visible = s.visible_entries();
    let collapsed = visible.iter().filter(|e| e.is_collapsed_group()).count();
    assert_eq!(
        collapsed, 1,
        "expected exactly one collapsed thinking group"
    );
    // Visible should be: agent line + 1 collapsed group + tool call = 3.
    assert_eq!(visible.len(), 3);
    // Raw transcript still holds every line.
    assert_eq!(s.transcript_len(), 14);
}

/// Comments interleave chronologically with transcript messages via the merged
/// stream (`CommentAdded` + `TaskMessage` land in arrival order).
#[test]
fn comments_interleave_with_transcript_in_arrival_order() {
    let mut s = state_for_task();
    s = reduce_task_detail(
        &s,
        TaskDetailEvent::Event(message_event(MessageKind::Agent, "msg1")),
    )
    .state;
    s = reduce_task_detail(
        &s,
        TaskDetailEvent::Event(HangarEvent::CommentAdded(CommentRow {
            id: CommentId::from_str("c1").unwrap(),
            issue_id: IssueId::from_str("i1").unwrap(),
            author: "member:alice".into(),
            body: "a comment".into(),
            created_at: 5,
        })),
    )
    .state;
    s = reduce_task_detail(
        &s,
        TaskDetailEvent::Event(message_event(MessageKind::ToolCall, "msg2")),
    )
    .state;

    let bodies: Vec<&str> = s.transcript().map(TranscriptEntry::body).collect();
    assert_eq!(bodies, vec!["msg1", "a comment", "msg2"]);
}

/// Events addressed to a different task are ignored (no cross-talk between
/// task-detail subscriptions).
#[test]
fn message_for_other_task_is_ignored() {
    let s = state_for_task();
    let other = HangarEvent::TaskMessage {
        task_id: TaskId::from_str("t-other").unwrap(),
        kind: MessageKind::Agent,
        body: "not mine".into(),
    };
    let out = reduce_task_detail(&s, TaskDetailEvent::Event(other));
    assert_eq!(out.state.transcript_len(), 0);
}

/// Presence events (not addressed to this screen) are harmless no-ops.
#[test]
fn unrelated_presence_event_is_noop() {
    let s = state_for_task();
    let out = reduce_task_detail(
        &s,
        TaskDetailEvent::Event(HangarEvent::AgentPresence {
            agent_id: ainb_hangar_core::ids::AgentId::from_str("a1").unwrap(),
            state: PresenceState::Online,
        }),
    );
    assert_eq!(out.state.transcript_len(), 0);
}

// --- e38.5 comment compose ----------------------------------------------------

/// `c` opens the compose modal with an empty buffer; typed printable chars and
/// Backspace edit it; the modal stays open (no intent) while typing.
#[test]
fn compose_key_opens_and_types_into_buffer() {
    let s = state_for_task();
    assert!(s.compose_buffer().is_none(), "compose closed initially");

    let out = reduce_task_detail(&s, TaskDetailEvent::Key('c'));
    assert_eq!(
        out.state.compose_buffer(),
        Some(""),
        "c opens an empty buffer"
    );
    assert!(out.intent.is_none());

    let out = reduce_task_detail(&out.state, TaskDetailEvent::Key('h'));
    let out = reduce_task_detail(&out.state, TaskDetailEvent::Key('i'));
    assert_eq!(out.state.compose_buffer(), Some("hi"));
    assert!(out.intent.is_none(), "typing emits no intent");

    // Backspace deletes the last char.
    let out = reduce_task_detail(&out.state, TaskDetailEvent::Key('\u{8}'));
    assert_eq!(out.state.compose_buffer(), Some("h"));
}

/// Enter on a non-empty compose buffer submits an `AddComment` intent carrying
/// the bound issue id + the typed body, and closes the modal.
#[test]
fn compose_enter_submits_add_comment_intent() {
    let s = state_for_task();
    let mut out = reduce_task_detail(&s, TaskDetailEvent::Key('c'));
    for ch in "ship it".chars() {
        out = reduce_task_detail(&out.state, TaskDetailEvent::Key(ch));
    }
    let out = reduce_task_detail(&out.state, TaskDetailEvent::Key('\n'));
    assert_eq!(
        out.intent,
        Some(TaskDetailIntent::AddComment {
            issue_id: IssueId::from_str("i1").unwrap(),
            body: "ship it".into(),
        }),
        "Enter must submit the typed comment for the bound issue"
    );
    assert!(
        out.state.compose_buffer().is_none(),
        "submitting closes the compose modal"
    );
}

/// Enter on an empty (or whitespace-only) compose buffer submits nothing and
/// keeps the modal open — never an empty comment.
#[test]
fn compose_enter_on_empty_buffer_does_not_submit() {
    let s = state_for_task();
    let out = reduce_task_detail(&s, TaskDetailEvent::Key('c'));
    let out = reduce_task_detail(&out.state, TaskDetailEvent::Key('\n'));
    assert!(out.intent.is_none(), "empty body must not submit");
    assert_eq!(out.state.compose_buffer(), Some(""), "modal stays open");
}

/// Esc closes the compose modal without submitting, discarding the buffer.
#[test]
fn compose_esc_cancels_without_submit() {
    let s = state_for_task();
    let mut out = reduce_task_detail(&s, TaskDetailEvent::Key('c'));
    for ch in "draft".chars() {
        out = reduce_task_detail(&out.state, TaskDetailEvent::Key(ch));
    }
    let out = reduce_task_detail(&out.state, TaskDetailEvent::Esc);
    assert!(out.intent.is_none(), "Esc submits nothing");
    assert!(out.state.compose_buffer().is_none(), "Esc closes the modal");
}

/// While the compose modal is open, scroll keys (`j`/`k`) type into the buffer
/// rather than moving the viewport — the modal captures input.
#[test]
fn compose_captures_navigation_keys() {
    let s = state_for_task();
    let out = reduce_task_detail(&s, TaskDetailEvent::Key('c'));
    let out = reduce_task_detail(&out.state, TaskDetailEvent::Key('j'));
    assert_eq!(
        out.state.compose_buffer(),
        Some("j"),
        "j is captured as text while composing"
    );
}
