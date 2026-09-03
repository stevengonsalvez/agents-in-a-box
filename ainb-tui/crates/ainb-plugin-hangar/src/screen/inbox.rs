//! e38.14 / crisp B3 §2.4 — Inbox: the ONE attention surface.
//!
//! The hangar grew three places that say "a human is needed": the Control
//! Center (`C`), this inbox (`I`), and the board's attention flag. Crisp B3
//! makes this screen the surface and the other two views of the same store.
//!
//! ```text
//! Inbox  member:me  [2 need you] [35 unread]   (all) asks runs issues   I inbox
//! ┌ needs you ─────────────────────────────────────────────────────────────┐
//! │▸● ASK  40s   boxtrack · Decide the Boxtrack sqlite file location       │
//! │  OPTIONS                                                               │
//! │  ① data/boxtrack.db                                                    │
//! │     Repo-root data/ dir, outside api/src                               │
//! │  h/l option · enter/1-9 answer                                         │
//! │ ● ERR  3m    api · rate_limited                                        │
//! └────────────────────────────────────────────────────────────────────────┘
//! ┌ recent ────────────────────────────────────────────────────────────────┐
//! │ ✗ 9m   qa-1 failed     HGR-5 Ticket stats: GET /api/tickets/stats      │
//! │ ● 2m   impl-1 done     HGR-3 Add GET /api/version endpoint             │
//! └────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! **`needs you`** is the open `attention/list` rows — the SAME
//! [`ControlCenterState`] the Control Center paints, handed in at render time
//! rather than copied, so the two surfaces cannot disagree about what is
//! waiting. The inline answer is [`control_center::render_options`] paired with
//! [`reduce_control_center`](super::control_center::reduce_control_center):
//! moved, not rewritten, so `I` raises the identical `attention/answer` RPC `C`
//! raises.
//!
//! **`recent`** is the durable `hangar/inbox_list` aggregate — events that
//! survive a detach. The wire row carries a pre-rendered `summary` full of ULIDs
//! (`Task started: 01M1GVN6...`), so each line is recomposed locally (crisp B1):
//! `subject_id` resolves through an [`InboxLookup`] to the agent name and the
//! issue's `HGR-n title`, `event` becomes a lowercase verb + glyph from
//! [`crate::vocab`], and `created_at` a relative age. A row nothing resolves
//! keeps the daemon's summary, never a blank. Failed runs float to the top of
//! their age bucket.
//!
//! **Filters** `(all) asks runs issues` are client-side over the cached rows
//! (`f` cycles): no refetch, and every cached row is reachable from at least one
//! of them. `r` still raises the mark-all-read request the glue turns into
//! `hangar/inbox_mark_read`.

use std::collections::BTreeMap;

use ainb_hangar_proto::events::InboxEntryRow;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use super::control_center::{self, ControlCenterState};
use super::kanban::{age_label, short_id};
use crate::vocab::{AttentionKind, RunState};

/// Title / accent gold (also the ASK code and the active filter chip).
const GOLD: Color = Color::rgb(255, 215, 0);
/// Primary text (entry summary).
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted text (kind column, read entries, hints, frames).
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Amber: the unread marker in `recent` and the `WAIT` code in `needs you` —
/// one "look at this" accent, never two shades of the same idea on one pane.
const UNREAD_AMBER: Color = Color::rgb(230, 190, 90);
/// Error red: the `ERR` code and a failed run's glyph.
const ALERT_RED: Color = Color::rgb(220, 100, 100);
/// The `▸` cursor on the focused attention row (the Control Center's `▶` green).
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);

/// The smallest block that can carry a frame: two borders and one content row.
const MIN_BLOCK_H: u16 = 3;

/// The client-side filter over the cached rows (crisp B3 §2.4), cycled by `f`.
///
/// Client-side is the point: no refetch, no daemon round-trip, and the rows the
/// screen already holds are the rows it filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InboxFilter {
    /// Everything: both blocks.
    #[default]
    All,
    /// Only the `needs you` block.
    Asks,
    /// Only run rows in `recent`.
    Runs,
    /// Everything in `recent` that is not a run: issues, comments, mentions,
    /// and any event family a newer daemon grows.
    Issues,
}

impl InboxFilter {
    /// The chips in strip order — also the set a guard asserts is exhaustive.
    pub const ALL: [Self; 4] = [Self::All, Self::Asks, Self::Runs, Self::Issues];

    /// The lowercase chip label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Asks => "asks",
            Self::Runs => "runs",
            Self::Issues => "issues",
        }
    }

    /// The next filter in the `f` cycle (wraps back to [`Self::All`]).
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::All => Self::Asks,
            Self::Asks => Self::Runs,
            Self::Runs => Self::Issues,
            Self::Issues => Self::All,
        }
    }

    /// `true` when the `needs you` block renders under this filter.
    #[must_use]
    const fn shows_attention(self) -> bool {
        matches!(self, Self::All | Self::Asks)
    }

    /// `true` when the `recent` block renders under this filter.
    #[must_use]
    const fn shows_recent(self) -> bool {
        matches!(self, Self::All | Self::Runs | Self::Issues)
    }

    /// `true` when `entry` renders in `recent` under this filter.
    ///
    /// `Issues` is "not a run" rather than a list of issue events on purpose:
    /// every cached row is then reachable from at least one filter, so an event
    /// family the daemon grows later cannot become invisible in every one of
    /// them (`every_cached_row_is_reachable_from_some_filter`).
    #[must_use]
    fn keeps(self, entry: &InboxEntryRow) -> bool {
        match self {
            Self::All => true,
            Self::Asks => false,
            Self::Runs => is_run_event(&entry.event),
            Self::Issues => !is_run_event(&entry.event),
        }
    }
}

