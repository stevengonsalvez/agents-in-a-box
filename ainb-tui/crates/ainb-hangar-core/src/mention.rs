//! **Mention grammar + routing outcome vocabulary** (multica parity #2-rest).
//!
//! This module is the pure half of the mention-routing layer: it turns a comment
//! body into the ordered set of things the author addressed, and it names the
//! per-target outcomes the router reports back. Resolution (matching a target to
//! a real agent / member / squad) and the writes live in
//! `ainb_hangar_store::service::mention`, which is the single seam both the
//! daemon RPC and the CLI drive.
//!
//! # Two markup forms
//!
//! 1. **Link form** — multica's `util.MentionRe`
//!    (`server/internal/util/mention.go:17`):
//!    `` [@Label](mention://<type>/<id>) `` with `type` in
//!    `member | agent | squad | issue | all`. This is the exact, unambiguous
//!    address: the id is carried verbatim so two agents sharing a display label
//!    can never be confused. The label is matched NON-GREEDILY because a label
//!    may itself contain `[` / `]` (the upstream comment says the same).
//!
//!    One deliberate widening: multica's id class is UUID hex
//!    (`[0-9a-fA-F-]+`), hangar ids are ULIDs and its slugs are kebab-case, so
//!    the class here is `[A-Za-z0-9_-]+` (plus the literal `all`).
//!
//! 2. **Bare form** — `@handle`, the grammar hangar has shipped since e38.7.
//!    [`parse_handles`] is that implementation moved here VERBATIM (it was
//!    `ainb_hangar_daemon::mentions::parse_mentions`) together with its
//!    regression tests, because the daemon must keep resolving today's bare
//!    mentions exactly as it does now.
//!
//! Both forms are collected in ONE pass over the body, first-seen order
//! preserved. A bare `@handle` that appears INSIDE a link's label is not a
//! second mention: link spans are blanked out before the bare scan, so
//! `[@alice](mention://member/u1)` yields exactly one target.
//!
//! # Dedup
//!
//! Multica's dedup key is `type + ":" + id` (`mention.go:29`). This module uses
//! the same key for links — `(kind, token)` — and the bare `token` alone for
//! bare handles (a bare handle has no type until the router resolves it).

use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// The entity family a `mention://` link addresses.
///
/// `member | agent | issue | all` are multica's original four (`mention.go:17`);
/// `squad` is the newer revision's addition and is the form that routes to the
/// squad's leader (multica step 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionTargetKind {
    /// A human workspace member — a NOTIFICATION target, never a trigger.
    Member,
    /// An agent — the trigger target.
    Agent,
    /// A squad — routes to the squad's leader.
    Squad,
    /// A cross-reference to another issue. Never a trigger.
    Issue,
    /// The `mention://all/all` broadcast marker. Never a trigger.
    All,
}

impl MentionTargetKind {
    /// The wire / link token for this kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Agent => "agent",
            Self::Squad => "squad",
            Self::Issue => "issue",
            Self::All => "all",
        }
    }

    /// Parse a link type token, returning `None` outside the closed set.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "member" => Some(Self::Member),
            "agent" => Some(Self::Agent),
            "squad" => Some(Self::Squad),
            "issue" => Some(Self::Issue),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

/// Which markup produced a [`ParsedMention`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MentionForm {
    /// `[@Label](mention://type/id)` — typed and exact.
    Link,
    /// `@handle` — untyped; the router resolves it (agent first, then member).
    Bare,
}

/// One addressed target lifted out of a comment body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMention {
    /// The link's declared type, or `None` for a bare handle (whose type is only
    /// known once the router has resolved it).
    pub kind: Option<MentionTargetKind>,
    /// The id for a link, the handle for a bare mention — verbatim as typed.
    pub token: String,
    /// The display label from the link, or the handle itself for a bare mention.
    pub label: String,
    /// Which markup this came from.
    pub form: MentionForm,
}

