//! Crisp B2 §2.1 — the ONE status vocabulary every hangar screen renders from.
//!
//! The audit found four vocabularies for one idea: the task FSM writes
//! `running/done/failed/cancelled`, the inbox paints `Success/Failure/Cancelled`,
//! the usage dashboard `success/failed`, and the Fleet pane both `RUN/DONE` and
//! `Running/Completed`. Same run, four words, so nothing on screen can be
//! compared to anything else.
//!
//! ```text
//! run/task    ○ queued   ◔ running   ● done   ✗ failed   ⊘ cancelled
//! issue       backlog · todo · in progress · in review · done · blocked · cancelled
//! attention   ASK · ERR · IDLE · WAIT
//! fleet lens  needs input · running · idle · done
//! ```
//!
//! Rules: **lowercase everywhere except the four attention codes**, and one glyph
//! per token, shared. Never `Success`, never `DONE`, never `Completed`.
//!
//! Two total mappings turn what the daemon sends into this vocabulary —
//! [`RunState::of`] over the CHECK'd task FSM and [`AttentionKind::of_kind`] over
//! the attention request families — and each token carries its own glyph, so a
//! screen can never pair the right word with the wrong mark.
//!
//! This module MAPS the daemon's three stores onto one vocabulary; it does not
//! merge them (PLAN.md "do not attempt in track B" #1). The stores collapse in
//! the spine work, and this table is what the screens read until they do.

use ainb_hangar_core::task_status::TaskStatus;
use ainb_hangar_proto::lifecycle::IssueLifecycle;

/// The five run/task words — the CHECK'd `agent_task_queue` FSM as a human reads it.
///
/// `dispatched` folds into `queued`: a task handed to a runtime that has not
/// reported back is, to the operator, still waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Enqueued or dispatched, not yet executing.
    Queued,
    /// Actively executing.
    Running,
    /// Finished successfully.
    Done,
    /// Finished with an error.
    Failed,
    /// Cancelled before it finished.
    Cancelled,
}

impl RunState {
    /// TOTAL over the task FSM: every [`TaskStatus`] has a word here, so a new
    /// FSM variant fails to compile rather than rendering an old word.
    #[must_use]
    pub const fn of(status: TaskStatus) -> Self {
        match status {
            TaskStatus::Queued | TaskStatus::Dispatched => Self::Queued,
            TaskStatus::Running => Self::Running,
            TaskStatus::Done => Self::Done,
            TaskStatus::Failed => Self::Failed,
            TaskStatus::Cancelled => Self::Cancelled,
        }
    }

    /// The state a wire status token names, `None` for a token outside the FSM.
    ///
    /// Deliberately NOT total over strings: a status the daemon grew after this
    /// build should read as "no run chip", not as a confident `queued`. The
    /// caller decides what an unknown run looks like.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        TaskStatus::parse(token).map(Self::of)
    }

    /// The lowercase word this state paints as.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// The glyph shared by every surface that paints this state.
    #[must_use]
    pub const fn glyph(self) -> char {
        match self {
            Self::Queued => '○',
            Self::Running => '◔',
            Self::Done => '●',
            Self::Failed => '✗',
            Self::Cancelled => '⊘',
        }
    }
}

/// The four attention codes — the ONLY uppercase words in the vocabulary, short
/// enough to sit on a card footer beside an agent name and an age.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionKind {
    /// A question is waiting for an answer (`ask_user_question`,
    /// `codex_request_user`, `approval`).
    Ask,
    /// The run hit an error a human must look at.
    Err,
    /// The session went idle with nothing to do.
    Idle,
    /// Blocked on something else, waiting.
    Wait,
}

impl AttentionKind {
    /// TOTAL over the attention `kind` wire tokens.
    ///
    /// An unrecognised family reads as [`Self::Wait`] — "something wants you and
    /// we do not know what" — never as a silent drop: an attention row that
    /// paints nothing is a human waiting on a screen that says all is well.
    #[must_use]
    pub fn of_kind(kind: &str) -> Self {
        match kind.trim().to_ascii_lowercase().as_str() {
            "ask_user_question" | "codex_request_user" | "approval" => Self::Ask,
            "error" | "escalation" => Self::Err,
            "idle" => Self::Idle,
            _ => Self::Wait,
        }
    }

    /// The uppercase code this kind paints as.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Ask => "ASK",
            Self::Err => "ERR",
            Self::Idle => "IDLE",
            Self::Wait => "WAIT",
        }
    }

    /// The attention dot every code shares — the colour, not the glyph, tells
    /// them apart, so a row of attention items reads as one column of dots.
    ///
    /// An associated const rather than a method, because it does not vary by
    /// kind and a `kind.glyph()` call implies it might.
    pub const GLYPH: char = '●';
}

/// The issue vocabulary as a lowercase word (`in progress`, never `In Progress`).
///
/// The board COLUMN HEADERS keep their title-case
/// [`label`](IssueLifecycle::label) — they are proper nouns of the board, and
/// three tripwires assert them — so this is the word for prose: a status inside a
/// sentence, a filter chip, a card line.
#[must_use]
pub const fn issue_word(status: IssueLifecycle) -> &'static str {
    match status {
        IssueLifecycle::Backlog => "backlog",
        IssueLifecycle::Todo => "todo",
        IssueLifecycle::InProgress => "in progress",
        IssueLifecycle::InReview => "in review",
        IssueLifecycle::Done => "done",
        IssueLifecycle::Blocked => "blocked",
        IssueLifecycle::Cancelled => "cancelled",
    }
}

