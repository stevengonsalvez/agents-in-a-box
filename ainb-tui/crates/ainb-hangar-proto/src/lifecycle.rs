//! The canonical issue lifecycle: the seven-status board vocabulary plus the
//! single state-to-column ordering both the daemon and the plugin map through
//! (63l.3).
//!
//! Before this module the issue board derived its columns ad-hoc: the daemon
//! queried a free-string `state` per-state, and the plugin's `IssueColumn`
//! bucketed three columns (`Todo` / `InProgress` / `Done`) from whatever string
//! it was handed. The redesign establishes SEVEN canonical statuses —
//! `backlog`, `todo`, `in_progress`, `in_review`, `done`, `blocked`,
//! `cancelled` — as the lifecycle, and this module is the **single source of
//! truth** for that vocabulary and its left-to-right column order.
//!
//! `blocked` and `cancelled` are APPENDED (columns 5 and 6): every pre-existing
//! 0..4 index is untouched, so per-column arrays, hit-tests, and facet tests
//! keyed on a column index keep working.
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

/// The seven canonical issue lifecycle statuses, in left-to-right board order
/// (`backlog` = column 0 … `cancelled` = column 6).
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
    /// Work cannot proceed — a human/agent annotation, NOT the dependency gate
    /// (that is `card_dependency`). Appended as column 5 so the 0..4 indices are
    /// stable; see [`Self::advance_rank`] for why it is not "past done".
    Blocked,
    /// Terminal — abandoned without completing. Terminal alongside
    /// [`Self::Done`] ([`Self::is_terminal`]).
    Cancelled,
}

impl IssueLifecycle {
    /// The seven statuses in left-to-right board order (`backlog` …
    /// `cancelled`).
    pub const ALL: [Self; 7] = [
        Self::Backlog,
        Self::Todo,
        Self::InProgress,
        Self::InReview,
        Self::Done,
        Self::Blocked,
        Self::Cancelled,
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
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
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
            Self::Blocked => "Blocked",
            Self::Cancelled => "Cancelled",
        }
    }

    /// The 0-based left-to-right column index (`backlog` = 0 … `cancelled` = 6).
    #[must_use]
    pub const fn order(self) -> usize {
        match self {
            Self::Backlog => 0,
            Self::Todo => 1,
            Self::InProgress => 2,
            Self::InReview => 3,
            Self::Done => 4,
            Self::Blocked => 5,
            Self::Cancelled => 6,
        }
    }

    /// Whether this status is TERMINAL — the issue will never complete further
    /// work under it.
    ///
    /// `done` (completed) and `cancelled` (abandoned) both qualify: a cancelled
    /// sibling will never complete, so it must not hold a parent stage open, and
    /// a terminal issue is never revived by a run transition.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }

    /// The WORKFLOW-PROGRESS rank — deliberately NOT [`Self::order`].
    ///
    /// [`Self::order`] is the *board column* order, where `blocked` and
    /// `cancelled` are APPENDED to the right of `done` (columns 5 and 6) so the
    /// pre-existing indices stay stable. Progress is a different question: a
    /// `blocked` issue is not "past done" — it is a `todo` that cannot start
    /// yet, so it ranks WITH `todo` (1). `cancelled` ranks with `done` (4) as a
    /// terminal outcome.
    ///
    /// Any advance-only guard must compare THIS, not `order()`: comparing column
    /// order would freeze a `blocked` issue forever, because a later `running`
    /// (rank 2) or `done` (rank 4) transition would read as "behind" column 5.
    #[must_use]
    pub const fn advance_rank(self) -> usize {
        match self {
            Self::Backlog => 0,
            // A blocked issue is a not-started issue, not a post-done one.
            Self::Blocked | Self::Todo => 1,
            Self::InProgress => 2,
            Self::InReview => 3,
            Self::Done | Self::Cancelled => 4,
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
            "blocked" => Self::Blocked,
            "cancelled" => Self::Cancelled,
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

    /// STRICT parse of a canonical wire token — the WRITE-boundary validator.
    ///
    /// Unlike [`Self::for_state`] (the tolerant DISPLAY mapper, which never
    /// drops a row), this returns `None` for the legacy vocabulary
    /// (`open` / `closed`) and for every unrecognised token, so a typo at an RPC
    /// or CLI boundary is a clean rejection instead of a phantom row that falls
    /// into Todo forever. The two mappings are deliberately NOT merged.
    #[must_use]
    pub fn parse_canonical(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == s)
    }

    /// The canonical vocabulary as a human-readable, comma-separated list — the
    /// tail of every "invalid state" rejection message.
    #[must_use]
    pub fn canonical_list() -> String {
        Self::ALL.map(Self::as_str).join(", ")
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
            (IssueLifecycle::Blocked, "blocked", 5),
            (IssueLifecycle::Cancelled, "cancelled", 6),
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
            7,
            "exactly seven canonical columns"
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

    /// `done` and `cancelled` are terminal; nothing else is (a blocked issue is
    /// still expected to move).
    #[test]
    fn terminal_is_done_and_cancelled_only() {
        for status in IssueLifecycle::ALL {
            let expected = matches!(status, IssueLifecycle::Done | IssueLifecycle::Cancelled);
            assert_eq!(
                status.is_terminal(),
                expected,
                "{} terminal?",
                status.as_str()
            );
        }
    }

    /// Workflow progress is ranked separately from board-column order: `blocked`
    /// sits at column 5 but ranks WITH `todo`, and `cancelled` ranks with `done`.
    #[test]
    fn advance_rank_is_progress_not_column_order() {
        assert_eq!(
            IssueLifecycle::Blocked.advance_rank(),
            IssueLifecycle::Todo.advance_rank(),
            "blocked ranks with todo"
        );
        assert!(
            IssueLifecycle::Blocked.advance_rank() < IssueLifecycle::InProgress.advance_rank(),
            "blocked is behind in_progress, so a run still promotes it"
        );
        assert_eq!(
            IssueLifecycle::Cancelled.advance_rank(),
            IssueLifecycle::Done.advance_rank(),
            "cancelled ranks with done"
        );
        assert!(
            IssueLifecycle::Blocked.order() > IssueLifecycle::Done.order(),
            "column order really does put blocked to the right of done"
        );
    }

    /// The strict parser accepts exactly the seven canonical tokens and rejects
    /// the legacy vocabulary + garbage (the write-boundary contract).
    #[test]
    fn parse_canonical_is_strict() {
        for status in IssueLifecycle::ALL {
            assert_eq!(
                IssueLifecycle::parse_canonical(status.as_str()),
                Some(status),
                "canonical token {} parses",
                status.as_str()
            );
        }
        for rejected in ["open", "closed", "blockd", "cancelled_", "", "Done"] {
            assert_eq!(
                IssueLifecycle::parse_canonical(rejected),
                None,
                "{rejected:?} is not canonical"
            );
        }
    }

    /// The rejection message enumerates every canonical token.
    #[test]
    fn canonical_list_enumerates_every_status() {
        let list = IssueLifecycle::canonical_list();
        for status in IssueLifecycle::ALL {
            assert!(
                list.contains(status.as_str()),
                "canonical_list() mentions {}",
                status.as_str()
            );
        }
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
