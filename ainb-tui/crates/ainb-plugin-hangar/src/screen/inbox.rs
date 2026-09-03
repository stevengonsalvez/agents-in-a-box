//! e38.14 — Inbox screen: the aggregated notification inbox with an unread badge.
//!
//! The inbox screen (hotkey `I`) renders the durable notification aggregate the
//! daemon's inbox writer folds live issue / comment / task events into. Unlike a
//! live event stream, these survive a detach: an event that fired while the TUI
//! was closed is still here. The screen pulls `hangar/inbox_list` (an
//! [`InboxListResult`](ainb_hangar_proto::snapshots::InboxListResult)) and paints
//! each entry as one composed line, with an unread-count badge in the title.
//!
//! The wire row carries a pre-rendered `summary` full of ULIDs (`Task started:
//! 01M1GVN6...`). The plugin already holds the tasks, issues and agents
//! snapshots, so each line is recomposed locally (crisp B1, defect ledger
//! section 1.6): `subject_id` resolves through an [`InboxLookup`] to the agent
//! name and the issue's `HGR-n title`, `event` becomes a lowercase verb from the
//! task FSM vocabulary, `created_at` a relative age, and `read_at` a per-row
//! unread dot. A row nothing resolves keeps the daemon's summary, never a blank.
//!
//! It is intentionally minimal (the heavy data-plane proof is the store/RPC
//! layer): a read-only list. Pressing `r` raises a mark-read request the glue
//! turns into a `hangar/inbox_mark_read` RPC, after which the badge drops to zero.

use std::collections::BTreeMap;

use ainb_hangar_proto::events::InboxEntryRow;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use super::kanban::{age_label, short_id};

/// Title / accent gold.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Primary text (entry summary).
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted text (kind column, read entries, hints).
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Unread badge + unread-entry marker.
const UNREAD_AMBER: Color = Color::rgb(230, 190, 90);

/// The render state for the inbox screen.
///
/// Holds the aggregated entries (newest-first, as the daemon orders them) plus
/// the unread count the badge renders. Default is the empty pane shown before the
/// first `hangar/inbox_list` snapshot lands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InboxState {
    /// The aggregated inbox rows, newest-first.
    entries: Vec<InboxEntryRow>,
    /// The unread count (`read_at IS NULL`) the badge renders.
    unread: i64,
    /// The actor whose inbox this is (`member:me` today), painted in the header
    /// so the screen visibly proves WHOSE notifications these are.
    recipient: String,
}

impl InboxState {
    /// Build the state from an `hangar/inbox_list` snapshot result, tagged with
    /// the actor the snapshot was requested for.
    #[must_use]
    pub const fn from_snapshot(
        entries: Vec<InboxEntryRow>,
        unread: i64,
        recipient: String,
    ) -> Self {
        Self {
            entries,
            unread,
            recipient,
        }
    }

    /// The unread count (read accessor for the glue / tests).
    #[must_use]
    pub const fn unread(&self) -> i64 {
        self.unread
    }

    /// The actor whose inbox this is (read accessor for the glue / tests).
    #[must_use]
    pub fn recipient(&self) -> &str {
        &self.recipient
    }

    /// The aggregated entries (read accessor for tests).
    #[must_use]
    pub fn entries(&self) -> &[InboxEntryRow] {
        &self.entries
    }
}

/// A task the inbox can name: its agent's display label and its parent issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxTaskRef {
    /// The executing agent's roster name (or the Kanban short-id fallback).
    pub agent: String,
    /// The parent issue id, or `None` for an orphan task.
    pub issue_id: Option<String>,
}

/// An issue the inbox can name: its `HGR-n` display id and title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxIssueRef {
    /// The human display id (`HGR-3`), or the short id when the daemon sent none.
    pub display_id: String,
    /// The issue title.
    pub title: String,
}

/// The snapshot-derived names an inbox row resolves its ULIDs through (crisp
/// B1): projected by the glue from the cached tasks + issues + agents snapshots,
/// so the arrival order of those snapshots never matters. Rebuilt when a snapshot
/// moves, not per paint, so the render clock is passed to [`render_inbox`]
/// separately rather than riding along here.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InboxLookup {
    /// `task_id -> (agent label, parent issue)`.
    pub tasks: BTreeMap<String, InboxTaskRef>,
    /// `issue_id -> (display id, title)`.
    pub issues: BTreeMap<String, InboxIssueRef>,
}