/// Where a routed target came from: an explicit mention in the body, or one of
/// the three implicit fallbacks multica walks when the body mentions nobody
/// (`comment.go` step 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionSource {
    /// Named in the comment body (or inherited from the parent, which multica
    /// also treats as explicit).
    Explicit,
    /// The author of the comment this one replies to.
    ReplyParent,
    /// The author of the thread's root comment.
    ThreadRoot,
    /// The issue's assignee.
    Assignee,
}

impl MentionSource {
    /// The wire token for this source.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::ReplyParent => "reply_parent",
            Self::ThreadRoot => "thread_root",
            Self::Assignee => "assignee",
        }
    }
}

/// What the router DID about one addressed target.
///
/// `Queued | Coalesced | Deferred | Blocked` are multica's four
/// `CommentTriggerOutcome` values verbatim (MUL-4525 §2).
///
/// # Hangar divergence
///
/// `Notified` and `Ignored` are **hangar-additive**. Multica's outcome array
/// only ever describes AGENT triggers: a member mention and an unresolvable
/// handle simply do not appear in it. Hangar reports them because reporting
/// every target — including "this went to a human, not a run" and "this handle
/// matched nothing" — is the entire point of this layer; silently dropping the
/// non-trigger cases is exactly the failure mode it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionOutcome {
    /// A task row was written for this agent.
    Queued,
    /// Folded into a task this agent already had pending on the issue.
    Coalesced,
    /// Admitted but parked behind unfinished blockers; promoted later.
    Deferred,
    /// Refused. The paired `DispatchReason` says why.
    Blocked,
    /// A human was notified (inbox + subscription). Never a run.
    Notified,
    /// Nothing to do: the handle resolved to nothing, or the link was an
    /// `issue` / `all` cross-reference.
    Ignored,
}

impl MentionOutcome {
    /// Every variant, in declaration order.
    pub const ALL: [Self; 6] = [
        Self::Queued,
        Self::Coalesced,
        Self::Deferred,
        Self::Blocked,
        Self::Notified,
        Self::Ignored,
    ];

    /// The wire token for this outcome (matches the `serde` rename).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Coalesced => "coalesced",
            Self::Deferred => "deferred",
            Self::Blocked => "blocked",
            Self::Notified => "notified",
            Self::Ignored => "ignored",
        }
    }

    /// Parse an outcome token, returning `None` outside the closed set.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.as_str() == raw)
    }
}

/// multica `util.MentionRe` (`mention.go:17`), with the id class widened from
/// UUID hex to the ULID / slug characters hangar ids actually use.
static LINK_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\[@?(.+?)\]\(mention://(member|agent|squad|issue|all)/([A-Za-z0-9_-]+)\)")
        .expect("mention link regex is a compile-time constant")
});

/// Scan `body` for every addressed target, link form and bare form, in
/// first-seen order.
///
/// Links are scanned first and their spans blanked out, so a bare `@handle`
/// inside a link's label is counted ONCE (as the link), never twice. Dedup is by
/// `(kind, token)` for links (multica's `type:id` key) and by `token` for bare
/// handles.
#[must_use]
pub fn parse(body: &str) -> Vec<ParsedMention> {
    let mut out: Vec<ParsedMention> = Vec::new();
    // Blank out the link spans byte-for-byte (spaces keep the byte offsets of
    // the surrounding text intact, so the bare scan sees the same boundaries).
    let mut residue: Vec<u8> = body.as_bytes().to_vec();
    for caps in LINK_RE.captures_iter(body) {
        let whole = caps.get(0).expect("group 0 always matches");
        let label = caps.get(1).map_or("", |m| m.as_str()).to_string();
        let kind = MentionTargetKind::parse(caps.get(2).map_or("", |m| m.as_str()));
        let token = caps.get(3).map_or("", |m| m.as_str()).to_string();
        for b in &mut residue[whole.start()..whole.end()] {
            *b = b' ';
        }
        if !out
            .iter()
            .any(|m| m.form == MentionForm::Link && m.kind == kind && m.token == token)
        {
            out.push(ParsedMention {
                kind,
                token,
                label,
                form: MentionForm::Link,
            });
        }
    }
    // The residue is still valid UTF-8: only whole ASCII-delimited link spans
    // were overwritten with ASCII spaces.
    let residue = String::from_utf8(residue).unwrap_or_else(|_| body.to_string());
    for handle in parse_handles(&residue) {
        if !out.iter().any(|m| m.form == MentionForm::Bare && m.token == handle) {
            out.push(ParsedMention {
                kind: None,
                label: handle.clone(),
                token: handle,
                form: MentionForm::Bare,
            });
        }
    }
    out
}

