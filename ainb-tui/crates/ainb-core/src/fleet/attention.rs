// ABOUTME: The one attention vocabulary the sessions screen paints and answers.
//
// The TUI used to carry five competing "an agent needs you" surfaces, each with
// its own words for the same four states. This module is the single vocabulary
// they collapse onto: ASK, APPROVE, ERR, DONE — the same four-code collapse the
// hangar Inbox uses, so the two surfaces cannot drift apart.
//
// PURE. No IO, no clock, no daemon. The clock arrives as `now_ms` and the rows
// arrive already read, which is what lets the precedence rule, the header count
// and the age format be unit-tested without a store or a socket.

use std::fmt;

/// One attention state a session row can be in.
///
/// Ordering is PRECEDENCE, tightest first: a row carrying several states paints
/// them in this order (spec: "both chips shown, ASK first"). `Ord` is derived
/// from the variant order deliberately — sorting a chip list sorts it into the
/// order it renders in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AttentionKind {
    /// A structured question is waiting on a human. Blocks the agent.
    Ask,
    /// A permission / approval request is waiting on a human. Blocks the agent.
    Approve,
    /// The session failed and a human has to see it. Does NOT block a turn —
    /// nothing is parked waiting for an answer — so it is not counted in the
    /// header badge.
    Err,
    /// The session finished. Informational.
    Done,
}

impl AttentionKind {
    /// The chip word. Never abbreviated, at any terminal width: an abbreviated
    /// chip is a chip an operator has to decode, and these four words are the
    /// whole vocabulary.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ask => "ASK",
            Self::Approve => "APPROVE",
            Self::Err => "ERR",
            Self::Done => "DONE",
        }
    }

    /// Whether this state is BLOCKING an agent — the header badge's question.
    ///
    /// The header block ("what is open") and the badge ("what is blocking an
    /// agent") are deliberately different counts, mirroring the hangar Inbox.
    /// `ERR` and `DONE` are open states that block nobody.
    #[must_use]
    pub const fn blocks(self) -> bool {
        matches!(self, Self::Ask | Self::Approve)
    }
}

impl fmt::Display for AttentionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Where an attention state was observed. The daemon wins over local producers
/// while it is up (spec: "daemon row wins while the daemon is up"), and the
/// source is carried so the merge can say WHY a row looks the way it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttentionSource {
    /// Read from the local notifyd notifications store or the session's own
    /// status. Always available, daemon up or down.
    Local,
    /// Read from the hangar daemon's `attention/list`.
    Daemon,
}

/// One live attention state on one session row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionAttention {
    /// What the session needs.
    pub kind: AttentionKind,
    /// Epoch-ms the state was raised. Drives the age shown beside the chip.
    ///
    /// Taken from the producer's own timestamp whenever it has one (a notifyd
    /// row's `ts`, a daemon row's `created_at`) so the age survives a TUI
    /// restart and reads as the TRUE age, not "how long this process has known".
    pub since_ms: i64,
    /// Which producer this came from.
    pub source: AttentionSource,
}

impl SessionAttention {
    /// A locally-observed state.
    #[must_use]
    pub const fn local(kind: AttentionKind, since_ms: i64) -> Self {
        Self {
            kind,
            since_ms,
            source: AttentionSource::Local,
        }
    }

    /// A daemon-observed state.
    #[must_use]
    pub const fn daemon(kind: AttentionKind, since_ms: i64) -> Self {
        Self {
            kind,
            since_ms,
            source: AttentionSource::Daemon,
        }
    }
}

/// Sort a row's states into render precedence and drop duplicates of one kind,
/// keeping the OLDEST occurrence of each.
///
/// Oldest wins because the age beside a chip answers "how long has this been
/// waiting", and a producer that re-raises the same open state (notifyd
/// re-classifying the same pane, the daemon re-listing an unanswered row) must
/// not reset that clock back to zero every refresh.
pub fn normalise(mut chips: Vec<SessionAttention>) -> Vec<SessionAttention> {
    chips.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.since_ms.cmp(&b.since_ms)));
    chips.dedup_by_key(|chip| chip.kind);
    chips
}