impl InboxLookup {
    /// The `HGR-n title` form of `issue_id`, or `None` when the issues snapshot
    /// does not carry it (deleted, or not landed yet).
    fn issue_line(&self, issue_id: &str) -> Option<String> {
        self.issues.get(issue_id).map(|i| format!("{} {}", i.display_id, i.title))
    }
}

/// One composed inbox line: the actor + verb half and the subject half, so the
/// renderer can colour them apart (`impl-1 done` muted, `HGR-3 Add GET ...` white).
#[derive(Debug, Clone, PartialEq, Eq)]
struct InboxLine {
    /// `<agent> <verb>` for a run, `new issue` / `updated` / `comment` otherwise.
    head: String,
    /// `HGR-n title`, or the best fallback the row itself carries.
    subject: String,
}

/// Compose an entry's line from its wire fields plus the lookup (crisp B1).
///
/// Task rows resolve `subject_id` to the agent and the parent issue; the verb is
/// the task FSM word (`queued` / `running` / `done` / `failed` / `cancelled`),
/// the last three read off the daemon's `Task finished (<Result>)` summary. Issue
/// rows resolve the issue; a comment resolves the issue named in its summary; a
/// mention keeps its body as the subject. Anything else (or anything that fails
/// to resolve) falls back to the daemon's summary, so a row is never blank.
fn compose_line(entry: &InboxEntryRow, lookup: &InboxLookup) -> InboxLine {
    let fallback = |head: &str| InboxLine {
        head: head.to_string(),
        subject: entry.summary.clone(),
    };
    match entry.event.as_str() {
        "task_queued" | "task_started" | "task_finished" => {
            let verb = task_verb(entry);
            let Some(task) = lookup.tasks.get(&entry.subject_id) else {
                return InboxLine {
                    head: format!("run {verb}"),
                    subject: format!("#{}", short_id(&entry.subject_id)),
                };
            };
            let subject = task
                .issue_id
                .as_deref()
                .and_then(|id| lookup.issue_line(id))
                .unwrap_or_else(|| format!("#{}", short_id(&entry.subject_id)));
            InboxLine {
                head: format!("{} {verb}", task.agent),
                subject,
            }
        }
        "issue_created" | "issue_updated" | "issue_deleted" => {
            let head = match entry.event.as_str() {
                "issue_created" => "new issue",
                "issue_updated" => "updated",
                _ => "deleted",
            };
            match lookup.issue_line(&entry.subject_id) {
                Some(subject) => InboxLine {
                    head: head.to_string(),
                    subject,
                },
                // The daemon's summary is `<Verb phrase>: <title or id>`; keep the
                // half after the colon so a deleted issue still reads by title.
                None => InboxLine {
                    head: head.to_string(),
                    subject: entry
                        .summary
                        .split_once(": ")
                        .map_or_else(|| entry.summary.clone(), |(_, rest)| rest.to_string()),
                },
            }
        }
        // `New comment on <issue_id>`: the issue is only in the summary.
        "comment_added" => entry
            .summary
            .rsplit_once(" on ")
            .and_then(|(_, issue_id)| lookup.issue_line(issue_id))
            .map_or_else(
                || fallback("comment"),
                |subject| InboxLine {
                    head: "comment".to_string(),
                    subject,
                },
            ),
        "mention" => InboxLine {
            head: "mention".to_string(),
            subject: lookup.issue_line(&entry.subject_id).map_or_else(
                || entry.summary.clone(),
                |line| format!("{line} · {}", entry.summary),
            ),
        },
        _ => fallback(&entry.kind),
    }
}

/// The task FSM verb for a task row. A finished run's outcome is only in the
/// daemon's summary (`Task finished (Success): <id>`), so it is read from there;
/// an unrecognised outcome reads `finished` rather than guessing.
fn task_verb(entry: &InboxEntryRow) -> &'static str {
    match entry.event.as_str() {
        "task_queued" => "queued",
        "task_started" => "running",
        _ if entry.summary.contains("(Success)") => "done",
        _ if entry.summary.contains("(Failure)") => "failed",
        _ if entry.summary.contains("(Cancelled)") => "cancelled",
        _ => "finished",
    }
}

