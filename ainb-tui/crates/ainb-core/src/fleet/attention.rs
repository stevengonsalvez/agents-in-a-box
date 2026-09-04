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

/// Why a row cannot be answered from here.
///
/// Always carries a REASON. "Not answerable" rendered as a greyed chip with no
/// explanation is the silent no-op the spec forbids: the operator is looking at
/// something that needs them and has no way to learn why the surface will not
/// take their answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unanswerable {
    /// The row came from the daemon, and the daemon has since gone away, so the
    /// `attention/answer` call that would deliver the answer is unavailable.
    DaemonGone,
    /// The session has no live pane to type into and no daemon row to answer
    /// through — nothing to deliver an answer over at all.
    NoTransport,
    /// The state is informational. `DONE` and `ERR` are not questions; there is
    /// nothing to answer.
    NotAQuestion,
}

impl Unanswerable {
    /// The sentence the `ask` tab shows in place of a composer.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::DaemonGone => {
                "the hangar daemon is not reachable, so attention/answer cannot deliver this"
            }
            Self::NoTransport => "this session has no live pane and no daemon route to answer over",
            Self::NotAQuestion => "nothing is waiting on an answer here",
        }
    }
}

/// How an answer to this row would be delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answerable {
    /// Through the daemon's `attention/answer`, targeting this attention id.
    /// Unambiguous by construction: the id names exactly one open row, so the
    /// cwd-ambiguity refusal the phone reply path needs does not apply here.
    Daemon {
        /// The `attention/answer` target.
        attention_id: String,
    },
    /// By typing into the session's own tmux pane, through the one verified
    /// send path. Works with the daemon down, which is the whole point.
    Tmux {
        /// The pane to send into.
        tmux_session: String,
    },
    /// Through notifyd's approve broker, which is where a `PermissionRequest`
    /// hook is parked.
    ///
    /// A permission request is NOT answerable by typing: the hook is blocked in
    /// `client_await` and nothing it reads comes from the pane. The broker is
    /// local and independent of the hangar daemon, so this route survives the
    /// daemon being down — which is the case it exists for.
    Broker {
        /// The provider session id the waiter is parked under.
        session_id: String,
    },
    /// Not from here, and this is why.
    No(Unanswerable),
}

impl Answerable {
    /// Whether an answer can be delivered at all.
    #[must_use]
    pub const fn is_answerable(&self) -> bool {
        !matches!(self, Self::No(_))
    }

    /// Why not, or `None` when it can be answered.
    #[must_use]
    pub const fn refusal(&self) -> Option<&'static str> {
        match self {
            Self::No(reason) => Some(reason.reason()),
            _ => None,
        }
    }
}

/// The two answers a bare permission request takes.
///
/// Synthesised for an APPROVE that arrived with no structured options, because
/// those ARE its only two answers and an empty option list would leave the
/// operator typing free text at a hook that reads none.
#[must_use]
pub fn approval_options() -> Vec<AttentionOption> {
    vec![
        AttentionOption {
            label: APPROVE_LABEL.to_string(),
            description: "let the tool call proceed".to_string(),
        },
        AttentionOption {
            label: DENY_LABEL.to_string(),
            description: "block it".to_string(),
        },
    ]
}

/// The label that means "allow". Matched on the way back out, so the word the
/// operator picked and the decision the broker receives cannot drift.
pub const APPROVE_LABEL: &str = "approve";

/// The label that means "block".
pub const DENY_LABEL: &str = "deny";

/// One structured option an ASK offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionOption {
    /// The label the operator picks and the text delivered as the answer.
    pub label: String,
    /// The option's own explanation, or empty.
    pub description: String,
}

/// One live attention state on one session row.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// The one-line question or reason the `ask` tab leads with, when the
    /// producer supplied one.
    pub detail: Option<String>,
    /// Structured answer options. EMPTY unless the producer supplied a
    /// structured request — a free-text composer, not a zero-option list.
    pub options: Vec<AttentionOption>,
    /// How an answer would be delivered, or why it cannot be.
    pub answerable: Answerable,
}