/// How many session rows are BLOCKING an agent — the header badge's number.
///
/// Counts ROWS, not chips: a row carrying both an ASK and an APPROVE is one
/// session needing one human, and "2 need you" has to mean two sessions or the
/// badge lies about how much work is waiting.
pub fn needs_you_count<'a, I>(rows: I) -> usize
where
    I: IntoIterator<Item = &'a [SessionAttention]>,
{
    rows.into_iter().filter(|chips| chips.iter().any(|chip| chip.kind.blocks())).count()
}

/// Render an age as the shortest unambiguous form: `40s`, `9m`, `3h`, `2d`.
///
/// Saturating and monotone: a `since_ms` in the future (clock skew between the
/// daemon's clock and this host's) reads as `0s` rather than wrapping into a
/// nonsense age.
#[must_use]
pub fn format_age(now_ms: i64, since_ms: i64) -> String {
    let secs = now_ms.saturating_sub(since_ms).max(0) / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ask_and_approve_block() {
        assert!(AttentionKind::Ask.blocks());
        assert!(AttentionKind::Approve.blocks());
        assert!(!AttentionKind::Err.blocks());
        assert!(!AttentionKind::Done.blocks());
    }

    #[test]
    fn chips_render_ask_before_err_on_one_row() {
        // Spec edge case: "ASK arrives while ERR is open -> both chips shown,
        // ASK first".
        let chips = normalise(vec![
            SessionAttention::local(AttentionKind::Err, 1_000),
            SessionAttention::local(AttentionKind::Ask, 9_000),
        ]);
        assert_eq!(
            chips.iter().map(|c| c.kind).collect::<Vec<_>>(),
            vec![AttentionKind::Ask, AttentionKind::Err]
        );
    }

    #[test]
    fn re_raising_one_kind_does_not_reset_its_age() {
        let chips = normalise(vec![
            SessionAttention::local(AttentionKind::Ask, 9_000),
            SessionAttention::local(AttentionKind::Ask, 1_000),
        ]);
        assert_eq!(chips.len(), 1);
        assert_eq!(chips[0].since_ms, 1_000, "oldest occurrence must win");
    }

    #[test]
    fn header_counts_blocking_rows_not_chips() {
        let ask = [SessionAttention::local(AttentionKind::Ask, 0)];
        let err = [SessionAttention::local(AttentionKind::Err, 0)];
        let approve = [SessionAttention::local(AttentionKind::Approve, 0)];
        let done = [SessionAttention::local(AttentionKind::Done, 0)];
        let quiet: [SessionAttention; 0] = [];
        // The spec's own left-pane mock: ASK, ERR, APPROVE, DONE -> "2 need you".
        assert_eq!(
            needs_you_count([
                ask.as_slice(),
                err.as_slice(),
                approve.as_slice(),
                done.as_slice(),
                quiet.as_slice(),
            ]),
            2
        );
    }

    #[test]
    fn a_row_blocking_twice_still_counts_once() {
        let both = [
            SessionAttention::local(AttentionKind::Ask, 0),
            SessionAttention::local(AttentionKind::Approve, 0),
        ];
        assert_eq!(needs_you_count([both.as_slice()]), 1);
    }

    #[test]
    fn age_shortens_by_magnitude() {
        assert_eq!(format_age(40_000, 0), "40s");
        assert_eq!(format_age(59_999, 0), "59s");
        assert_eq!(format_age(60_000, 0), "1m");
        assert_eq!(format_age(9 * 60_000, 0), "9m");
        assert_eq!(format_age(3 * 3_600_000, 0), "3h");
        assert_eq!(format_age(2 * 86_400_000, 0), "2d");
    }

    #[test]
    fn a_future_timestamp_reads_as_zero_not_a_wrapped_age() {
        assert_eq!(format_age(0, 10_000), "0s");
    }

    #[test]
    fn every_chip_word_is_spelled_out() {
        for kind in [
            AttentionKind::Ask,
            AttentionKind::Approve,
            AttentionKind::Err,
            AttentionKind::Done,
        ] {
            let label = kind.label();
            assert!(
                label.len() >= 3 && label.chars().all(|c| c.is_ascii_uppercase()),
                "chip words never abbreviate: {label}"
            );
        }
    }
}