/// `true` for a run that ended in failure: the row the list floats first. The
/// verb the row already RENDERS is the input, so the sort and the text cannot
/// disagree, and the failure rule itself is the one shared with the usage
/// dashboard ([`crate::screen::is_failed_outcome`]).
fn is_failed(entry: &InboxEntryRow) -> bool {
    entry.event == "task_finished" && crate::screen::is_failed_outcome(task_verb(entry))
}

/// The coarse age bucket a row sorts within (crisp B1, Q10): the same unit the
/// age label uses (minutes / hours / days), so failed-first never drags a
/// week-old failure above this hour's rows.
fn age_bucket(created_at_ms: i64, now_ms: i64) -> u8 {
    let mins = now_ms.saturating_sub(created_at_ms).max(0) / 60_000;
    if mins < 60 {
        0
    } else if mins < 60 * 24 {
        1
    } else {
        2
    }
}

/// The entries in display order: the daemon's newest-first order, with failed
/// runs floated to the top of their age bucket (a stable sort, so everything
/// else keeps its place).
fn ordered<'a>(entries: &'a [InboxEntryRow], now_ms: i64) -> Vec<&'a InboxEntryRow> {
    let mut rows: Vec<&InboxEntryRow> = entries.iter().collect();
    rows.sort_by_key(|e| (age_bucket(e.created_at, now_ms), !is_failed(e)));
    rows
}

/// Render the inbox pane into `buf` between rows `top` and `bottom`.
///
/// Layout (top-to-bottom):
///
/// ```text
/// Inbox (member:me)  [3 unread]
/// r mark all read
/// ● 9m   qa-1 failed    HGR-5 Ticket stats: GET /api/tickets/stats
/// ● 2m   impl-1 done    HGR-3 Add GET /api/version endpoint
///   12m  new issue      HGR-8 Dependent B: must refuse to run while A is open
/// …
/// ```
///
/// The unread badge sits next to the title (`feedback_keybinding_hints_near_control`
/// for the `r` hint on the line below). Each entry is one row: an amber `●` when
/// unread, the relative age, the muted `<agent> <verb>` head padded to a column,
/// then the subject, amber when unread, soft-white once read. Failed runs sort
/// first within their age bucket. Strings truncate via `chars()`, never
/// byte-slice (the rust-utf8-truncate trap).
pub fn render_inbox(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &InboxState,
    lookup: &InboxLookup,
    now_ms: i64,
) {
    let mut row = top;

    // Title + the actor this inbox belongs to + unread badge. Every entry is
    // addressed to exactly one actor (store migration 0060), so the header names
    // whose inbox is on screen rather than implying a workspace-wide feed.
    let mut x = put_str(buf, 0, row, "Inbox", GOLD, area_w);
    if !state.recipient.is_empty() {
        x = put_str(buf, x, row, " (", MUTED_GRAY, area_w);
        x = put_str(buf, x, row, &state.recipient, MUTED_GRAY, area_w);
        x = put_str(buf, x, row, ")", MUTED_GRAY, area_w);
    }
    if state.unread > 0 {
        x = put_str(buf, x, row, "  ", MUTED_GRAY, area_w);
        x = put_str(buf, x, row, "[", MUTED_GRAY, area_w);
        x = put_str(buf, x, row, &state.unread.to_string(), UNREAD_AMBER, area_w);
        let _ = put_str(buf, x, row, " unread]", UNREAD_AMBER, area_w);
    }
    row += 1;

    // The mark-all-read hint next to its key.
    put_str(buf, 0, row, "r mark all read", MUTED_GRAY, area_w);
    row += 2;

    if state.entries.is_empty() {
        put_str(buf, 0, row, "no notifications", MUTED_GRAY, area_w);
        return;
    }

    for entry in ordered(&state.entries, now_ms) {
        if row > bottom {
            break;
        }
        render_entry(buf, row, area_w, entry, lookup, now_ms);
        row += 1;
    }
}

