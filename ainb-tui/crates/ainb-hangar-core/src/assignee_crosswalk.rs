//! Polymorphic assignee crosswalk between Hangar actors and `bd` assignee
//! strings (P2.3).
//!
//! `bd` carries a single flat `assignee: String`, whereas Hangar models an
//! assignee as a polymorphic [`ActorRef`] — a `(kind, id)` pair. The sync layer
//! needs a *single source of truth* for translating between the two so the
//! outbound mirror and the inbound reconcile agree on the wire shape.
//!
//! # The mapping (single source of truth)
//!
//! | Hangar                                  | `bd` assignee   |
//! |-----------------------------------------|-----------------|
//! | `Some(ActorRef { Agent,  "ag_x" })`     | `agent:ag_x`    |
//! | `Some(ActorRef { Member, "stevie" })`   | `stevie`        |
//! | `None`                                  | *(unassigned)*  |
//!
//! Agent ids are prefixed `agent:` on the bd side (matching the canonical
//! [`ActorRef`] `Display` form for agents); member ids are emitted raw. The two
//! directions are mutual inverses, exercised by the round-trip property test in
//! this module.
//!
//! # Round-trip invariant
//!
//! For every constructible [`ActorRef`] `x`:
//! `bd_to_hangar(&hangar_to_bd(&Some(x))) == Some(x)`. A `member:`-prefixed id
//! is the one degenerate case the property test deliberately excludes: a member
//! whose id literally began with `member:` cannot be distinguished from a raw
//! member id by [`bd_to_hangar`] (which only special-cases the `agent:`
//! prefix). The issue-create path rejects such ids before they reach the
//! crosswalk (per the P2 "user assignee string collides with `agent:` prefix"
//! risk row), so the property holds for every id the crosswalk actually sees.

use crate::actor::{ActorKind, ActorRef};

/// The `agent:` prefix that distinguishes an agent assignee on the bd side.
///
/// This is exactly the canonical [`ActorRef`] `Display` prefix for an agent, so
/// the two representations stay aligned by construction.
const AGENT_PREFIX: &str = "agent:";

/// Translate a Hangar assignee into the flat `bd` assignee string.
///
/// An [`ActorKind::Agent`] actor is rendered as `agent:<id>`; an
/// [`ActorKind::Member`] actor is rendered as its raw id. The unassigned case
/// (`None`) is the caller's concern: `assignee.as_ref().map(hangar_to_bd)`
/// yields the `Option<String>` shape `bd`'s `--assignee` flag expects.
#[must_use]
pub fn hangar_to_bd(actor: &ActorRef) -> String {
    match actor.kind() {
        ActorKind::Agent => format!("{AGENT_PREFIX}{}", actor.id()),
        ActorKind::Member => actor.id().to_string(),
    }
}

/// Translate a flat `bd` assignee string back into a Hangar assignee.
///
/// `None` (unassigned) yields `None`. A string carrying the `agent:` prefix
/// becomes an [`ActorKind::Agent`] actor (prefix stripped); any other string is
/// treated as a raw [`ActorKind::Member`] id.
///
/// # Errors
///
/// Returns [`CrosswalkError::EmptyId`] when the resulting id half would be empty
/// (a bare `"agent:"` or an empty assignee string), which cannot map to a valid
/// non-empty [`ActorRef`].
pub fn bd_to_hangar(assignee: Option<&str>) -> Result<Option<ActorRef>, CrosswalkError> {
    let Some(raw) = assignee else {
        return Ok(None);
    };
    let (kind, id) = raw.strip_prefix(AGENT_PREFIX).map_or_else(
        || (ActorKind::Member, raw),
        |stripped| (ActorKind::Agent, stripped),
    );
    let actor = ActorRef::new(kind, id).map_err(|_| CrosswalkError::EmptyId)?;
    Ok(Some(actor))
}

/// Failure modes of the assignee crosswalk.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CrosswalkError {
    /// The bd assignee string mapped to an empty Hangar id (e.g. a bare
    /// `"agent:"` or the empty string).
    #[error("bd assignee maps to an empty hangar actor id")]
    EmptyId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_actor_gets_agent_prefix() {
        let agent = ActorRef::new(ActorKind::Agent, "ag_x").expect("actor");
        assert_eq!(hangar_to_bd(&agent), "agent:ag_x");
    }

    #[test]
    fn member_actor_is_raw() {
        let member = ActorRef::new(ActorKind::Member, "stevie").expect("actor");
        assert_eq!(hangar_to_bd(&member), "stevie");
    }

    #[test]
    fn none_maps_to_none_both_directions() {
        let unassigned: Option<&ActorRef> = None;
        assert_eq!(unassigned.map(hangar_to_bd), None);
        assert_eq!(bd_to_hangar(None), Ok(None));
    }

    #[test]
    fn bd_agent_prefix_becomes_agent_actor() {
        let got = bd_to_hangar(Some("agent:ag_x")).expect("ok").expect("some");
        assert_eq!(got.kind(), ActorKind::Agent);
        assert_eq!(got.id(), "ag_x");
    }

    #[test]
    fn bd_raw_string_becomes_member_actor() {
        let got = bd_to_hangar(Some("stevie")).expect("ok").expect("some");
        assert_eq!(got.kind(), ActorKind::Member);
        assert_eq!(got.id(), "stevie");
    }

    #[test]
    fn bare_agent_prefix_is_empty_id_error() {
        assert_eq!(bd_to_hangar(Some("agent:")), Err(CrosswalkError::EmptyId));
    }

    #[test]
    fn empty_string_is_empty_id_error() {
        assert_eq!(bd_to_hangar(Some("")), Err(CrosswalkError::EmptyId));
    }

    /// Round-trip property: `bd_to_hangar ∘ hangar_to_bd == identity` for every
    /// constructible actor whose member id does not itself begin with `agent:`
    /// (the documented degenerate case the issue-create path rejects upstream).
    #[test]
    fn round_trip_hangar_to_bd_to_hangar() {
        let cases = [
            ActorRef::new(ActorKind::Agent, "ag_x").unwrap(),
            ActorRef::new(ActorKind::Agent, "ag_long-id_123").unwrap(),
            ActorRef::new(ActorKind::Member, "stevie").unwrap(),
            ActorRef::new(ActorKind::Member, "user-with-dashes").unwrap(),
            ActorRef::new(ActorKind::Member, "a:b:c").unwrap(),
        ];
        for x in cases {
            let bd = hangar_to_bd(&x);
            let back = bd_to_hangar(Some(&bd)).expect("ok").expect("some");
            assert_eq!(back, x, "round-trip failed for {x}");
        }
    }
}