/// The handle characters that may follow `@` in a mention: ASCII alphanumerics
/// plus `-` and `_` (agent names like `claude-agent` are valid handles).
const fn is_handle_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Scan `body` for bare `@handle` mentions, returning the distinct handles in
/// first-seen order (case preserved — the resolver decides case sensitivity).
///
/// A mention is an `@` at a token boundary (body start, or after a non-handle
/// char such as whitespace) followed by ≥1 [handle char](is_handle_char). The
/// leading `@` and the handle are case-preserved verbatim. Duplicates collapse
/// to their first occurrence so `"@bot @bot"` yields one handle, mirroring the
/// resolver's idempotent per-agent enqueue.
///
/// An `@` embedded mid-token (e.g. inside `user@host`) is not a boundary mention
/// and is skipped, so email-shaped text never spawns a phantom mention.
///
/// This is `ainb_hangar_daemon::mentions::parse_mentions` moved here unchanged;
/// the daemon re-exports it so today's call sites and tests are untouched.
#[must_use]
pub fn parse_handles(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // A mention's `@` must sit at a token boundary: either the body start or
        // immediately after a character that cannot itself be part of a handle.
        // This stops `user@host` (the `@` follows the handle char `r`) from being
        // read as an `@host` mention.
        let at_boundary = i == 0 || !is_handle_char(chars[i - 1]);
        if chars[i] == '@' && at_boundary {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && is_handle_char(chars[j]) {
                j += 1;
            }
            // Require at least one handle char after `@`; a bare `@` is ignored.
            if j > start {
                let handle: String = chars[start..j].iter().collect();
                if !out.contains(&handle) {
                    out.push(handle);
                }
            }
            i = j.max(start);
            continue;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{MentionForm, MentionOutcome, MentionTargetKind, parse, parse_handles};

    // --- bare grammar: the 9 regression tests moved verbatim from the daemon ---

    #[test]
    fn extracts_a_single_hyphenated_handle() {
        assert_eq!(
            parse_handles("@claude-agent please do X"),
            vec!["claude-agent".to_string()]
        );
    }

    #[test]
    fn extracts_multiple_handles_in_first_seen_order() {
        assert_eq!(
            parse_handles("ping @alpha and @beta then @gamma"),
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
        );
    }

    #[test]
    fn collapses_duplicate_handles_to_first_occurrence() {
        assert_eq!(
            parse_handles("@bot @bot @bot"),
            vec!["bot".to_string()],
            "a repeated mention yields one handle (resolver is idempotent)"
        );
    }

    #[test]
    fn a_plain_comment_yields_no_handles() {
        assert!(parse_handles("just a normal comment, no pings").is_empty());
    }

    #[test]
    fn an_email_address_is_not_a_mention() {
        assert!(
            parse_handles("mail me at alice@example.com").is_empty(),
            "the @ in an email follows a handle char, not a boundary"
        );
    }

    #[test]
    fn a_bare_at_sign_is_ignored() {
        assert!(parse_handles("just an @ here and @ there").is_empty());
    }

    #[test]
    fn punctuation_terminates_a_handle() {
        assert_eq!(
            parse_handles("hey @claude-agent, ship it!"),
            vec!["claude-agent".to_string()],
            "the comma is not a handle char so it ends the handle"
        );
    }

    #[test]
    fn underscores_and_digits_are_handle_chars() {
        assert_eq!(parse_handles("@agent_7 go"), vec!["agent_7".to_string()]);
    }

    #[test]
    fn mention_at_body_start_with_no_leading_space() {
        assert_eq!(parse_handles("@solo"), vec!["solo".to_string()]);
    }

    // --- link grammar ---

    #[test]
    fn parses_a_link_form_agent_mention() {
        let got = parse("[@Builder](mention://agent/agent-1) please ship");
        assert_eq!(got.len(), 1, "one target: {got:?}");
        assert_eq!(got[0].kind, Some(MentionTargetKind::Agent));
        assert_eq!(got[0].token, "agent-1");
        assert_eq!(got[0].label, "Builder");
        assert_eq!(got[0].form, MentionForm::Link);
    }

    #[test]
    fn a_bare_handle_inside_a_link_label_is_counted_once() {
        let got = parse("[@alice](mention://member/user-1) can you look?");
        assert_eq!(
            got.len(),
            1,
            "the label's `@alice` must not double-count as a bare mention: {got:?}"
        );
        assert_eq!(got[0].kind, Some(MentionTargetKind::Member));
        assert_eq!(got[0].token, "user-1");
    }

    #[test]
    fn dedups_links_by_type_and_id() {
        let got = parse(
            "[@A](mention://agent/a1) then [@A again](mention://agent/a1) \
             and [@A](mention://member/a1)",
        );
        assert_eq!(
            got.len(),
            2,
            "same (type,id) collapses; a different type does not: {got:?}"
        );
        assert_eq!(got[0].kind, Some(MentionTargetKind::Agent));
        assert_eq!(got[1].kind, Some(MentionTargetKind::Member));
    }

    #[test]
    fn a_label_may_contain_brackets() {
        let got = parse("[@Team [core]](mention://squad/sq-1) heads up");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].kind, Some(MentionTargetKind::Squad));
        assert_eq!(got[0].token, "sq-1");
    }

    #[test]
    fn issue_and_all_links_parse_as_their_own_kinds() {
        let got = parse("see [#12](mention://issue/issue-12) and [@all](mention://all/all)");
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].kind, Some(MentionTargetKind::Issue));
        assert_eq!(got[1].kind, Some(MentionTargetKind::All));
    }

    #[test]
    fn links_and_bare_handles_mix_in_first_seen_order() {
        let got = parse("[@Builder](mention://agent/a1) and @claude-agent");
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].form, MentionForm::Link);
        assert_eq!(got[1].form, MentionForm::Bare);
        assert_eq!(got[1].token, "claude-agent");
    }

    #[test]
    fn an_unknown_link_type_is_not_a_link_mention() {
        let got = parse("[@x](mention://robot/r1)");
        assert!(
            got.iter().all(|m| m.form == MentionForm::Bare),
            "only the closed type set yields a LINK target: {got:?}"
        );
        // The label's `@x` is still a bare handle — that is today's shipped
        // behaviour and the link scan must not silently take it away.
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].token, "x");
    }

    #[test]
    fn a_plain_markdown_link_is_not_a_mention() {
        assert!(parse("see [the docs](https://example.com/x)").is_empty());
    }

    // --- outcome vocabulary ---

    #[test]
    fn outcome_tokens_round_trip() {
        for o in MentionOutcome::ALL {
            assert_eq!(MentionOutcome::parse(o.as_str()), Some(o));
            let json = serde_json::to_string(&o).unwrap();
            assert_eq!(json, format!("\"{}\"", o.as_str()), "serde matches as_str");
        }
    }

    #[test]
    fn multicas_four_outcome_tokens_are_present_verbatim() {
        for token in ["queued", "coalesced", "deferred", "blocked"] {
            assert!(
                MentionOutcome::parse(token).is_some(),
                "multica CommentTriggerOutcome `{token}` must exist verbatim"
            );
        }
    }
}