/// Render one inbox entry: `● <age>  <head>  <subject>`, the dot only when
/// unread, the subject coloured by read state.
fn render_entry(
    buf: &mut WireBuffer,
    row: u16,
    area_w: u16,
    entry: &InboxEntryRow,
    lookup: &InboxLookup,
    now_ms: i64,
) {
    let unread = entry.read_at.is_none();
    let line = compose_line(entry, lookup);
    let dot = if unread { "● " } else { "  " };
    let mut x = put_str(buf, 0, row, dot, UNREAD_AMBER, area_w);
    x = put_str(
        buf,
        x,
        row,
        &pad_to(&age_label(entry.created_at, now_ms), 5),
        MUTED_GRAY,
        area_w,
    );
    // The head column pads to the longest common shape (`impl-1 running`) so the
    // subjects align; a longer head simply pushes its own subject right.
    x = put_str(buf, x, row, &pad_to(&line.head, 15), MUTED_GRAY, area_w);
    x = put_str(buf, x, row, " ", MUTED_GRAY, area_w);
    let color = if unread { UNREAD_AMBER } else { SOFT_WHITE };
    let _ = put_str(buf, x, row, &line.subject, color, area_w);
}

/// Right-pad `s` to `width` chars (char-safe). A longer string is returned as-is
/// (the summary clip happens in `put_str`).
fn pad_to(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        let mut out = s.to_string();
        out.extend(std::iter::repeat_n(' ', width - len));
        out
    }
}

/// Write `s` at `(x, row)` in `color`, clipping at `right`. Returns the next free
/// column. Char-safe (iterates `char`s, not bytes — the utf8-truncate trap).
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        put_cell(buf, cx, row, ch, color);
        cx = cx.saturating_add(1);
    }
    cx
}

/// Write a single coloured glyph at `(x, row)`.
fn put_cell(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
}

/// The colours, exported so a snapshot/render test can assert the unread badge +
/// per-entry colouring non-vacuously without re-declaring the RGB triples.
pub mod colors {
    use ainb_plugin_sdk::Color;