impl SessionAttention {
    /// A locally-observed state, not yet routed.
    ///
    /// Answerability is resolved later, against the session the row lands on:
    /// only the session row knows whether it has a live pane.
    #[must_use]
    pub const fn local(kind: AttentionKind, since_ms: i64) -> Self {
        Self {
            kind,
            since_ms,
            source: AttentionSource::Local,
            detail: None,
            options: Vec::new(),
            answerable: Answerable::No(Unanswerable::NoTransport),
        }
    }

    /// A daemon-observed state, answerable through its attention id.
    #[must_use]
    pub fn daemon(kind: AttentionKind, since_ms: i64, attention_id: String) -> Self {
        Self {
            kind,
            since_ms,
            source: AttentionSource::Daemon,
            detail: None,
            options: Vec::new(),
            answerable: Answerable::Daemon { attention_id },
        }
    }

    /// Attach the one-line question the `ask` tab leads with.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        self.detail = (!detail.trim().is_empty()).then_some(detail);
        self
    }

    /// Attach structured options.
    #[must_use]
    pub fn with_options(mut self, options: Vec<AttentionOption>) -> Self {
        self.options = options;
        self
    }

    /// Route this row over the session's own pane.
    #[must_use]
    pub fn over_tmux(mut self, tmux_session: impl Into<String>) -> Self {
        self.answerable = Answerable::Tmux {
            tmux_session: tmux_session.into(),
        };
        self
    }

    /// Refuse this row, with a reason.
    #[must_use]
    pub fn unanswerable(mut self, why: Unanswerable) -> Self {
        self.answerable = Answerable::No(why);
        self
    }
}

/// Sort a row's states into render precedence and collapse duplicates of one
/// kind down to the single row that should represent it.
///
/// Two rules, in order:
///
/// 1. **The daemon wins while the daemon is up.** Both producers watch the same
///    session, so an ASK usually arrives twice. The daemon's row is the one that
///    carries an attention id, the structured options and the authoritative
///    answered/open state, so it is the one an operator can act on. The local
///    row is not merged into it (a half-daemon, half-local row would be a state
///    neither producer ever reported) — it is dropped, and it comes back on its
///    own the moment the daemon stops answering.
/// 2. **Otherwise the oldest wins.** The age beside a chip answers "how long has
///    this been waiting", so a producer re-raising the same still-open state
///    (notifyd re-classifying an unchanged pane, the daemon re-listing an
///    unanswered row) must not reset that clock to zero on every refresh.
pub fn normalise(mut chips: Vec<SessionAttention>) -> Vec<SessionAttention> {
    chips.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            // `Daemon` sorts before `Local` so `dedup_by_key` (which keeps the
            // FIRST of each run) keeps the daemon row.
            .then_with(|| daemon_first(a.source).cmp(&daemon_first(b.source)))
            .then(a.since_ms.cmp(&b.since_ms))
    });
    chips.dedup_by(|a, b| a.kind == b.kind);
    chips
}

/// Sort key placing daemon rows ahead of local ones.
const fn daemon_first(source: AttentionSource) -> u8 {
    match source {
        AttentionSource::Daemon => 0,
        AttentionSource::Local => 1,
    }
}

/// Map a daemon `attention` row's wire kind token to a chip.
///
/// Returns `None` for a token this build does not know, which is the additive
/// case: a newer daemon raising a kind this TUI has never heard of must not
/// render as a wrong chip, and dropping it is what the header's elsewhere count
/// then reports honestly.
#[must_use]
pub fn chip_for_daemon_kind(kind: &str) -> Option<AttentionKind> {
    Some(match kind {
        // All three are "a human has to answer something".
        "ask_user_question" | "waiting" | "codex_request_user" => AttentionKind::Ask,
        "approval" => AttentionKind::Approve,
        // An escalation is an error a human must see; it is not a question, so
        // it does not go in the blocking count.
        "error" | "escalation" => AttentionKind::Err,
        _ => return None,
    })
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
    rows.into_iter()
        .filter(|chips| chips.iter().any(|chip| chip.kind.blocks()))
        .count()
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

/// The merged attention picture the sessions screen renders.
///
/// One value, refreshed off the UI thread, holding both what the daemon said
/// and whether it was reachable at all. The two travel together on purpose: a
/// consumer that saw only the rows could not tell "the daemon says nothing is
/// waiting" from "the daemon did not answer", and those need opposite chips.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonAttention {
    /// Open rows, keyed by the working directory they were raised in.
    ///
    /// Keyed by cwd because that is the only identity the host row and the
    /// daemon row share: an ainb session knows its worktree path, and the
    /// daemon's row carries the cwd the hook fired in. The daemon's
    /// `session_id` is the PROVIDER's, which the host tree never learns.
    pub by_cwd: std::collections::HashMap<String, Vec<SessionAttention>>,
    /// `true` when the last poll reached the daemon, whatever it returned.
    pub reachable: bool,
    /// Why the last poll failed, for the one banner line the header shows.
    pub error: Option<String>,
}