/// The render state for the inbox screen.
///
/// Holds the aggregated entries (newest-first, as the daemon orders them), the
/// unread count the badge renders, and the human's filter. The `needs you`
/// rows do NOT live here: they are the Control Center's store, handed to
/// [`render_inbox`] so there is one attention state, not a copy that can go
/// stale. Default is the empty pane shown before the first snapshot lands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InboxState {
    /// The aggregated inbox rows, newest-first.
    entries: Vec<InboxEntryRow>,
    /// The unread count (`read_at IS NULL`) the badge renders.
    unread: i64,
    /// The actor whose inbox this is (`member:me` today), painted in the header
    /// so the screen visibly proves WHOSE notifications these are.
    recipient: String,
    /// The client-side filter (`f` cycles).
    filter: InboxFilter,
}

impl InboxState {
    /// Build the state from an `hangar/inbox_list` snapshot result, tagged with
    /// the actor the snapshot was requested for, filtered to [`InboxFilter::All`].
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
            filter: InboxFilter::All,
        }
    }

    /// Replace the rows + unread count from a fresh `hangar/inbox_list`
    /// snapshot, KEEPING the human's filter.
    ///
    /// In place rather than rebuilding the state, which is what makes the
    /// keeping structural: the `filter` field is never assigned here, so a
    /// refresh landing while `runs` is picked cannot snap the pane back to
    /// `all` by omission.
    pub fn replace_rows(&mut self, entries: Vec<InboxEntryRow>, unread: i64, recipient: String) {
        self.entries = entries;
        self.unread = unread;
        self.recipient = recipient;
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

    /// The active filter.
    #[must_use]
    pub const fn filter(&self) -> InboxFilter {
        self.filter
    }

    /// Advance the filter one step (the `f` key).
    pub const fn cycle_filter(&mut self) {
        self.filter = self.filter.next();
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

/// `true` for the three task-lifecycle event families — the rows the `runs`
/// filter keeps and the ones that carry a [`RunState`].
///
/// The one definition: the composer, the glyph and the filter all read it, so a
/// row cannot be a run for one of them and not for another.
fn is_run_event(event: &str) -> bool {
    matches!(event, "task_queued" | "task_started" | "task_finished")
}

/// Compose an entry's line from its wire fields plus the lookup (crisp B1).
///
/// Task rows resolve `subject_id` to the agent and the parent issue; the verb is
/// the task FSM word from [`crate::vocab`]. Issue rows resolve the issue; a
/// comment resolves the issue named in its summary; a mention keeps its body as
/// the subject. Anything else (or anything that fails to resolve) falls back to
/// the daemon's summary, so a row is never blank.
fn compose_line(entry: &InboxEntryRow, lookup: &InboxLookup) -> InboxLine {
    let fallback = |head: &str| InboxLine {
        head: head.to_string(),
        subject: entry.summary.clone(),
    };
    match entry.event.as_str() {
        event if is_run_event(event) => {
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

/// The vocabulary state a run row is in, or `None` when the row is not a run (or
/// is a `task_finished` whose outcome this build does not recognise).
///
/// A finished run's outcome is only in the daemon's summary (`Task finished
/// (Success): <id>`), so it is read from there. The event guard comes FIRST: an
/// issue title that happens to contain `(Success)` is not a finished run.
fn task_run_state(entry: &InboxEntryRow) -> Option<RunState> {
    if !is_run_event(&entry.event) {
        return None;
    }
    match entry.event.as_str() {
        "task_queued" => Some(RunState::Queued),
        "task_started" => Some(RunState::Running),
        _ if entry.summary.contains("(Success)") => Some(RunState::Done),
        _ if entry.summary.contains("(Failure)") => Some(RunState::Failed),
        _ if entry.summary.contains("(Cancelled)") => Some(RunState::Cancelled),
        _ => None,
    }
}

/// The task FSM verb for a task row, from the ONE vocabulary table (crisp B2
/// §2.1). An unrecognised outcome reads `finished` rather than guessing.
fn task_verb(entry: &InboxEntryRow) -> &'static str {
    task_run_state(entry).map_or("finished", RunState::word)
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

/// The entries `filter` keeps, in display order: the daemon's newest-first
/// order, with failed runs floated to the top of their age bucket (a stable
/// sort, so everything else keeps its place).
fn ordered<'a>(
    entries: &'a [InboxEntryRow],
    filter: InboxFilter,
    now_ms: i64,
) -> Vec<&'a InboxEntryRow> {
    let mut rows: Vec<&InboxEntryRow> = entries.iter().filter(|e| filter.keeps(e)).collect();
    rows.sort_by_key(|e| (age_bucket(e.created_at, now_ms), !is_failed(e)));
    rows
}

/// Render the inbox pane into `buf` between rows `top` and `bottom`.
///
/// `attention` is the Control Center's store, not a copy: the `needs you` block
/// and the `[N need you]` badge are views of it. `now_ms` is the render clock
/// both blocks age their rows against.
///
/// The two blocks split the body: `needs you` takes what it needs but never
/// more than half, so a long attention list cannot squeeze `recent` off the
/// screen. Under `asks` it takes the whole body; under `runs` / `issues`
/// `recent` does. Strings truncate via `chars()`, never byte-slice (the
/// rust-utf8-truncate trap).
pub fn render_inbox(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &InboxState,
    lookup: &InboxLookup,
    attention: &ControlCenterState,
    now_ms: i64,
) {
    render_header(buf, area_w, top, state, attention);
    let body_top = top.saturating_add(1);
    if body_top > bottom || area_w < 2 {
        return;
    }
    let body_h = bottom - body_top + 1;

    let mut row = body_top;
    if state.filter.shows_attention() {
        let want = needs_you_height(attention).saturating_add(2);
        let height = if state.filter.shows_recent() {
            want.min((body_h / 2).max(MIN_BLOCK_H))
        } else {
            body_h
        }
        .min(body_h);
        if height >= MIN_BLOCK_H {
            let block_bottom = row + height - 1;
            draw_frame(buf, area_w, row, block_bottom, "needs you");
            render_needs_you(buf, area_w, row + 1, block_bottom - 1, attention, now_ms);
            row = block_bottom + 1;
        }
    }
    if state.filter.shows_recent() && bottom >= row.saturating_add(MIN_BLOCK_H - 1) {
        draw_frame(buf, area_w, row, bottom, "recent");
        render_recent(buf, area_w, row + 1, bottom - 1, state, lookup, now_ms);
    }
}

/// The header row: the title, whose inbox it is, the two badges, the filter
/// chips, and the hotkey hint flush right.
fn render_header(
    buf: &mut WireBuffer,
    area_w: u16,
    row: u16,
    state: &InboxState,
    attention: &ControlCenterState,
) {
    let mut x = put_str(buf, 0, row, "Inbox", GOLD, area_w);
    // Every entry is addressed to exactly one actor (store migration 0060), so
    // the header names whose inbox is on screen rather than implying a
    // workspace-wide feed.
    if !state.recipient.is_empty() {
        x = put_str(buf, x, row, "  ", MUTED_GRAY, area_w);
        x = put_str(buf, x, row, &state.recipient, MUTED_GRAY, area_w);
    }
    // The same count the Control title paints, over the same store.
    let need = attention.needs_you_count();
    if need > 0 {
        x = put_str(buf, x, row, &format!("   [{need} need you]"), GOLD, area_w);
    }
    if state.unread > 0 {
        x = put_str(
            buf,
            x,
            row,
            &format!("  [{} unread]", state.unread),
            UNREAD_AMBER,
            area_w,
        );
    }
    x = put_str(buf, x, row, "    ", MUTED_GRAY, area_w);
    for filter in InboxFilter::ALL {
        let (text, color) = if filter == state.filter {
            (format!("({}) ", filter.label()), GOLD)
        } else {
            (format!("{} ", filter.label()), MUTED_GRAY)
        };
        x = put_str(buf, x, row, &text, color, area_w);
    }
    // The hotkey hint next to the control (feedback_keybinding_hints_near_control).
    let hint = "I inbox";
    let hint_w = u16::try_from(hint.chars().count()).unwrap_or(0);
    if let Some(start) = area_w.checked_sub(hint_w) {
        if start > x {
            put_str(buf, start, row, hint, MUTED_GRAY, area_w);
        }
    }
}

/// The content rows the `needs you` block would like: one per open attention
/// row, plus the selected card's inline answer block.
fn needs_you_height(attention: &ControlCenterState) -> u16 {
    if attention.cards().is_empty() {
        // The "nothing needs you" line.
        return 1;
    }
    let rows = u16::try_from(attention.cards().len()).unwrap_or(u16::MAX);
    rows.saturating_add(answer_block_height(attention))
}

/// The rows [`control_center::render_options`] paints under the selected card.
///
/// The ONE place that shape is counted: the block sizing and the row the next
/// card starts on both read it, so they cannot drift into overlapping paints.
fn answer_block_height(attention: &ControlCenterState) -> u16 {
    let Some(card) = attention.selected_card() else {
        return 0;
    };
    if !card.kind.is_answerable() || card.options().is_empty() {
        // The single "(no inline options — surfaced for visibility)" note.
        return 1;
    }
    // `OPTIONS` header + one row per option (two when it carries a description)
    // + the answer hint line.
    let options: u16 = card
        .options()
        .iter()
        .map(|o| u16::from(o.description.is_some()) + 1)
        .sum();
    options.saturating_add(2)
}

/// Render the `needs you` block: one row per open attention row, the focused
/// one marked `▸` and carrying the inline answer options beneath it.
fn render_needs_you(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    attention: &ControlCenterState,
    now_ms: i64,
) {
    let right = area_w.saturating_sub(1);
    if attention.cards().is_empty() {
        put_str(buf, 1, top, "✓ nothing needs you", MUTED_GRAY, right);
        return;
    }
    let selected = attention.selected_id();
    let mut row = top;
    for card in attention.cards() {
        if row > bottom {
            break;
        }
        let is_selected = Some(card.id.as_str()) == selected;
        let kind = card.vocab_kind();
        let color = code_color(kind);
        let mut x = put_str(
            buf,
            1,
            row,
            if is_selected { "▸" } else { " " },
            SELECTION_GREEN,
            right,
        );
        x = put_str(buf, x, row, &AttentionKind::GLYPH.to_string(), color, right);
        x = put_str(buf, x, row, &format!(" {} ", pad_to(kind.code(), 4)), color, right);
        let age = control_center::format_age(now_ms.saturating_sub(card.created_at));
        x = put_str(buf, x, row, &pad_to(&age, 6), MUTED_GRAY, right);
        // The attention feed carries a session id and a cwd, never an issue, so
        // the row names the session's directory and what it said. Resolving it
        // to `impl-1 on HGR-7` needs a session→task join no snapshot carries.
        let text = format!("{} · {}", card.short_label(), control_center::last_reply(card));
        put_str(buf, x, row, &text, SOFT_WHITE, right);
        row += 1;

        if is_selected && row <= bottom {
            // The inline answer, byte for byte the Control Center's: the same
            // ①②③ renderer, driven by the same reducer, raising the same
            // `attention/answer` RPC (crisp B3 §2.4, "moved not rewritten").
            control_center::render_options(
                buf,
                3,
                row,
                bottom,
                right,
                card,
                attention.option_cursor(),
            );
            row = row.saturating_add(answer_block_height(attention));
        }
    }
}

/// Render the `recent` block: the filtered, ordered aggregate rows.
fn render_recent(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &InboxState,
    lookup: &InboxLookup,
    now_ms: i64,
) {
    let rows = ordered(&state.entries, state.filter, now_ms);
    if rows.is_empty() {
        put_str(
            buf,
            1,
            top,
            "no notifications",
            MUTED_GRAY,
            area_w.saturating_sub(1),
        );
        return;
    }
    // Bounded zip: one row per entry from `top` until `bottom`, so the list can
    // never overrun the block and no manual counter is needed.
    for (row, entry) in (top..=bottom).zip(rows) {
        render_entry(buf, row, area_w, entry, lookup, now_ms);
    }
}

/// Render one inbox entry: `<glyph> <age>  <head>  <subject>`, the glyph from
/// the run vocabulary, the subject amber while the row is unread.
fn render_entry(
    buf: &mut WireBuffer,
    row: u16,
    area_w: u16,
    entry: &InboxEntryRow,
    lookup: &InboxLookup,
    now_ms: i64,
) {
    let right = area_w.saturating_sub(1);
    let unread = entry.read_at.is_none();
    let line = compose_line(entry, lookup);
    let (glyph, glyph_color) = row_glyph(entry);
    let mut x = put_str(buf, 1, row, &format!("{glyph} "), glyph_color, right);
    x = put_str(
        buf,
        x,
        row,
        &pad_to(&age_label(entry.created_at, now_ms), 5),
        MUTED_GRAY,
        right,
    );
    // The head column pads to the longest common shape (`impl-1 running`) so the
    // subjects align; a longer head simply pushes its own subject right.
    x = put_str(buf, x, row, &pad_to(&line.head, 15), MUTED_GRAY, right);
    x = put_str(buf, x, row, " ", MUTED_GRAY, right);
    let color = if unread { UNREAD_AMBER } else { SOFT_WHITE };
    let _ = put_str(buf, x, row, &line.subject, color, right);
}

/// The leading glyph of a `recent` row: the run state's glyph from the ONE
/// vocabulary table, `+` for a new issue, `·` for everything else.
fn row_glyph(entry: &InboxEntryRow) -> (char, Color) {
    if let Some(state) = task_run_state(entry) {
        return (state.glyph(), run_color(state));
    }
    match entry.event.as_str() {
        "issue_created" => ('+', SOFT_WHITE),
        _ => ('·', MUTED_GRAY),
    }
}

/// A failed run is the row an operator opens this list to find, so it is the one
/// glyph that carries a colour; the other four are told apart by shape (the
/// vocabulary pins them distinct).
const fn run_color(state: RunState) -> Color {
    match state {
        RunState::Failed => ALERT_RED,
        _ => MUTED_GRAY,
    }
}

/// The accent an attention code paints in: gold for a question, red for an
/// error, muted for an idle session, amber for an explicit wait.
const fn code_color(kind: AttentionKind) -> Color {
    match kind {
        AttentionKind::Ask => GOLD,
        AttentionKind::Err => ALERT_RED,
        AttentionKind::Idle => MUTED_GRAY,
        AttentionKind::Wait => UNREAD_AMBER,
    }
}

/// Draw a titled frame `┌ <label> ───┐ … └───┘` around rows `top..=bottom`.
fn draw_frame(buf: &mut WireBuffer, area_w: u16, top: u16, bottom: u16, label: &str) {
    let right = area_w.saturating_sub(1);
    if right == 0 || bottom <= top {
        return;
    }
    let head = put_str(buf, 0, top, &format!("┌ {label} "), MUTED_GRAY, right);
    for x in head..right {
        put_cell(buf, x, top, '─', MUTED_GRAY);
    }
    put_cell(buf, right, top, '┐', MUTED_GRAY);
    for row in (top + 1)..bottom {
        put_cell(buf, 0, row, '│', MUTED_GRAY);
        put_cell(buf, right, row, '│', MUTED_GRAY);
    }
    put_cell(buf, 0, bottom, '└', MUTED_GRAY);
    for x in 1..right {
        put_cell(buf, x, bottom, '─', MUTED_GRAY);
    }
    put_cell(buf, right, bottom, '┘', MUTED_GRAY);
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
        // Same hardening as the Control Center's renderer, for the same reason:
        // this pane now paints fleet-wide, session-originated free text (ASK
        // questions, error snippets, cwd labels) and daemon-authored summaries
        // char by char, and each char becomes a Cell symbol the host paints
        // verbatim. A raw ESC/BEL/C1 byte would reassemble on flush into a live
        // control sequence (OSC 52 clipboard write, title set) in the operator's
        // terminal. Every control char is surfaced as a visible middot, never
        // executed; `put_str` is the one choke point all rendered text flows
        // through.
        put_cell(buf, cx, row, if ch.is_control() { '·' } else { ch }, color);
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
    use ainb_hangar_proto::events::AttentionRow;

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

    /// A rendered row with the block's frame rails stripped.
    fn row_body(buf: &WireBuffer, row: u16, width: u16) -> String {
        row_text(buf, row, width)
            .trim_start_matches('│')
            .trim_end_matches('│')
            .trim()
            .to_string()
    }

    /// The render clock every inbox test ages its rows against.
    const NOW: i64 = 1_700_000_600_000;

    /// An empty attention board — the `needs you` block with nothing in it.
    fn no_attention() -> ControlCenterState {
        ControlCenterState::default()
    }

    /// The first `recent` content row when the attention board is empty:
    /// header(0), `needs you` frame(1..3), `recent` frame top(4), rows from 5.
    const RECENT_TOP: u16 = 5;

    fn attention_row(id: &str, kind: &str, created_at: i64, payload: &str) -> AttentionRow {
        AttentionRow {
            id: id.to_string(),
            session_id: format!("sess-{id}"),
            cwd: format!("/work/{id}"),
            workspace_id: None,
            kind: kind.to_string(),
            payload: payload.to_string(),
            degraded: false,
            created_at,
            channels: ainb_hangar_proto::ChannelSet::NONE,
        }
    }

    fn ask_payload(question: &str, options: &[(&str, Option<&str>)]) -> String {
        let options: Vec<serde_json::Value> = options
            .iter()
            .map(|(label, desc)| match desc {
                Some(d) => serde_json::json!({ "label": label, "description": d }),
                None => serde_json::json!({ "label": label }),
            })
            .collect();
        serde_json::json!({
            "kind": "ASK",
            "context": { "question": question, "options": options }
        })
        .to_string()
    }

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

    // --- crisp B3 §2.4: the `needs you` block ------------------------------

    /// The `needs you` block is the open attention rows: the vocabulary code and
    /// its dot, a live age, the raising session, and what it asked — with the
    /// focused row's options rendered inline beneath it, ready to answer.
    ///
    /// MUTATION GUARD: this is the whole point of the step. Drop the block, drop
    /// the options, or paint the wire kind instead of the vocabulary code, and
    /// one of these four assertions fails.
    #[test]
    fn needs_you_block_renders_open_attention_rows_with_inline_options() {
        let mut attention = ControlCenterState::default();
        attention.set_attention(&[
            attention_row(
                "ask-1",
                "ask_user_question",
                NOW - 40_000,
                &ask_payload(
                    "Decide the Boxtrack sqlite file location",
                    &[
                        ("data/boxtrack.db", Some("Repo-root data/ dir")),
                        ("api/app.db", None),
                    ],
                ),
            ),
            attention_row(
                "err-1",
                "error",
                NOW - 180_000,
                r#"{"kind":"ERR","context":{"pattern":"rate_limited","snippet":"429 slow down"}}"#,
            ),
        ]);
        let state = InboxState::from_snapshot(vec![], 0, "member:me".into());
        let mut buf = WireBuffer::new(80, 24);
        render_inbox(&mut buf, 80, 0, 20, &state, &lookup(), &attention, NOW);

        let rows: Vec<String> = (1..11).map(|r| row_body(&buf, r, 80)).collect();
        let block = rows.join("\n");
        assert!(block.contains("needs you"), "block title: {block}");
        assert!(
            rows.iter().any(|r| r.starts_with("▸● ASK  40s")),
            "the focused ASK row carries the vocab code + a live age: {block}"
        );
        assert!(
            rows.iter()
                .any(|r| r.contains("ask-1 · Decide the Boxtrack sqlite file location")),
            "the row names the raising session and its question: {block}"
        );
        assert!(
            block.contains("① data/boxtrack.db") && block.contains("② api/app.db"),
            "the focused ASK answers inline: {block}"
        );
        assert!(
            rows.iter().any(|r| r.starts_with("● ERR  3m")),
            "the error row reads as ERR, never `error`: {block}"
        );
    }

    /// The `[N need you]` badge is the Control Center's count over the Control
    /// Center's store — one number, two surfaces.
    ///
    /// MUTATION GUARD: computing it from anything else (the entry list, a cached
    /// copy) disagrees with `needs_you_count` the moment a row is answered.
    #[test]
    fn header_badge_is_the_control_center_count() {
        let mut attention = ControlCenterState::default();
        attention.set_attention(&[
            attention_row("a1", "ask_user_question", 1, &ask_payload("q", &[("y", None)])),
            attention_row("a2", "approval", 2, r#"{"kind":"WAIT","context":{}}"#),
            attention_row(
                "e1",
                "error",
                3,
                r#"{"kind":"ERR","context":{"pattern":"x"}}"#,
            ),
        ]);
        let state = InboxState::from_snapshot(vec![], 0, "member:me".into());
        let mut buf = WireBuffer::new(100, 24);
        render_inbox(&mut buf, 100, 0, 20, &state, &lookup(), &attention, NOW);
        let header = row_text(&buf, 0, 100);
        assert_eq!(attention.needs_you_count(), 2, "two decisions, one error");
        assert!(header.contains("[2 need you]"), "header: {header:?}");
    }

    /// An empty attention board still renders the block, saying so — the surface
    /// is the anchor whether or not anything is waiting.
    #[test]
    fn needs_you_block_says_so_when_nothing_is_waiting() {
        let state = InboxState::from_snapshot(vec![], 0, "member:me".into());
        let mut buf = WireBuffer::new(80, 24);
        render_inbox(&mut buf, 80, 0, 20, &state, &lookup(), &no_attention(), NOW);
        let header = row_text(&buf, 0, 80);
        assert!(!header.contains("need you"), "no badge: {header:?}");
        assert!(
            row_body(&buf, 2, 80).contains("nothing needs you"),
            "{:?}",
            row_body(&buf, 2, 80)
        );
    }

    // --- crisp B3 §2.4: the filters ----------------------------------------

    /// `f` cycles the four chips and wraps.
    #[test]
    fn f_cycles_the_filter_and_wraps() {
        let mut state = InboxState::default();
        let mut seen = vec![state.filter()];
        for _ in 0..InboxFilter::ALL.len() {
            state.cycle_filter();
            seen.push(state.filter());
        }
        assert_eq!(
            seen,
            vec![
                InboxFilter::All,
                InboxFilter::Asks,
                InboxFilter::Runs,
                InboxFilter::Issues,
                InboxFilter::All,
            ]
        );
    }

    /// Every cached row is reachable from at least one filter.
    ///
    /// MUTATION GUARD: this asserts the SET of families, not a count. Narrowing
    /// `Issues` to a list of known issue events would make a `mention` (or any
    /// family a newer daemon grows) invisible under every filter but `all`, and
    /// invisible entirely once the operator leaves `all` — the failure mode a
    /// count-based guard cannot see.
    #[test]
    fn every_cached_row_is_reachable_from_some_filter() {
        let families = [
            "task_queued",
            "task_started",
            "task_finished",
            "issue_created",
            "issue_updated",
            "issue_deleted",
            "comment_added",
            "mention",
            "something_a_newer_daemon_grew",
        ];
        for family in families {
            let row = InboxEntryRow {
                event: family.into(),
                ..entry("x", "a summary", false)
            };
            let reachable: Vec<&str> = InboxFilter::ALL
                .into_iter()
                .filter(|f| f.keeps(&row))
                .map(InboxFilter::label)
                .collect();
            assert!(
                reachable.contains(&"all"),
                "{family} is missing from `all`: {reachable:?}"
            );
            assert!(
                reachable.len() >= 2,
                "{family} is only reachable from `all`: {reachable:?}"
            );
        }
    }

    /// `runs` keeps the task rows and drops the issue rows; `issues` does the
    /// reverse; `asks` empties `recent` and leaves only the attention block.
    #[test]
    fn filters_partition_the_recent_rows() {
        let run = task_entry(
            "t-1",
            "task_finished",
            "Task finished (Success): t-1",
            NOW,
            false,
        );
        let issue = entry("issue", "New issue: Refactor API", false);
        let entries = vec![run.clone(), issue.clone()];

        let kept = |filter: InboxFilter| -> Vec<String> {
            ordered(&entries, filter, NOW).iter().map(|e| e.event.clone()).collect()
        };
        assert_eq!(kept(InboxFilter::All).len(), 2);
        assert_eq!(kept(InboxFilter::Runs), vec!["task_finished"]);
        assert_eq!(kept(InboxFilter::Issues), vec!["issue_created"]);
        assert!(kept(InboxFilter::Asks).is_empty());
    }

    /// Under `asks` the `recent` block is gone and the attention block owns the
    /// pane; under `runs` it is the other way round.
    #[test]
    fn the_active_filter_decides_which_blocks_paint() {
        let mut attention = ControlCenterState::default();
        attention.set_attention(&[attention_row(
            "ask-1",
            "ask_user_question",
            NOW,
            &ask_payload("Ship?", &[("yes", None)]),
        )]);
        let mut state = InboxState::from_snapshot(
            vec![entry("issue", "New issue: Refactor API", false)],
            1,
            "member:me".into(),
        );

        let paint = |state: &InboxState| {
            let mut buf = WireBuffer::new(80, 24);
            render_inbox(&mut buf, 80, 0, 20, state, &lookup(), &attention, NOW);
            (0..=20).map(|r| row_body(&buf, r, 80)).collect::<Vec<_>>().join("\n")
        };

        let all = paint(&state);
        assert!(all.contains("(all)"), "the active chip is marked: {all}");
        assert!(all.contains("needs you") && all.contains("recent"), "{all}");

        state.cycle_filter(); // asks
        let asks = paint(&state);
        assert!(asks.contains("(asks)"), "{asks}");
        assert!(asks.contains("needs you"), "{asks}");
        assert!(!asks.contains("recent"), "`asks` drops the recent block: {asks}");
        assert!(!asks.contains("Refactor API"), "{asks}");

        state.cycle_filter(); // runs
        let runs = paint(&state);
        assert!(runs.contains("(runs)"), "{runs}");
        assert!(!runs.contains("needs you"), "`runs` drops the attention block: {runs}");
        assert!(runs.contains("no notifications"), "no run rows cached: {runs}");
    }

    /// A refresh landing while the operator is filtered keeps the filter.
    ///
    /// MUTATION GUARD: rebuilding the state from the snapshot instead of
    /// replacing its rows resets the filter to `all` on every poll, which reads
    /// as the pane randomly un-filtering itself.
    #[test]
    fn a_refresh_keeps_the_operators_filter() {
        let mut state = InboxState::from_snapshot(vec![], 0, "member:me".into());
        state.cycle_filter();
        state.cycle_filter();
        assert_eq!(state.filter(), InboxFilter::Runs);
        state.replace_rows(
            vec![entry("issue", "New issue: Refactor API", false)],
            1,
            "member:me".into(),
        );
        assert_eq!(state.filter(), InboxFilter::Runs, "the filter survives");
        assert_eq!(state.entries().len(), 1, "the rows were replaced");
        assert_eq!(state.unread(), 1);
    }

    // --- crisp B1: the recomposed `recent` rows ----------------------------

    /// Crisp B1 + B3: a task row reads `<glyph> <age> <agent> <verb>  <HGR-n>
    /// <title>`, never the ULID the summary carries; the glyph and the verb both
    /// come from the vocabulary table, and the finished verb from the summary's
    /// `(Result)`.
    #[test]
    fn task_rows_read_as_glyph_agent_verb_issue_with_age() {
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
        render_inbox(&mut buf, 80, 0, 20, &state, &lookup(), &no_attention(), NOW);
        let r0 = row_body(&buf, RECENT_TOP, 80);
        let r1 = row_body(&buf, RECENT_TOP + 1, 80);
        assert!(r0.starts_with("● 2m"), "done glyph + age: {r0:?}");
        assert!(r0.contains("impl-1 done"), "agent + verb: {r0:?}");
        assert!(
            r0.contains("HGR-3 Add GET /api/version endpoint"),
            "issue: {r0:?}"
        );
        assert!(!r0.contains(task), "no ULID on the row: {r0:?}");
        assert!(r1.starts_with("◔ 4m"), "started reads as running: {r1:?}");
        assert!(r1.contains("impl-1 running"), "agent + verb: {r1:?}");
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
        render_inbox(&mut buf, 80, 0, 20, &state, &lookup(), &no_attention(), NOW);
        let rows: Vec<String> =
            (RECENT_TOP..RECENT_TOP + 5).map(|r| row_body(&buf, r, 80)).collect();
        assert!(
            rows[0].starts_with("✗ ") && rows[0].contains("run failed") && rows[0].contains("#ZZZZZZ"),
            "{:?}",
            rows[0]
        );
        assert!(
            rows[1].starts_with("+ ") && rows[1].contains("new issue") && rows[1].contains("HGR-3 Add GET"),
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

    /// An issue whose TITLE happens to contain `(Success)` is not a finished
    /// run: the event family decides, never the summary text.
    #[test]
    fn an_issue_titled_like_a_run_result_is_not_a_run() {
        let row = entry("issue", "New issue: Ship it (Success) banner", false);
        assert_eq!(task_run_state(&row), None);
        assert_eq!(row_glyph(&row).0, '+', "it is a new issue, not a done run");
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
        let ids: Vec<&str> = ordered(&entries, InboxFilter::All, NOW)
            .into_iter()
            .map(|e| e.subject_id.as_str())
            .collect();
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
        render_inbox(&mut buf, 60, 0, 20, &state, &InboxLookup::default(), &no_attention(), NOW);

        // Title row carries the unread badge.
        let title = row_text(&buf, 0, 60);
        assert!(title.starts_with("Inbox"), "title: {title:?}");
        assert!(title.contains("2 unread"), "badge shows count: {title:?}");

        let body = (RECENT_TOP..RECENT_TOP + 2)
            .map(|r| row_body(&buf, r, 60))
            .collect::<Vec<_>>()
            .join("\n");
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
    /// MUTATION GUARD: dropping the recipient from the header fails the first
    /// assertion — the screen would imply a workspace-wide feed while the data
    /// plane returns one actor's rows.
    #[test]
    fn header_names_the_recipient_and_only_their_entries_render() {
        let state = InboxState::from_snapshot(
            vec![entry("comment", "New comment on issue-1", false)],
            1,
            "member:me".into(),
        );
        let mut buf = WireBuffer::new(60, 24);
        render_inbox(&mut buf, 60, 0, 20, &state, &InboxLookup::default(), &no_attention(), NOW);

        let title = row_text(&buf, 0, 60);
        assert!(
            title.contains("member:me"),
            "the header names the recipient: {title:?}"
        );
        assert!(title.starts_with("Inbox"), "title: {title:?}");
        assert_eq!(state.recipient(), "member:me");

        // Only the one handed-in entry paints; no other actor's row appears.
        let rows: Vec<String> = (RECENT_TOP..20)
            .map(|r| row_body(&buf, r, 60))
            .filter(|r| !r.is_empty())
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "exactly the entries handed to the screen render: {rows:?}"
        );
        assert!(rows[0].contains("New comment on issue-1"), "{rows:?}");
    }

    #[test]
    fn unread_entries_render_amber_read_entries_white() {
        let state =
            InboxState::from_snapshot(vec![entry("task", "Task finished", true)], 0, String::new());
        let mut buf = WireBuffer::new(60, 24);
        render_inbox(&mut buf, 60, 0, 20, &state, &InboxLookup::default(), &no_attention(), NOW);

        // No badge when unread is zero.
        let title = row_text(&buf, 0, 60);
        assert!(!title.contains("unread"), "no badge when zero: {title:?}");

        // A read entry's summary glyph is soft-white, not unread-amber.
        let summary_cell = buf
            .cells
            .iter()
            .find(|(coord, c)| coord.y == RECENT_TOP && c.symbol == "T")
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
        render_inbox(&mut buf, 60, 0, 20, &state, &InboxLookup::default(), &no_attention(), NOW);
        assert!(
            row_body(&buf, RECENT_TOP, 60).contains("no notifications"),
            "empty placeholder: {:?}",
            row_body(&buf, RECENT_TOP, 60)
        );
        assert_eq!(state.unread(), 0);
    }

    /// The pane paints fleet-wide, session-originated text now, so a crafted
    /// payload must never reassemble into a live terminal control sequence.
    #[test]
    fn control_chars_in_session_text_are_sanitized_on_render() {
        let mut attention = ControlCenterState::default();
        attention.set_attention(&[attention_row(
            "idle",
            "waiting",
            NOW,
            "{\"kind\":\"IDLE\",\"context\":{\"last_assistant_text\":\"\u{1b}]52;c;AAAA\u{07}\"}}",
        )]);
        let state = InboxState::from_snapshot(
            vec![entry("issue", "New issue: \u{1b}]0;pwned\u{07}", false)],
            1,
            "member:me".into(),
        );
        let mut buf = WireBuffer::new(80, 24);
        render_inbox(&mut buf, 80, 0, 20, &state, &InboxLookup::default(), &attention, NOW);
        let has_control = buf.cells.iter().any(|(_, c)| c.symbol.chars().any(char::is_control));
        assert!(
            !has_control,
            "no control char may survive into a rendered cell"
        );
    }

    /// A pane too short for two blocks still paints the header and whatever
    /// fits, and never below `bottom`.
    #[test]
    fn a_short_pane_never_paints_past_its_bottom() {
        let mut attention = ControlCenterState::default();
        attention.set_attention(&[attention_row(
            "ask-1",
            "ask_user_question",
            NOW,
            &ask_payload("Ship?", &[("yes", None), ("no", None)]),
        )]);
        let state = InboxState::from_snapshot(
            vec![entry("issue", "New issue: Refactor API", false)],
            1,
            "member:me".into(),
        );
        for bottom in 0..8u16 {
            let mut buf = WireBuffer::new(80, 24);
            render_inbox(&mut buf, 80, 0, bottom, &state, &lookup(), &attention, NOW);
            let past = buf.cells.iter().filter(|(c, _)| c.y > bottom).count();
            assert_eq!(past, 0, "painted past row {bottom}");
        }
    }
}