    /// Unread badge + unread-entry amber.
    pub const UNREAD: Color = super::UNREAD_AMBER;
    /// Read-entry soft white.
    pub const READ: Color = super::SOFT_WHITE;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str, summary: &str, read: bool) -> InboxEntryRow {
        InboxEntryRow {
            id: format!("ie-{summary}"),
            kind: kind.into(),
            event: "issue_created".into(),
            subject_id: "s-1".into(),
            summary: summary.into(),
            recipient: "member:me".into(),
            created_at: 0,
            read_at: if read { Some(100) } else { None },
        }
    }

    /// Collect the rendered glyphs at `row` into a string (for assertions).
    fn row_text(buf: &WireBuffer, row: u16, width: u16) -> String {
        let mut s = String::new();
        for x in 0..width {
            let ch = buf
                .cells
                .iter()
                .find(|(coord, _)| coord.x == x && coord.y == row)
                .map_or(' ', |(_, c)| c.symbol.chars().next().unwrap_or(' '));
            s.push(ch);
        }
        s.trim_end().to_string()
    }

    /// The render clock every inbox test ages its rows against.
    const NOW: i64 = 1_700_000_600_000;

    fn task_entry(
        id: &str,
        event: &str,
        summary: &str,
        created_at: i64,
        read: bool,
    ) -> InboxEntryRow {
        InboxEntryRow {
            id: format!("ie-{id}-{event}"),
            kind: "task".into(),
            event: event.into(),
            subject_id: id.into(),
            summary: summary.into(),
            recipient: "member:me".into(),
            created_at,
            read_at: if read { Some(100) } else { None },
        }
    }

    /// The snapshots a real session holds: one task by impl-1 on HGR-3.
    fn lookup() -> InboxLookup {
        InboxLookup {
            tasks: BTreeMap::from([(
                "01M1GVN6MAF3121GEDM1E66KW5".to_string(),
                InboxTaskRef {
                    agent: "impl-1".into(),
                    issue_id: Some("01M1FH6AG5YJF1S".into()),
                },
            )]),
            issues: BTreeMap::from([(
                "01M1FH6AG5YJF1S".to_string(),
                InboxIssueRef {
                    display_id: "HGR-3".into(),
                    title: "Add GET /api/version endpoint".into(),
                },
            )]),
        }
    }

    /// Crisp B1: a task row reads `<agent> <verb>  <HGR-n> <title>` with a
    /// relative age and an unread dot, never the ULID the summary carries; the
    /// finished verb comes from the summary's `(Result)`.
    #[test]
    fn task_rows_read_as_agent_verb_issue_with_age_and_unread_dot() {
        let task = "01M1GVN6MAF3121GEDM1E66KW5";
        let state = InboxState::from_snapshot(
            vec![
                task_entry(
                    task,
                    "task_finished",
                    &format!("Task finished (Success): {task}"),
                    NOW - 120_000,
                    false,
                ),
                task_entry(
                    task,
                    "task_started",
                    &format!("Task started: {task}"),
                    NOW - 240_000,
                    true,
                ),
            ],
            1,
            "member:me".into(),
        );
        let mut buf = WireBuffer::new(80, 24);
        render_inbox(&mut buf, 80, 0, 20, &state, &lookup(), NOW);
        let r0 = row_text(&buf, 3, 80);
        let r1 = row_text(&buf, 4, 80);
        assert!(r0.starts_with("● 2m"), "unread dot + age: {r0:?}");
        assert!(r0.contains("impl-1 done"), "agent + verb: {r0:?}");
        assert!(
            r0.contains("HGR-3 Add GET /api/version endpoint"),
            "issue: {r0:?}"
        );
        assert!(!r0.contains(task), "no ULID on the row: {r0:?}");
        assert!(r1.starts_with("  4m"), "a read row has no dot: {r1:?}");
        assert!(
            r1.contains("impl-1 running"),
            "started reads as running: {r1:?}"
        );
    }

    /// A task the snapshots do not know (an orphan, or a card gone from the
    /// board) still reads as a run with a short id, and the other event families
    /// resolve their issue or keep the daemon's summary. Nothing renders blank.
    #[test]
    fn unresolved_rows_fall_back_to_short_ids_and_summaries() {
        let orphan = task_entry(
            "01M1ZZZZZZZZZZZZZZZZZZZZZZ",
            "task_finished",
            "Task finished (Failure): 01M1ZZZZZZZZZZZZZZZZZZZZZZ",
            NOW,
            false,
        );
        let mut created = entry("issue", "New issue: Boxtrack scaffold v2", false);
        created.subject_id = "01M1FH6AG5YJF1S".into();
        let deleted = InboxEntryRow {
            event: "issue_deleted".into(),
            ..entry("issue", "Issue deleted: 01M1GONE", false)
        };
        let comment = InboxEntryRow {
            event: "comment_added".into(),
            ..entry("comment", "New comment on 01M1FH6AG5YJF1S", false)
        };
        let unknown = InboxEntryRow {
            event: "something_new".into(),
            ..entry("issue", "A future summary", false)
        };
        let state = InboxState::from_snapshot(
            vec![orphan, created, deleted, comment, unknown],
            5,
            "member:me".into(),
        );
        let mut buf = WireBuffer::new(80, 24);
        render_inbox(&mut buf, 80, 0, 20, &state, &lookup(), NOW);
        let rows: Vec<String> = (3..8).map(|r| row_text(&buf, r, 80)).collect();
        assert!(
            rows[0].contains("run failed") && rows[0].contains("#ZZZZZZ"),
            "{:?}",
            rows[0]
        );
        assert!(
            rows[1].contains("new issue") && rows[1].contains("HGR-3 Add GET"),
            "{:?}",
            rows[1]
        );
        assert!(
            rows[2].contains("deleted") && rows[2].contains("01M1GONE"),
            "{:?}",
            rows[2]
        );
        assert!(
            rows[3].contains("comment") && rows[3].contains("HGR-3 Add GET"),
            "{:?}",
            rows[3]
        );
        assert!(
            rows[4].contains("issue") && rows[4].contains("A future summary"),
            "{:?}",
            rows[4]
        );
    }

    /// Crisp B1 (Q10): a failed run floats to the top of its age bucket, ahead of
    /// newer successes, but never above a younger bucket (this hour's rows stay
    /// above yesterday's failure).
    #[test]
    fn failed_runs_sort_first_within_their_age_bucket() {
        let hour = 3_600_000;
        let day = 24 * hour;
        let ok_now = task_entry(
            "t-ok",
            "task_finished",
            "Task finished (Success): t-ok",
            NOW - 60_000,
            false,
        );
        let fail_now = task_entry(
            "t-bad",
            "task_finished",
            "Task finished (Failure): t-bad",
            NOW - 30 * 60_000,
            false,
        );
        let fail_old = task_entry(
            "t-old",
            "task_finished",
            "Task finished (Failure): t-old",
            NOW - 2 * day,
            false,
        );
        let ok_today = task_entry(
            "t-mid",
            "task_finished",
            "Task finished (Success): t-mid",
            NOW - 3 * hour,
            false,
        );
        // Daemon order: newest first.
        let entries = vec![
            ok_now.clone(),
            fail_now.clone(),
            ok_today.clone(),
            fail_old.clone(),
        ];
        let ids: Vec<&str> =
            ordered(&entries, NOW).into_iter().map(|e| e.subject_id.as_str()).collect();
        assert_eq!(ids, vec!["t-bad", "t-ok", "t-mid", "t-old"]);
    }

    #[test]
    fn renders_unread_badge_and_entries() {
        let state = InboxState::from_snapshot(
            vec![
                entry("issue", "New issue: Refactor API", false),
                entry("comment", "New comment on issue-1", false),
            ],
            2,
            "member:me".into(),
        );
        let mut buf = WireBuffer::new(60, 24);
        render_inbox(&mut buf, 60, 0, 20, &state, &InboxLookup::default(), NOW);

        // Title row carries the unread badge.
        let title = row_text(&buf, 0, 60);
        assert!(title.starts_with("Inbox"), "title: {title:?}");
        assert!(title.contains("2 unread"), "badge shows count: {title:?}");

        // The two entries render their kind + summary below the hint row
        // (title=0, hint=1, blank=2, entries from row 3).
        let body = (3..7).map(|r| row_text(&buf, r, 60)).collect::<Vec<_>>().join("\n");
        assert!(body.contains("new issue"), "issue entry verb: {body}");
        assert!(body.contains("Refactor API"), "issue title: {body}");
        assert!(
            body.contains("New comment on issue-1"),
            "comment summary: {body}"
        );
    }

    /// The header names WHOSE inbox is on screen, and the pane paints only the
    /// entries it was handed.
    ///
    /// MUTATION GUARD: dropping the recipient from the title line fails the
    /// header assertion — the screen would imply a workspace-wide feed while the
    /// data plane returns one actor's rows.
    #[test]
    fn header_names_the_recipient_and_only_their_entries_render() {
        let state = InboxState::from_snapshot(
            vec![entry("comment", "New comment on issue-1", false)],
            1,
            "member:me".into(),
        );
        let mut buf = WireBuffer::new(60, 24);
        render_inbox(&mut buf, 60, 0, 20, &state, &InboxLookup::default(), NOW);

        let title = row_text(&buf, 0, 60);
        assert!(
            title.contains("member:me"),
            "the header names the recipient: {title:?}"
        );
        assert!(title.starts_with("Inbox"), "title: {title:?}");
        assert_eq!(state.recipient(), "member:me");

        // Only the one handed-in entry paints; no other actor's row appears.
        let body = (3..8).map(|r| row_text(&buf, r, 60)).collect::<Vec<_>>().join("\n");
        assert!(body.contains("New comment on issue-1"), "{body}");
        assert_eq!(
            body.lines().filter(|l| !l.trim().is_empty()).count(),
            1,
            "exactly the entries handed to the screen render: {body}"
        );
    }

    #[test]
    fn unread_entries_render_amber_read_entries_white() {
        let state =
            InboxState::from_snapshot(vec![entry("task", "Task finished", true)], 0, String::new());
        let mut buf = WireBuffer::new(60, 24);
        render_inbox(&mut buf, 60, 0, 20, &state, &InboxLookup::default(), NOW);

        // No badge when unread is zero.
        let title = row_text(&buf, 0, 60);
        assert_eq!(title, "Inbox", "no badge when zero unread: {title:?}");

        // A read entry's summary glyph is soft-white, not unread-amber. The
        // single entry renders at row 3 (title=0, hint=1, blank=2).
        let summary_cell = buf
            .cells
            .iter()
            .find(|(coord, c)| coord.y == 3 && c.symbol == "T")
            .map(|(_, c)| c.fg);
        assert_eq!(
            summary_cell,
            Some(Some(colors::READ)),
            "a read entry renders soft-white, not amber"
        );
    }

    #[test]
    fn empty_inbox_renders_placeholder() {
        let state = InboxState::default();
        let mut buf = WireBuffer::new(60, 24);
        render_inbox(&mut buf, 60, 0, 20, &state, &InboxLookup::default(), NOW);
        // The empty placeholder renders at row 3 (title=0, hint=1, blank=2).
        let body = (3..5).map(|r| row_text(&buf, r, 60)).collect::<Vec<_>>().join("\n");
        assert!(
            body.contains("no notifications"),
            "empty placeholder: {body}"
        );
        assert_eq!(state.unread(), 0);
    }
}