impl DaemonAttention {
    /// The daemon answered, with these rows.
    #[must_use]
    pub fn up(by_cwd: std::collections::HashMap<String, Vec<SessionAttention>>) -> Self {
        Self {
            by_cwd,
            reachable: true,
            error: None,
        }
    }

    /// The daemon did not answer, carrying the last rows it DID report.
    ///
    /// Retained, not dropped. Dropping them looks like the right call — a stale
    /// row invites an answer no transport can deliver — but the chip is what
    /// tells the operator something needs them, and a chip that VANISHES on a
    /// transient poll failure is the silent no-op the spec forbids. Retaining
    /// it and flagging the daemon unreachable produces the shape the spec
    /// actually asks for: the chip greys out and the `ask` pane names the call
    /// that is unavailable.
    ///
    /// A row answered while the daemon was down lingers greyed until the next
    /// successful poll drops it. That is the safe direction: it is unanswerable
    /// from here for as long as it lingers, and the alternative is a live
    /// request disappearing because one socket read timed out.
    #[must_use]
    pub fn down(
        previous: std::collections::HashMap<String, Vec<SessionAttention>>,
        error: String,
    ) -> Self {
        Self {
            by_cwd: previous,
            reachable: false,
            error: Some(error),
        }
    }

    /// The daemon rows raised in `cwd`, trailing slash insensitive.
    #[must_use]
    pub fn rows_for(&self, cwd: &str) -> &[SessionAttention] {
        self.by_cwd.get(cwd.trim_end_matches('/')).map_or(&[], Vec::as_slice)
    }

    /// Daemon rows whose cwd matched no session row on this screen.
    ///
    /// Reported rather than dropped: the sessions screen is the ONE attention
    /// surface, so a request it cannot place still has to be counted somewhere
    /// the operator can see. `claimed` is every cwd a row on screen consumed.
    #[must_use]
    pub fn elsewhere(&self, claimed: &std::collections::HashSet<String>) -> usize {
        self.by_cwd
            .iter()
            .filter(|(cwd, _)| !claimed.contains(*cwd))
            .map(|(_, rows)| rows.iter().filter(|row| row.kind.blocks()).count())
            .sum()
    }
}

