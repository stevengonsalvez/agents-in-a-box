//! Polymorphic actor references.
//!
//! A Hangar actor is a `(kind, id)` pair where `kind` is one of [`ActorKind`]'s
//! variants. Issues, comments, and tasks all carry actors in two flavours
//! (`member` and `agent`) without a real foreign key — the actor may live in
//! either the `member` or `agent` table, which a single `SQLite` FK cannot
//! express. The `(actor_type, actor_id)` invariant is therefore **FK-less by
//! design** (per the Multica architecture review §7): the `_type` column's
//! `CHECK` constraint keeps the discriminant honest, and the service layer is
//! responsible for the referential integrity of the `_id` half.
//!
//! The canonical textual form is `member:<id>` / `agent:<id>`, produced by the
//! [`Display`] impl and parsed by the [`FromStr`] impl.

use std::fmt;
use std::str::FromStr;

/// The kind of actor an [`ActorRef`] refers to.
///
/// Mirrors the `('member','agent')` set enforced by the `*_type` `CHECK`
/// constraints on the `issue`, `comment`, and (in P1) `agent_task_queue`
/// tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorKind {
    /// A human workspace member (`member` table).
    Member,
    /// An automated agent (`agent` table).
    Agent,
}

impl ActorKind {
    /// The lowercase wire token (`"member"` / `"agent"`) stored in the
    /// `*_type` columns.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Agent => "agent",
        }
    }
}

impl fmt::Display for ActorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ActorKind {
    type Err = ActorParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "member" => Ok(Self::Member),
            "agent" => Ok(Self::Agent),
            other => Err(ActorParseError::UnknownKind(other.to_string())),
        }
    }
}

/// A reference to a polymorphic actor: a [`ActorKind`] plus a non-empty id.
///
/// The `id` half is guaranteed non-empty by construction (see [`ActorRef::new`]
/// and [`ActorRef::from_str`]); there is no way to build an `ActorRef` with an
/// empty id, so downstream code never has to defend against `actor.id == ""`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActorRef {
    kind: ActorKind,
    id: String,
}

impl ActorRef {
    /// Build an [`ActorRef`], rejecting an empty `id`.
    ///
    /// # Errors
    ///
    /// Returns [`ActorParseError::EmptyId`] if `id` is empty (the only
    /// construction-time invariant).
    pub fn new(kind: ActorKind, id: impl Into<String>) -> Result<Self, ActorParseError> {
        let id = id.into();
        if id.is_empty() {
            return Err(ActorParseError::EmptyId);
        }
        Ok(Self { kind, id })
    }

    /// The actor's kind discriminant.
    #[must_use]
    pub const fn kind(&self) -> ActorKind {
        self.kind
    }

    /// The actor's (guaranteed non-empty) id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl fmt::Display for ActorRef {
    /// Render as the canonical `member:<id>` / `agent:<id>` form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind, self.id)
    }
}

impl FromStr for ActorRef {
    type Err = ActorParseError;

    /// Parse the canonical `member:<id>` / `agent:<id>` form.
    ///
    /// Splits on the **first** `:` so ids may themselves contain colons.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (kind, id) = s.split_once(':').ok_or(ActorParseError::MissingSeparator)?;
        Self::new(kind.parse::<ActorKind>()?, id)
    }
}

/// Failure modes when constructing or parsing an [`ActorRef`] / [`ActorKind`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActorParseError {
    /// The string had no `:` separating kind from id.
    #[error("actor reference missing ':' separator (expected 'member:<id>' or 'agent:<id>')")]
    MissingSeparator,
    /// The kind token was not one of `member` / `agent`.
    #[error("unknown actor kind '{0}' (expected 'member' or 'agent')")]
    UnknownKind(String),
    /// The id half was empty.
    #[error("actor id must not be empty")]
    EmptyId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_roundtrips_through_from_str() {
        for s in ["member:abc", "agent:xyz-123"] {
            let parsed: ActorRef = s.parse().expect("parse");
            assert_eq!(parsed.to_string(), s);
        }
    }

    #[test]
    fn parses_member_and_agent_kinds() {
        assert_eq!(
            "member:u1".parse::<ActorRef>().unwrap().kind(),
            ActorKind::Member
        );
        assert_eq!(
            "agent:a1".parse::<ActorRef>().unwrap().kind(),
            ActorKind::Agent
        );
    }

    #[test]
    fn id_may_contain_colons() {
        let a: ActorRef = "agent:a:b:c".parse().expect("parse");
        assert_eq!(a.id(), "a:b:c");
    }

    #[test]
    fn rejects_unknown_kind() {
        assert_eq!(
            "frog:abc".parse::<ActorRef>(),
            Err(ActorParseError::UnknownKind("frog".to_string()))
        );
    }

    #[test]
    fn rejects_missing_separator() {
        assert_eq!(
            "memberabc".parse::<ActorRef>(),
            Err(ActorParseError::MissingSeparator)
        );
    }

    #[test]
    fn rejects_empty_id_at_construction() {
        assert_eq!(
            ActorRef::new(ActorKind::Member, ""),
            Err(ActorParseError::EmptyId)
        );
        assert_eq!("member:".parse::<ActorRef>(), Err(ActorParseError::EmptyId));
    }
}
