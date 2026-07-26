//! Issue / task **origin provenance** — the `(origin_type, origin_id)` pair
//! (multica parity #21, migration 0056).
//!
//! multica stamps every platform-created issue with an origin pair
//! (`server/migrations/042_autopilot.up.sql:74-77`, widened by
//! `060_issue_origin_quick_create.up.sql`). It is load-bearing, not decorative:
//! the completion handler resolves "the issue this run produced" by a
//! **deterministic by-origin lookup** (`service/task.go:1836 GetIssueByOrigin`)
//! rather than "the agent's most recent issue", which races when one agent
//! creates several issues concurrently; and run/analytics attribution reads the
//! pair back (`service/autopilot.go:251`, `service/task.go:257`).
//!
//! # The kind set
//!
//! hangar's closed allow-list is [`OriginKind::Autopilot`],
//! [`OriginKind::CommentMention`] and [`OriginKind::Manual`].
//! `comment_mention` is the structural analogue of multica's `quick_create`:
//! hangar has no quick-create RPC, its agent-triggering flow is the `@handle`
//! comment mention, so the daemon injects that provenance into the agent
//! child's env and an issue the agent creates mid-run carries it.
//!
//! # `origin_id` semantics
//!
//! | kind | `origin_id` |
//! |---|---|
//! | `autopilot` | `autopilot.id` — the **rule**, not the run (multica passes `ap.ID`, `service/autopilot.go:145`) |
//! | `comment_mention` | `comment.id` |
//! | `manual` | `NULL` |
//!
//! # Strict on write, lenient on read
//!
//! [`OriginKind::parse`] is the **single write-side gate** — every wire, CLI and
//! repo write funnels through it, so a rogue caller cannot mint an arbitrary
//! label (multica enforces the same allow-list at its handler,
//! `internal/handler/issue.go:1213-1231`). [`OriginKind::from_db_str`] is
//! deliberately **lenient** (unknown → `Manual`, mirroring
//! `ConcurrencyPolicy::from_db_str`) so a newer daemon writing a future kind
//! cannot make an older binary fail a read.

use std::fmt;

/// The closed allow-list of provenance kinds.
///
/// Stored as the TEXT `origin_type` column; there is deliberately no SQLite
/// `CHECK` (SQLite cannot `ALTER TABLE … ADD CONSTRAINT`, and this crate
/// already enforces column domains in the repo layer — see 0055's `link_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OriginKind {
    /// An autopilot rule firing in create-issue mode produced this row.
    /// `origin_id` is the **autopilot** id.
    Autopilot,
    /// The row was produced by (or during) a run spawned from an `@handle`
    /// comment mention. `origin_id` is the **comment** id.
    CommentMention,
    /// A human authored it. `origin_id` is `NULL`.
    ///
    /// Note this is stamped *explicitly*, which makes the column a complete
    /// record: `origin_type IS NULL` means "created before provenance existed /
    /// unknown", `'manual'` means "a human authored it". Pre-0056 rows are
    /// deliberately **not** backfilled.
    Manual,
}

impl OriginKind {
    /// The exact string persisted in `origin_type` and carried on the wire.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Autopilot => "autopilot",
            Self::CommentMention => "comment_mention",
            Self::Manual => "manual",
        }
    }

    /// STRICT parse — the write-side gate.
    ///
    /// # Errors
    ///
    /// [`OriginParseError::UnknownKind`] for any string outside the allow-list.
    pub fn parse(s: &str) -> Result<Self, OriginParseError> {
        match s.trim() {
            "autopilot" => Ok(Self::Autopilot),
            "comment_mention" => Ok(Self::CommentMention),
            "manual" => Ok(Self::Manual),
            other => Err(OriginParseError::UnknownKind(other.to_string())),
        }
    }

    /// LENIENT read-side decode: an unrecognised stored value degrades to
    /// [`Self::Manual`] rather than failing the row.
    #[must_use]
    pub fn from_db_str(s: &str) -> Self {
        Self::parse(s).unwrap_or(Self::Manual)
    }

    /// Whether this kind REQUIRES an `origin_id` (multica's pair rule:
    /// `internal/handler/issue.go:1214`). Only `manual` may omit it.
    #[must_use]
    pub const fn requires_id(self) -> bool {
        !matches!(self, Self::Manual)
    }
}

impl fmt::Display for OriginKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_db_str())
    }
}

