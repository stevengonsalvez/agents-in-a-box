//! Per-issue ACTIVITY vocabulary (multica parity #13, migration 0059).
//!
//! The stable `created | status_changed | assignee_changed | …` token set shared
//! by the layer that DECIDES an issue changed (the diff service) and the layer
//! that SERIALIZES it (the repo + the wire), so the two cannot drift. Same shape
//! as [`crate::dispatch_reason`], the parity #12 precedent.
//!
//! # Tolerant reads
//!
//! [`ActivityAction::parse`] returns `None` for a token this binary does not
//! know: a row written by a newer daemon renders as its raw string instead of
//! poisoning the read path. That is why the `action` column carries no `CHECK`
//! (see the migration's comment block).
//!
//! # The third actor kind
//!
//! [`crate::actor::ActorKind`] is `member | agent`, and widening it would ripple
//! into the `CHECK` constraints on `issue`, `comment` and `agent_task_queue`.
//! Activity rows need a third, activity-only kind for a daemon-driven transition
//! with no human or agent author, so [`ActivityActor`] wraps an [`ActorRef`]
//! alongside a `System` variant. multica's own `actor_type` `CHECK` admits
//! `'system'` for the same reason.
//!
//! # multica DEVIATION
//!
//! multica also emits `description_updated`. hangar's `IssueFieldUpdate` has no
//! `description` field and `IssueUpdateParams` carries no description, so there
//! is no description-edit path to instrument. The variant is deliberately
//! omitted rather than added dead; adding it later is append-only.

use crate::actor::{ActorKind, ActorRef};

/// One entry in the per-issue narrative.
///
/// The wire/DB token is [`Self::as_db_str`]; the human render text is
/// [`Self::label`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityAction {
    /// The issue was created.
    Created,
    /// `state` moved between two lifecycle values.
    StatusChanged,
    /// `assignee` was set, cleared, or re-pointed.
    AssigneeChanged,
    /// `priority` changed (hangar priority is `0..3`, so the details carry
    /// NUMBERS where multica carries a string enum).
    PriorityChanged,
    /// `title` was edited.
    TitleChanged,
    /// `due_date` was set, cleared, or moved.
    DueDateChanged,
    /// A task on the issue finished successfully.
    TaskCompleted,
    /// A task on the issue failed.
    TaskFailed,
}

impl ActivityAction {
    /// The stable token stored in `activity_log.action` and put on the wire.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::StatusChanged => "status_changed",
            Self::AssigneeChanged => "assignee_changed",
            Self::PriorityChanged => "priority_changed",
            Self::TitleChanged => "title_changed",
            Self::DueDateChanged => "due_date_changed",
            Self::TaskCompleted => "task_completed",
            Self::TaskFailed => "task_failed",
        }
    }

    /// Decode a stored token. **Tolerant**: an unknown token is `None`, and the
    /// caller renders the raw string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "created" => Some(Self::Created),
            "status_changed" => Some(Self::StatusChanged),
            "assignee_changed" => Some(Self::AssigneeChanged),
            "priority_changed" => Some(Self::PriorityChanged),
            "title_changed" => Some(Self::TitleChanged),
            "due_date_changed" => Some(Self::DueDateChanged),
            "task_completed" => Some(Self::TaskCompleted),
            "task_failed" => Some(Self::TaskFailed),
            _ => None,
        }
    }

    /// Short human render text for the timeline surfaces.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::StatusChanged => "moved",
            Self::AssigneeChanged => "assigned",
            Self::PriorityChanged => "priority",
            Self::TitleChanged => "renamed",
            Self::DueDateChanged => "due date",
            Self::TaskCompleted => "task completed",
            Self::TaskFailed => "task failed",
        }
    }
}

/// Who an activity row is attributed to.
///
/// [`Self::System`] is the activity-only third kind: a daemon-driven transition
/// (a board card move, a PR-merged auto-done) with no human or agent author. It
/// stores `actor_type='system', actor_id=NULL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityActor {
    /// A real hangar actor (`member:<id>` / `agent:<id>`).
    Actor(ActorRef),
    /// The daemon itself.
    System,
}

impl ActivityActor {
    /// The `'member' | 'agent' | 'system'` token stored in `actor_type`.
    #[must_use]
    pub const fn type_str(&self) -> &'static str {
        match self {
            Self::Actor(a) => a.kind().as_str(),
            Self::System => "system",
        }
    }

    /// The `actor_id` half; `None` for [`Self::System`].
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Actor(a) => Some(a.id()),
            Self::System => None,
        }
    }

    /// Rebuild from a stored `(actor_type, actor_id)` pair.
    ///
    /// `None` for a malformed row (an unknown kind, or a `member`/`agent` row
    /// with no id) — the caller renders it raw rather than failing the read.
    #[must_use]
    pub fn parse(ty: &str, id: Option<&str>) -> Option<Self> {
        if ty == "system" {
            return Some(Self::System);
        }
        let kind: ActorKind = ty.parse().ok()?;
        ActorRef::new(kind, id?).ok().map(Self::Actor)
    }

    /// Convenience constructor for a member-attributed row.
    ///
    /// Returns [`Self::System`] when `member_id` is `None` or empty — hangar has
    /// no per-request auth context, so a write with no resolvable owner is a
    /// system fact, never a fabricated member.
    #[must_use]
    pub fn member_or_system(member_id: Option<&str>) -> Self {
        member_id
            .filter(|id| !id.is_empty())
            .and_then(|id| ActorRef::new(ActorKind::Member, id).ok())
            .map_or(Self::System, Self::Actor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_token_round_trips() {
        for a in [
            ActivityAction::Created,
            ActivityAction::StatusChanged,
            ActivityAction::AssigneeChanged,
            ActivityAction::PriorityChanged,
            ActivityAction::TitleChanged,
            ActivityAction::DueDateChanged,
            ActivityAction::TaskCompleted,
            ActivityAction::TaskFailed,
        ] {
            assert_eq!(ActivityAction::parse(a.as_db_str()), Some(a));
            assert!(!a.label().is_empty());
        }
    }

    #[test]
    fn unknown_action_token_decodes_to_none() {
        assert_eq!(ActivityAction::parse("teleported"), None);
    }

    #[test]
    fn system_actor_stores_no_id() {
        let s = ActivityActor::System;
        assert_eq!(s.type_str(), "system");
        assert_eq!(s.id(), None);
        assert_eq!(ActivityActor::parse("system", None), Some(s));
    }

    #[test]
    fn actor_round_trips_through_parse() {
        let a = ActivityActor::Actor(ActorRef::new(ActorKind::Agent, "a1").unwrap());
        assert_eq!(ActivityActor::parse(a.type_str(), a.id()), Some(a));
    }

    #[test]
    fn malformed_actor_pair_is_none() {
        assert_eq!(ActivityActor::parse("member", None), None);
        assert_eq!(ActivityActor::parse("frog", Some("x")), None);
    }

    #[test]
    fn member_or_system_falls_back_to_system() {
        assert_eq!(ActivityActor::member_or_system(None), ActivityActor::System);
        assert_eq!(
            ActivityActor::member_or_system(Some("")),
            ActivityActor::System
        );
        assert_eq!(
            ActivityActor::member_or_system(Some("m1")).type_str(),
            "member"
        );
    }
}