/// Resolve how an answer to `chip` would be delivered, given the session it
/// landed on and whether the daemon is up.
///
/// The four outcomes are the spec's edge table, in one place so the chip's grey
/// and the `ask` tab's refusal sentence can never disagree about why:
///
/// * a DONE or an ERR is not a question, so nothing is answerable;
/// * a daemon row keeps its attention id while the daemon is up;
/// * a daemon row whose daemon has gone says exactly which call is unavailable;
/// * a local APPROVE goes to notifyd's approve broker, because the hook is
///   parked there and reads nothing typed at the terminal;
/// * any other local row rides the session's own pane, which is what keeps the
///   surface working with the daemon down — and says so when there is neither.
#[must_use]
pub fn route_answer(
    chip: &SessionAttention,
    tmux_session: Option<&str>,
    session_id: Option<&str>,
    daemon_reachable: bool,
) -> Answerable {
    if !chip.kind.blocks() {
        return Answerable::No(Unanswerable::NotAQuestion);
    }
    if let Answerable::Daemon { attention_id } = &chip.answerable {
        return if daemon_reachable {
            Answerable::Daemon {
                attention_id: attention_id.clone(),
            }
        } else {
            Answerable::No(Unanswerable::DaemonGone)
        };
    }
    // A local permission request goes to the broker, never to the pane: the
    // hook is blocked in `client_await` and reads nothing typed at the
    // terminal. The broker is notifyd's and local, so this is the route that
    // keeps an APPROVE answerable with the hangar daemon stopped.
    if chip.kind == AttentionKind::Approve {
        return session_id.map_or(Answerable::No(Unanswerable::NoTransport), |session_id| {
            Answerable::Broker {
                session_id: session_id.to_string(),
            }
        });
    }
    tmux_session.map_or(Answerable::No(Unanswerable::NoTransport), |name| {
        Answerable::Tmux {
            tmux_session: name.to_string(),
        }
    })
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
    fn the_daemon_row_wins_while_the_daemon_is_up() {
        // Both producers watch the same session, so one ASK arrives twice. The
        // daemon's is the one carrying an id and options, so it is the one an
        // operator can act on.
        let local = SessionAttention::local(AttentionKind::Ask, 1_000);
        let from_daemon = SessionAttention::daemon(AttentionKind::Ask, 5_000, "att-1".into())
            .with_detail("Decide the sqlite path");
        let merged = normalise(vec![local, from_daemon]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, AttentionSource::Daemon);
        assert_eq!(merged[0].detail.as_deref(), Some("Decide the sqlite path"));
        assert_eq!(
            merged[0].answerable,
            Answerable::Daemon {
                attention_id: "att-1".into()
            }
        );
    }

    #[test]
    fn the_daemon_row_is_never_half_merged_into_the_local_one() {
        // The winning row keeps its OWN timestamp. Taking the local row's older
        // one would produce a row neither producer ever reported: a daemon id
        // with a local clock.
        let merged = normalise(vec![
            SessionAttention::local(AttentionKind::Ask, 1_000),
            SessionAttention::daemon(AttentionKind::Ask, 5_000, "att-1".into()),
        ]);
        assert_eq!(merged[0].since_ms, 5_000);
    }

    #[test]
    fn a_daemon_that_never_answered_lets_the_local_row_carry_the_surface() {
        // Third leg of the precedence sequence: local, then the daemon wins,
        // then the daemon is gone. With nothing to carry forward the merge sees
        // only the local row and the surface keeps working — which is the whole
        // point of reading notifyd off disk.
        let down = DaemonAttention::down(std::collections::HashMap::new(), "refused".into());
        assert!(down.rows_for("/work/proj").is_empty());
        let mut chips = vec![SessionAttention::local(AttentionKind::Ask, 1_000)];
        chips.extend(down.rows_for("/work/proj").iter().cloned());
        let merged = normalise(chips);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, AttentionSource::Local);
    }

    #[test]
    fn a_daemon_row_outlives_a_transient_poll_failure() {
        // And when the daemon HAS answered before, its row is carried across
        // the blip and greys instead of vanishing. A chip that disappears
        // because one socket read timed out is a request nobody answers.
        let mut by_cwd = std::collections::HashMap::new();
        by_cwd.insert(
            "/work/proj".to_string(),
            vec![SessionAttention::daemon(
                AttentionKind::Ask,
                1_000,
                "att-1".into(),
            )],
        );
        let down = DaemonAttention::down(by_cwd, "refused".into());
        let chips = normalise(down.rows_for("/work/proj").to_vec());
        assert_eq!(chips.len(), 1);
        assert!(
            !route_answer(&chips[0], Some("tmux_proj"), Some("sess"), down.reachable)
                .is_answerable(),
            "carried forward, but NOT answerable while the daemon is gone"
        );
    }

    #[test]
    fn different_kinds_from_different_producers_both_survive() {
        let merged = normalise(vec![
            SessionAttention::local(AttentionKind::Ask, 1_000),
            SessionAttention::daemon(AttentionKind::Err, 2_000, "att-e".into()),
        ]);
        assert_eq!(
            merged.iter().map(|c| c.kind).collect::<Vec<_>>(),
            vec![AttentionKind::Ask, AttentionKind::Err]
        );
    }

    #[test]
    fn rows_for_ignores_a_trailing_slash_on_either_side() {
        let mut by_cwd = std::collections::HashMap::new();
        by_cwd.insert(
            "/work/proj".to_string(),
            vec![SessionAttention::daemon(AttentionKind::Ask, 1, "a".into())],
        );
        let up = DaemonAttention::up(by_cwd);
        assert_eq!(up.rows_for("/work/proj/").len(), 1);
        assert_eq!(up.rows_for("/work/proj").len(), 1);
    }

    #[test]
    fn a_local_ask_rides_the_sessions_own_pane() {
        let chip = SessionAttention::local(AttentionKind::Ask, 0);
        assert_eq!(
            route_answer(&chip, Some("tmux_proj"), Some("sess"), false),
            Answerable::Tmux {
                tmux_session: "tmux_proj".into()
            },
            "the tmux route must survive the daemon being down — that is the point"
        );
    }

    #[test]
    fn a_daemon_ask_loses_its_route_when_the_daemon_goes_and_says_so() {
        let chip = SessionAttention::daemon(AttentionKind::Ask, 0, "att-1".into());
        let routed = route_answer(&chip, Some("tmux_proj"), Some("sess"), false);
        assert!(!routed.is_answerable());
        let refusal = routed.refusal().expect("a refusal always carries a reason");
        assert!(
            refusal.contains("attention/answer"),
            "the refusal must name the call that is unavailable: {refusal}"
        );
    }

    #[test]
    fn a_session_with_no_pane_and_no_daemon_row_says_it_has_no_transport() {
        let chip = SessionAttention::local(AttentionKind::Approve, 0);
        let routed = route_answer(&chip, None, None, false);
        assert!(!routed.is_answerable());
        assert!(routed.refusal().is_some_and(|r| r.contains("no live pane")));
    }

    #[test]
    fn an_err_or_a_done_is_never_answerable_because_it_is_not_a_question() {
        for kind in [AttentionKind::Err, AttentionKind::Done] {
            let chip = SessionAttention::local(kind, 0);
            let routed = route_answer(&chip, Some("tmux_proj"), Some("sess"), true);
            assert!(!routed.is_answerable(), "{kind} must not offer a composer");
            assert!(routed.refusal().is_some_and(|r| r.contains("nothing is waiting")));
        }
    }

    #[test]
    fn a_local_permission_request_goes_to_the_broker_not_the_pane() {
        // The hook is blocked in `client_await` and reads nothing typed at the
        // terminal, so routing an APPROVE to the pane is a send that lands
        // nowhere the agent is looking.
        let chip = SessionAttention::local(AttentionKind::Approve, 0);
        assert_eq!(
            route_answer(&chip, Some("tmux_proj"), Some("claude-sess-1"), false),
            Answerable::Broker {
                session_id: "claude-sess-1".into()
            },
            "and it must survive the hangar daemon being down"
        );
    }

    #[test]
    fn a_permission_request_with_no_waiter_identity_says_it_has_no_transport() {
        let chip = SessionAttention::local(AttentionKind::Approve, 0);
        let routed = route_answer(&chip, Some("tmux_proj"), None, false);
        assert!(!routed.is_answerable());
        assert!(routed.refusal().is_some());
    }

    #[test]
    fn a_daemon_approval_still_goes_through_the_daemon() {
        // The daemon runs its own first-answer-wins and its own last-mile send;
        // reaching around it to the broker would race its winner.
        let chip = SessionAttention::daemon(AttentionKind::Approve, 0, "att-1".into());
        assert_eq!(
            route_answer(&chip, Some("tmux_proj"), Some("claude-sess-1"), true),
            Answerable::Daemon {
                attention_id: "att-1".into()
            }
        );
    }

    #[test]
    fn a_bare_permission_request_offers_exactly_approve_and_deny() {
        let options = approval_options();
        assert_eq!(
            options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(),
            vec![APPROVE_LABEL, DENY_LABEL],
            "these are the only two answers a permission request takes"
        );
        assert!(
            options.iter().all(|o| !o.description.is_empty()),
            "each has to say what it does — `deny` alone is not obviously `block it`"
        );
    }

    #[test]
    fn every_refusal_carries_a_sentence_not_an_empty_grey_chip() {
        for why in [
            Unanswerable::DaemonGone,
            Unanswerable::NoTransport,
            Unanswerable::NotAQuestion,
        ] {
            let reason = why.reason();
            assert!(
                reason.len() > 20 && reason.chars().any(char::is_whitespace),
                "a greyed chip with no explanation is the silent no-op the spec \
                 forbids: {reason:?}"
            );
        }
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