/// Why an `(origin_type, origin_id)` pair was rejected at a write boundary.
///
/// Each variant maps to one distinct client-error message so the CLI and the
/// RPC handler say the same thing (multica's wording, `handler/issue.go:1215`
/// and `:1221`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginParseError {
    /// `origin_type` was outside the allow-list.
    UnknownKind(String),
    /// An `origin_id` was supplied with no `origin_type`.
    IdWithoutKind,
    /// A kind that [`OriginKind::requires_id`] was supplied with no (or a
    /// blank) `origin_id`.
    MissingId(OriginKind),
}

impl fmt::Display for OriginParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKind(got) => write!(
                f,
                "unsupported origin_type '{got}' (expected one of: autopilot, comment_mention, manual)"
            ),
            Self::IdWithoutKind => {
                f.write_str("origin_type and origin_id must be provided together")
            }
            Self::MissingId(kind) => write!(f, "origin_type '{kind}' requires an origin_id"),
        }
    }
}

impl std::error::Error for OriginParseError {}

/// A validated provenance pair.
///
/// The only way to build one is through a constructor that enforces the pair
/// rule, so an `IssueOrigin` in hand is always a legal `(origin_type,
/// origin_id)` write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueOrigin {
    kind: OriginKind,
    id: Option<String>,
}

impl IssueOrigin {
    /// Build a pair, enforcing the pair rule and rejecting a blank id.
    ///
    /// # Errors
    ///
    /// [`OriginParseError::MissingId`] when `kind.requires_id()` and `id` is
    /// absent or blank. A `manual` kind silently drops any supplied id (there
    /// is nothing meaningful to point at).
    pub fn new(kind: OriginKind, id: Option<String>) -> Result<Self, OriginParseError> {
        let id = id.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if kind.requires_id() && id.is_none() {
            return Err(OriginParseError::MissingId(kind));
        }
        let id = if kind.requires_id() { id } else { None };
        Ok(Self { kind, id })
    }

    /// The human-authored origin: `('manual', NULL)`.
    #[must_use]
    pub fn manual() -> Self {
        Self {
            kind: OriginKind::Manual,
            id: None,
        }
    }

    /// `('autopilot', <autopilot.id>)` — the RULE id, not the run id.
    ///
    /// # Errors
    ///
    /// [`OriginParseError::MissingId`] when `autopilot_id` is blank.
    pub fn autopilot(autopilot_id: impl Into<String>) -> Result<Self, OriginParseError> {
        Self::new(OriginKind::Autopilot, Some(autopilot_id.into()))
    }

    /// `('comment_mention', <comment.id>)`.
    ///
    /// # Errors
    ///
    /// [`OriginParseError::MissingId`] when `comment_id` is blank.
    pub fn comment_mention(comment_id: impl Into<String>) -> Result<Self, OriginParseError> {
        Self::new(OriginKind::CommentMention, Some(comment_id.into()))
    }

    /// Decode an optional wire/CLI pair. `(None, None)` ⇒ `Ok(None)` — the
    /// caller then applies its own default (the daemon stamps `manual`).
    ///
    /// # Errors
    ///
    /// [`OriginParseError::IdWithoutKind`] for an id with no kind,
    /// [`OriginParseError::UnknownKind`] for a kind outside the allow-list, and
    /// [`OriginParseError::MissingId`] for a kind that needs an id and got none.
    pub fn from_wire(
        kind: Option<&str>,
        id: Option<&str>,
    ) -> Result<Option<Self>, OriginParseError> {
        let kind = kind.map(str::trim).filter(|s| !s.is_empty());
        let id = id.map(str::trim).filter(|s| !s.is_empty());
        match (kind, id) {
            (None, None) => Ok(None),
            (None, Some(_)) => Err(OriginParseError::IdWithoutKind),
            (Some(k), id) => {
                let kind = OriginKind::parse(k)?;
                Self::new(kind, id.map(ToString::to_string)).map(Some)
            }
        }
    }

    /// LENIENT read-side decode of the two stored columns. A NULL
    /// `origin_type` (every pre-0056 row) yields `None`; a stored kind outside
    /// the allow-list degrades to `manual` rather than failing the row.
    #[must_use]
    pub fn from_db(kind: Option<String>, id: Option<String>) -> Option<Self> {
        let kind = OriginKind::from_db_str(kind?.as_str());
        let id = id.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        Some(Self {
            kind,
            id: if kind.requires_id() { id } else { None },
        })
    }

