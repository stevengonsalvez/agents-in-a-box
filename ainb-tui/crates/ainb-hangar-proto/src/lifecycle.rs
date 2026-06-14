//! The canonical issue lifecycle: the five-status board vocabulary plus the
//! single state-to-column ordering both the daemon and the plugin map through
//! (63l.3).
//!
//! Before this module the issue board derived its columns ad-hoc: the daemon
//! queried a free-string `state` per-state, and the plugin's `IssueColumn`
//! bucketed three columns (`Todo` / `InProgress` / `Done`) from whatever string
//! it was handed. The redesign establishes FIVE canonical statuses —
//! `backlog`, `todo`, `in_progress`, `in_review`, `done` — as the lifecycle, and
//! this module is the **single source of truth** for that vocabulary and its
//! left-to-right column order.
//!
//! # Legacy tolerance
//!
//! The pre-redesign vocabulary (`open`, `closed`) and any unrecognised string
//! must never drop a row off the board. [`IssueLifecycle::for_state`] maps the
//! legacy `open` and every unknown token forward to [`IssueLifecycle::Todo`],
//! and the legacy `closed` to [`IssueLifecycle::Done`] — fail-visible, so a
//! daemon that has not yet remapped an `open` row still renders it under Todo.
//! Migration 0023 rewrites the *stored* legacy values forward; this helper keeps
//! the *display* path tolerant for any row that slips through (a stale snapshot,
//! a Beads-sync write that still speaks `open`).
//!
//! These are **pure data** — no `serde`, no host deps — so both the daemon
//! (`ISSUE_STATES`) and the plugin (`IssueColumn`) collapse onto the one
//! ordering rather than each maintaining its own.

/// The five canonical issue lifecycle statuses, in left-to-right board order
/// (`backlog` = column 0 … `done` = column 4).
///
/// The discriminant order IS the column order: [`IssueLifecycle::order`] returns
/// the 0-based index and [`IssueLifecycle::ALL`] lists them left-to-right, so a
/// caller never re-derives the ordering. The canonical wire token for each is
/// its [`IssueLifecycle::as_str`] (`snake_case`), the value the store persists
/// after migration 0023 and the daemon queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IssueLifecycle {
    /// Not yet triaged into active work — the leftmost column.
    Backlog,
    /// Triaged, ready to start (the canonical "not started" state; legacy
    /// `open` and any unknown token map here).
    Todo,
    /// Actively being worked.
    InProgress,
    /// Work done, awaiting review / merge.
    InReview,
    /// Terminal — closed / merged (legacy `closed` maps here).
    Done,
}

impl IssueLifecycle {
    /// The five statuses in left-to-right board order (`backlog` … `done`).
    pub const ALL: [Self; 5] = [
        Self::Backlog,
        Self::Todo,
        Self::InProgress,
        Self::InReview,
        Self::Done,
    ];

    /// The canonical wire token (`snake_case`) this status is stored / queried
    /// as — the value migration 0023 writes and the daemon lists by.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::InReview => "in_review",
            Self::Done => "done",
        }
    }

    /// The human-readable column header label (without any count suffix).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Backlog => "Backlog",
            Self::Todo => "Todo",
            Self::InProgress => "In Progress",
            Self::InReview => "In Review",
            Self::Done => "Done",
        }
    }

    /// The 0-based left-to-right column index (`backlog` = 0 … `done` = 4).
    #[must_use]
    pub const fn order(self) -> usize {
        match self {
            Self::Backlog => 0,
            Self::Todo => 1,
            Self::InProgress => 2,
            Self::InReview => 3,
            Self::Done => 4,
        }
    }

    /// Bucket a wire `state` string into its canonical column, tolerant of the
    /// legacy vocabulary and any unknown token.
    ///
    /// The canonical tokens map to themselves; the legacy `open` and EVERY
    /// unrecognised string map forward to [`IssueLifecycle::Todo`] (fail-visible,
    /// never fail-hidden), and the legacy `closed` to [`IssueLifecycle::Done`].
    /// This is the one mapping both the daemon and the plugin call, so the board
    /// can never disagree on which column a row belongs to.
    #[must_use]
    pub fn for_state(state: &str) -> Self {
        match state {
            "backlog" => Self::Backlog,
            "in_progress" => Self::InProgress,
            "in_review" => Self::InReview,
            // Legacy `closed` is terminal; canonical `done` is terminal.
            "done" | "closed" => Self::Done,
            // Canonical `todo`, legacy `open`, and any unknown token are
            // fail-visible under Todo so a row never silently vanishes.
            _ => Self::Todo,
        }
    }

    /// The 0-based column index a wire `state` string sorts into, via
    /// [`Self::for_state`] then [`Self::order`].
    ///
    /// The convenience the daemon / plugin reach for when they only need the
    /// column index (e.g. to order columns left-to-right) rather than the typed
    /// status.
    #[must_use]
    pub fn order_of_state(state: &str) -> usize {
        Self::for_state(state).order()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each canonical status maps to its own column at the declared 0..4 index,
    /// and the wire token round-trips through `for_state`.
    #[test]
    fn canonical_states_map_to_their_own_column_in_order() {
        let expected = [
            (IssueLifecycle::Backlog, "backlog", 0),
            (IssueLifecycle::Todo, "todo", 1),
            (IssueLifecycle::InProgress, "in_progress", 2),
            (IssueLifecycle::InReview, "in_review", 3),
            (IssueLifecycle::Done, "done", 4),
        ];
        for (status, token, order) in expected {
            assert_eq!(status.as_str(), token, "canonical wire token");
            assert_eq!(status.order(), order, "0-based column order for {token}");
            assert_eq!(
                IssueLifecycle::for_state(token),
                status,
                "wire token {token} re-parses to its status"
            );
            assert_eq!(
                IssueLifecycle::order_of_state(token),
                order,
                "order_of_state({token})"
            );
        }
    }

    /// `ALL` lists the five statuses left-to-right with strictly increasing
    /// order indices.
    #[test]
    fn all_is_left_to_right_and_complete() {
        assert_eq!(
            IssueLifecycle::ALL.len(),
            5,
            "exactly five canonical columns"
        );
        for (i, status) in IssueLifecycle::ALL.iter().enumerate() {
            assert_eq!(status.order(), i, "ALL[{i}] sits at column {i}");
        }
    }

    /// Legacy `open` maps forward to Todo and `closed` forward to Done, so an
    /// un-remapped row still lands in a real column.
    #[test]
    fn legacy_states_map_forward() {
        assert_eq!(
            IssueLifecycle::for_state("open"),
            IssueLifecycle::Todo,
            "legacy open -> Todo"
        );
        assert_eq!(
            IssueLifecycle::for_state("closed"),
            IssueLifecycle::Done,
            "legacy closed -> Done"
        );
    }

    /// An unknown token is fail-visible under Todo, never dropped off the board.
    #[test]
    fn unknown_state_falls_into_todo() {
        assert_eq!(
            IssueLifecycle::for_state("weird-status"),
            IssueLifecycle::Todo,
            "unknown -> Todo (fail-visible)"
        );
        assert_eq!(IssueLifecycle::order_of_state("weird-status"), 1);
    }
}