/// The fleet lens word for "a human must answer this session".
///
/// The other three lens words are [`RunState::Running`] and [`RunState::Done`]
/// (shared with the run vocabulary — the same run, the same word) and
/// [`FLEET_IDLE`].
pub const FLEET_NEEDS_INPUT: &str = "needs input";

/// The fleet lens word for a session with nothing to do.
///
/// Lowercase, unlike the [`AttentionKind::Idle`] code: the lens is a filter over
/// sessions, the code is a flag on one row that needs a human.
pub const FLEET_IDLE: &str = "idle";

#[cfg(test)]
mod tests {
    use super::*;

    /// Every task-FSM status maps to a run word, `dispatched` collapsing into
    /// `queued` so the five-word vocabulary really covers the six-state FSM.
    #[test]
    fn every_task_status_has_a_run_word() {
        let expected = [
            (TaskStatus::Queued, "queued"),
            (TaskStatus::Dispatched, "queued"),
            (TaskStatus::Running, "running"),
            (TaskStatus::Done, "done"),
            (TaskStatus::Failed, "failed"),
            (TaskStatus::Cancelled, "cancelled"),
        ];
        for (status, word) in expected {
            assert_eq!(RunState::of(status).word(), word, "{status:?}");
        }
        // And the mapping is exhaustive over the FSM, not just over the six the
        // table lists: a new variant must appear here too.
        assert_eq!(TaskStatus::ALL.len(), expected.len());
    }

    /// The wire tokens the daemon writes parse to their word; anything else is
    /// `None` rather than a confident wrong word.
    #[test]
    fn parse_covers_the_fsm_and_rejects_the_rest() {
        for status in TaskStatus::ALL {
            assert_eq!(
                RunState::parse(status.as_str()),
                Some(RunState::of(status)),
                "wire token {}",
                status.as_str()
            );
        }
        for outside in ["Success", "success", "DONE", "in_progress", "", "runnning"] {
            assert_eq!(
                RunState::parse(outside),
                None,
                "{outside:?} is not a task status"
            );
        }
    }

    /// The lowercase rule, mechanically: every run, issue and fleet word is
    /// lowercase, and every attention code is uppercase. `Success` / `DONE` /
    /// `Completed` cannot come back without failing here.
    #[test]
    fn only_the_attention_codes_are_uppercase() {
        for status in TaskStatus::ALL {
            let word = RunState::of(status).word();
            assert_eq!(word, word.to_lowercase(), "run word {word:?}");
        }
        for status in IssueLifecycle::ALL {
            let word = issue_word(status);
            assert_eq!(word, word.to_lowercase(), "issue word {word:?}");
        }
        for word in [FLEET_NEEDS_INPUT, FLEET_IDLE] {
            assert_eq!(word, word.to_lowercase(), "fleet word {word:?}");
        }
        for kind in ["ask_user_question", "error", "idle", "waiting"] {
            let code = AttentionKind::of_kind(kind).code();
            assert_eq!(code, code.to_uppercase(), "attention code {code:?}");
            assert!(
                (3..=4).contains(&code.chars().count()),
                "attention code {code:?} must be 3-4 chars to fit a card footer"
            );
        }
    }

    /// Each of the five run states owns its own glyph — a shared glyph would make
    /// two states look identical on a card footer.
    #[test]
    fn run_glyphs_are_distinct() {
        let glyphs: Vec<char> = TaskStatus::ALL
            .into_iter()
            .map(|s| RunState::of(s).glyph())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(glyphs.len(), 5, "five distinct run glyphs, got {glyphs:?}");
        // The card footer shares its row with nothing else, but the ISSUE
        // PROPERTIES row paints `◆ Sprint: S2` (a tripwire asserts it), so no run
        // glyph may be that diamond.
        assert!(!glyphs.contains(&'◆'), "run glyph collides with ◆");
    }

    /// The attention families map onto the four codes, and an unknown family is
    /// WAIT (fail-visible), never dropped.
    #[test]
    fn attention_families_map_to_codes() {
        for (kind, code) in [
            ("ask_user_question", "ASK"),
            ("codex_request_user", "ASK"),
            ("approval", "ASK"),
            ("error", "ERR"),
            ("escalation", "ERR"),
            ("idle", "IDLE"),
            ("waiting", "WAIT"),
            ("something_new_from_a_newer_daemon", "WAIT"),
            ("", "WAIT"),
        ] {
            assert_eq!(AttentionKind::of_kind(kind).code(), code, "kind {kind:?}");
        }
        // Case and padding on the wire are not a different family.
        assert_eq!(AttentionKind::of_kind("  ERROR  "), AttentionKind::Err);
    }

    /// Every issue status has a lowercase word, spelled with spaces rather than
    /// the wire's underscores.
    #[test]
    fn every_issue_status_has_a_word() {
        assert_eq!(issue_word(IssueLifecycle::InProgress), "in progress");
        assert_eq!(issue_word(IssueLifecycle::InReview), "in review");
        for status in IssueLifecycle::ALL {
            assert!(
                !issue_word(status).contains('_'),
                "{} keeps a wire underscore",
                status.as_str()
            );
        }
    }
}