    /// The provenance kind.
    #[must_use]
    pub const fn kind(&self) -> OriginKind {
        self.kind
    }

    /// The provenance id (`None` for `manual`).
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// The `origin_type` column value.
    #[must_use]
    pub const fn kind_db_str(&self) -> &'static str {
        self.kind.as_db_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_strict_over_the_allow_list() {
        assert_eq!(OriginKind::parse("autopilot"), Ok(OriginKind::Autopilot));
        assert_eq!(
            OriginKind::parse("comment_mention"),
            Ok(OriginKind::CommentMention)
        );
        assert_eq!(OriginKind::parse("manual"), Ok(OriginKind::Manual));
        for bogus in ["quick_create", "Autopilot", "", "webhook", "autopilot "] {
            let got = OriginKind::parse(bogus);
            if bogus.trim() == "autopilot" {
                assert!(got.is_ok(), "surrounding whitespace is trimmed");
            } else {
                assert!(
                    matches!(got, Err(OriginParseError::UnknownKind(_))),
                    "{bogus:?} must be rejected by the write-side gate"
                );
            }
        }
    }

    #[test]
    fn from_db_str_is_lenient_so_a_future_kind_cannot_break_an_old_reader() {
        assert_eq!(OriginKind::from_db_str("autopilot"), OriginKind::Autopilot);
        assert_eq!(
            OriginKind::from_db_str("a_kind_from_the_future"),
            OriginKind::Manual
        );
    }

    #[test]
    fn pair_rule_requires_an_id_for_every_kind_but_manual() {
        assert!(OriginKind::Autopilot.requires_id());
        assert!(OriginKind::CommentMention.requires_id());
        assert!(!OriginKind::Manual.requires_id());

        assert_eq!(
            IssueOrigin::new(OriginKind::Autopilot, None),
            Err(OriginParseError::MissingId(OriginKind::Autopilot))
        );
        assert_eq!(
            IssueOrigin::new(OriginKind::CommentMention, Some("   ".into())),
            Err(OriginParseError::MissingId(OriginKind::CommentMention)),
            "a blank id is not an id"
        );
        let manual = IssueOrigin::new(OriginKind::Manual, Some("ignored".into())).unwrap();
        assert_eq!(manual.id(), None, "manual never carries an id");
    }

    #[test]
    fn from_wire_absent_pair_is_not_an_error() {
        assert_eq!(IssueOrigin::from_wire(None, None), Ok(None));
    }

    #[test]
    fn from_wire_rejects_an_id_without_a_kind() {
        assert_eq!(
            IssueOrigin::from_wire(None, Some("c-1")),
            Err(OriginParseError::IdWithoutKind)
        );
        assert_eq!(
            IssueOrigin::from_wire(None, Some("c-1")).unwrap_err().to_string(),
            "origin_type and origin_id must be provided together",
            "multica's wording, verbatim"
        );
    }

    #[test]
    fn from_wire_rejects_an_unknown_kind() {
        let err = IssueOrigin::from_wire(Some("quick_create"), Some("x")).unwrap_err();
        assert!(matches!(err, OriginParseError::UnknownKind(ref k) if k == "quick_create"));
        assert!(err.to_string().starts_with("unsupported origin_type"));
    }

    #[test]
    fn from_wire_accepts_the_allow_listed_pairs() {
        let ap = IssueOrigin::from_wire(Some("autopilot"), Some("ap-1")).unwrap().unwrap();
        assert_eq!(ap.kind(), OriginKind::Autopilot);
        assert_eq!(ap.id(), Some("ap-1"));

        let manual = IssueOrigin::from_wire(Some("manual"), None).unwrap().unwrap();
        assert_eq!(manual.kind(), OriginKind::Manual);
        assert_eq!(manual.id(), None);
    }

    #[test]
    fn from_db_maps_null_kind_to_no_provenance() {
        assert_eq!(IssueOrigin::from_db(None, Some("stray".into())), None);
        assert_eq!(IssueOrigin::from_db(None, None), None);
    }

    #[test]
    fn from_db_round_trips_a_stamped_pair() {
        let o = IssueOrigin::from_db(Some("comment_mention".into()), Some("c-7".into())).unwrap();
        assert_eq!(o.kind_db_str(), "comment_mention");
        assert_eq!(o.id(), Some("c-7"));
    }
}
