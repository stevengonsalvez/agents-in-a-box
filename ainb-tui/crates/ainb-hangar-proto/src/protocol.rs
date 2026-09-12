//! The ONE wire-protocol integer and the ONE capability catalogue (D17).
//!
//! Two different questions used to be answered by two different numbers, and a
//! client had no way to ask the first one at all:
//!
//! * *"can this build talk to that build?"*, answered here, by
//!   [`PROTOCOL_VERSION`] carried as a `{min, max}` range in `auth/hello`.
//! * *"does that build serve the method I am about to call?"*, answered by a
//!   capability string in [`CAPABILITY_CATALOGUE`].
//!
//! ```text
//! client ──auth/hello { protocol: {min,max}, capabilities }──▶ daemon
//!        ◀─{ protocol: {min,max}, selected, capabilities }──── overlap or 4409
//! ```
//!
//! ## Bump rule
//!
//! [`PROTOCOL_VERSION`] bumps ONLY when a peer that does not know about the
//! change would misread the wire:
//!
//! * a method or a field is REMOVED,
//! * a field's MEANING changes,
//! * framing, auth, or crypto changes.
//!
//! A new method, a new optional field, and a new event kind are NOT bumps,
//! they are capability strings. That rule is what lets an app-store phone keep
//! working: it cannot be force-upgraded, so the integer has to move rarely
//! enough that the range overlap survives a release the user never installed.
//!
//! ## Why the fleet integer does not move
//!
//! [`crate::fleet::FLEET_PROTOCOL_VERSION`] is frozen at 2 forever and
//! `fleet/negotiate` stays as a compatibility echo. Two integers would be worse
//! than none: a client would have to reason about which one governs a given
//! frame. The fleet capability ids are therefore APPENDED to the one catalogue
//! here, and `fleet/negotiate` keeps answering its own subset for the clients
//! that already call it.

use serde::{Deserialize, Serialize};

/// The highest Hangar wire-protocol version this build speaks.
///
/// Version 1 is the shape that exists the day W0-wire lands: JSON-RPC 2.0 over
/// LSP `Content-Length` framing on a unix socket, `auth/hello` first frame,
/// bearer token auth.
pub const PROTOCOL_VERSION: u32 = 1;

/// The lowest Hangar wire-protocol version this build still serves.
///
/// Equal to [`PROTOCOL_VERSION`] today. It moves only when an old version is
/// dropped, which is itself a [`PROTOCOL_VERSION`] bump.
pub const PROTOCOL_MIN_SUPPORTED: u32 = 1;

/// JSON-RPC error code for "our protocol ranges do not overlap".
///
/// Distinct from [`crate::auth::UNAUTHORIZED`] on purpose: a client that
/// presented a good token and a bad version must retry with a different BUILD,
/// never with a different credential. Mirrors the off-box WebSocket close code
/// 4409 that R1 will add for the same condition.
pub const PROTOCOL_INCOMPATIBLE: i32 = -32007;

/// An inclusive protocol-version range, as carried in `auth/hello`.
///
/// A range rather than an exact integer because neither peer can force the
/// other to upgrade: the overlap is the contract, and an empty overlap is the
/// only refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolRange {
    /// Lowest version this peer can speak.
    pub min: u32,
    /// Highest version this peer can speak.
    pub max: u32,
}

impl ProtocolRange {
    /// The range this build speaks.
    #[must_use]
    pub const fn supported() -> Self {
        Self {
            min: PROTOCOL_MIN_SUPPORTED,
            max: PROTOCOL_VERSION,
        }
    }

    /// The range assumed for a peer that sent no `protocol` member at all.
    ///
    /// Every pre-W0-wire client is exactly this: it speaks version 1 and
    /// nothing else. Pinned to the literal `1` rather than to
    /// [`PROTOCOL_MIN_SUPPORTED`] so a future release that drops version 1 does
    /// not silently re-label those clients as speaking whatever the new floor
    /// is, they would then negotiate a version they have never heard of.
    #[must_use]
    pub const fn legacy() -> Self {
        Self { min: 1, max: 1 }
    }

    /// Whether this is a non-empty range.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.min > 0 && self.min <= self.max
    }

    /// Whether `version` falls inside this inclusive range.
    #[must_use]
    pub const fn contains(self, version: u32) -> bool {
        self.min <= version && version <= self.max
    }
}

impl Default for ProtocolRange {
    fn default() -> Self {
        Self::legacy()
    }
}

/// The version two peers agree on: the highest both can speak, or `None` when
/// the ranges do not overlap.
///
/// Highest-common rather than lowest-common: a fix that shipped in a newer
/// version is worth having whenever both ends have it, and the older peer's
/// `max` is exactly its statement of what it can still read.
#[must_use]
pub const fn negotiate(client: ProtocolRange, daemon: ProtocolRange) -> Option<u32> {
    if !client.is_valid() || !daemon.is_valid() {
        return None;
    }
    let low = if client.min > daemon.min {
        client.min
    } else {
        daemon.min
    };
    let high = if client.max < daemon.max {
        client.max
    } else {
        daemon.max
    };
    if low <= high { Some(high) } else { None }
}

/// Capability: the daemon negotiates the protocol integer inside `auth/hello`
/// and answers with its own range and catalogue.
///
/// A client that does not see this string is talking to a pre-W0-wire daemon:
/// the hello reply is a bare `{}`, so it must assume protocol 1 and the
/// capability set implied by `fleet/negotiate` alone.
pub const CAP_AUTH_HELLO_NEGOTIATED: &str = "hangar.auth.hello.negotiated";
/// Capability: mutations carry an opaque op id and are deduplicated at
/// dispatch against a durable ledger (D18 tier 1).
pub const CAP_MUTATION_OP_ID: &str = "hangar.mutation.op_id";
/// Capability: PTY-effecting mutations additionally carry a transactional
/// receipt with the `writing` boundary (D18 tier 2).
pub const CAP_MUTATION_RECEIPT: &str = "hangar.mutation.receipt";
/// Capability: the daemon symlinks `hangar-v<N>.sock` beside `hangar.sock` for
/// every protocol version it serves.
pub const CAP_SOCKET_VERSIONED: &str = "hangar.socket.versioned";
/// Capability: the in-memory live surface registry (`hangar/connections_list`
/// plus the `ConnectionsChanged` event).
pub const CAP_CONNECTIONS_REGISTRY: &str = "hangar.connections.registry";
/// Capability: the converged attention inbox, list, subscribe, answer.
pub const CAP_ATTENTION_INBOX: &str = "hangar.attention.inbox";
/// Capability: the wire attention row carries its `version`, so a client can
/// fence `attention/answer` on the row it actually read (D18).
///
/// A client that does not see this string is talking to a daemon whose rows
/// report `0`, and must send no fence rather than a fence it made up.
pub const CAP_ATTENTION_FENCE: &str = "hangar.attention.fence";
/// Capability: workspace event subscription with a durable cursor.
pub const CAP_WORKSPACE_SUBSCRIBE: &str = "hangar.workspace.subscribe";
/// Capability: issue read surface, including search and the activity timeline.
pub const CAP_ISSUE_READ: &str = "hangar.issue.read";
/// Capability: issue write surface, update, labels, links, properties,
/// reactions, subscriptions, delete.
pub const CAP_ISSUE_WRITE: &str = "hangar.issue.write";
/// Capability: board read surface.
pub const CAP_BOARD_READ: &str = "hangar.board.read";
/// Capability: board write surface, boards, columns, cards, dependencies.
pub const CAP_BOARD_WRITE: &str = "hangar.board.write";
/// Capability: agent roster read surface.
pub const CAP_AGENT_READ: &str = "hangar.agent.read";
/// Capability: agent write surface, create, update, archive, delete.
pub const CAP_AGENT_WRITE: &str = "hangar.agent.write";
/// Capability: skill catalogue read surface.
pub const CAP_SKILL_READ: &str = "hangar.skill.read";
/// Capability: skill write surface, sync, attach, detach, enable.
pub const CAP_SKILL_WRITE: &str = "hangar.skill.write";
/// Capability: autopilot read surface, list, runs, versions, rosters.
pub const CAP_AUTOPILOT_READ: &str = "hangar.autopilot.read";
/// Capability: autopilot write surface, update, enable, triggers, rosters.
pub const CAP_AUTOPILOT_WRITE: &str = "hangar.autopilot.write";
/// Capability: squad read surface.
pub const CAP_SQUAD_READ: &str = "hangar.squad.read";
/// Capability: squad write surface, create, membership, assign, fan-out.
pub const CAP_SQUAD_WRITE: &str = "hangar.squad.write";
/// Capability: workspace membership read surface.
pub const CAP_MEMBER_READ: &str = "hangar.member.read";
/// Capability: workspace membership write surface, roles, removal, invites.
pub const CAP_MEMBER_WRITE: &str = "hangar.member.write";
/// Capability: the cross-surface inbox projection.
pub const CAP_INBOX_READ: &str = "hangar.inbox.read";
/// Capability: notification routing rules.
pub const CAP_NOTIFY_RULES: &str = "hangar.notify.rules";
/// Capability: daemon configuration get / set / list.
pub const CAP_DAEMON_CONFIG: &str = "hangar.daemon.config";
/// Capability: agent profile read surface.
pub const CAP_PROFILE_READ: &str = "hangar.profile.read";
/// Capability: agent profile write surface.
pub const CAP_PROFILE_WRITE: &str = "hangar.profile.write";
/// Capability: the custom property catalogue and issue metadata.
pub const CAP_PROPERTY_CATALOG: &str = "hangar.property.catalog";
/// Capability: task list, transition and retry.
pub const CAP_TASK_CONTROL: &str = "hangar.task.control";
/// Capability: bounded usage rollups.
pub const CAP_USAGE_ROLLUP: &str = "hangar.usage.rollup";
/// Capability: the run-history observability projection.
pub const CAP_RUN_HISTORY: &str = "hangar.run.history";
/// Capability: on-demand pull-request status refresh.
pub const CAP_PR_STATUS_REFRESH: &str = "hangar.pr_status.refresh";
/// Capability: daemon and store health probes.
pub const CAP_HEALTH: &str = "hangar.health";
/// Capability: the cross-entity command-palette search.
pub const CAP_SEARCH: &str = "hangar.search";
/// Capability: the card-create repository roster.
pub const CAP_REPO_LIST: &str = "hangar.repo.list";
/// Capability: dispatch reason codes.
pub const CAP_DISPATCH_ATTEMPTS: &str = "hangar.dispatch.attempts";
/// Capability: the ATC instance registry and its retry projection.
pub const CAP_ATC_REGISTRY: &str = "atc.registry";
/// Capability: Interactive Codex thread ensure / discard.
pub const CAP_CODEX_SESSION: &str = "codex.session";

/// The ONE capability catalogue: every string this build advertises.
///
/// Ordered and APPEND-ONLY. The Hangar strings come first, then the fleet ids
/// from [`crate::fleet::FLEET_PROTOCOL_CAPABILITY_IDS`] verbatim, D17's "its
/// 25 ids append to the one catalogue". A test in this module asserts that
/// every fleet id is present here, and
/// `tests/capability_catalogue.rs` asserts against the COMMITTED
/// `capabilities.catalogue` file that no string is ever removed.
///
/// Removing a string is a [`PROTOCOL_VERSION`] bump, never a quiet edit: a
/// client branches on these, so a vanished string is a vanished feature.
pub const CAPABILITY_CATALOGUE: &[&str] = &[
    CAP_AUTH_HELLO_NEGOTIATED,
    CAP_MUTATION_OP_ID,
    CAP_MUTATION_RECEIPT,
    CAP_SOCKET_VERSIONED,
    CAP_CONNECTIONS_REGISTRY,
    CAP_ATTENTION_INBOX,
    CAP_WORKSPACE_SUBSCRIBE,
    CAP_ISSUE_READ,
    CAP_ISSUE_WRITE,
    CAP_BOARD_READ,
    CAP_BOARD_WRITE,
    CAP_AGENT_READ,
    CAP_AGENT_WRITE,
    CAP_SKILL_READ,
    CAP_SKILL_WRITE,
    CAP_AUTOPILOT_READ,
    CAP_AUTOPILOT_WRITE,
    CAP_SQUAD_READ,
    CAP_SQUAD_WRITE,
    CAP_MEMBER_READ,
    CAP_MEMBER_WRITE,
    CAP_INBOX_READ,
    CAP_NOTIFY_RULES,
    CAP_DAEMON_CONFIG,
    CAP_PROFILE_READ,
    CAP_PROFILE_WRITE,
    CAP_PROPERTY_CATALOG,
    CAP_TASK_CONTROL,
    CAP_USAGE_ROLLUP,
    CAP_RUN_HISTORY,
    CAP_PR_STATUS_REFRESH,
    CAP_HEALTH,
    CAP_SEARCH,
    CAP_REPO_LIST,
    CAP_DISPATCH_ATTEMPTS,
    CAP_ATC_REGISTRY,
    CAP_CODEX_SESSION,
    // The 25 fleet ids, appended verbatim (D17). `fleet/negotiate` still
    // answers its own subset for the clients that already call it; this is the
    // same list, reachable from the one handshake.
    crate::fleet::FLEET_CAPABILITY_ACP_SPAWN,
    crate::fleet::FLEET_CAPABILITY_ACTION_EXECUTE,
    crate::fleet::FLEET_CAPABILITY_CHAT_READ,
    crate::fleet::FLEET_CAPABILITY_CHAT_WRITE,
    crate::fleet::FLEET_CAPABILITY_CONFIRM_ANSWER,
    crate::fleet::FLEET_CAPABILITY_PAL_CONFIGURE,
    crate::fleet::FLEET_CAPABILITY_PAL_GATE,
    crate::fleet::FLEET_CAPABILITY_ATC_READ,
    crate::fleet::FLEET_CAPABILITY_BROADCAST_EXECUTE,
    crate::fleet::FLEET_CAPABILITY_MESSAGE_READ,
    crate::fleet::FLEET_CAPABILITY_MESSAGE_SEND,
    "fleet.protocol.negotiate",
    crate::fleet::FLEET_CAPABILITY_RECEIPT_READ,
    crate::fleet::FLEET_CAPABILITY_QUOTA_READ,
    "fleet.snapshot.read",
    crate::fleet::FLEET_CAPABILITY_START_EXECUTE,
    "fleet.subscription.live",
    "fleet.subscription.replay",
    "fleet.subscription.resync",
    crate::fleet::FLEET_CAPABILITY_TIMELINE_READ,
    crate::fleet::FLEET_CAPABILITY_TRANSCRIPT_PRUNE,
    crate::fleet::FLEET_CAPABILITY_TRANSCRIPT_READ,
    crate::fleet::FLEET_CAPABILITY_RUNTIME_READ,
    crate::fleet::FLEET_CAPABILITY_USAGE_READ,
    crate::fleet::FLEET_CAPABILITY_DASHBOARD_READ,
    // APPENDED, never spliced: the catalogue is append-only and a committed
    // file records the order, so a new string goes after every existing one.
    CAP_ATTENTION_FENCE,
    crate::fleet::FLEET_CAPABILITY_STATUS_READ,
];

/// Whether this build advertises `id`.
#[must_use]
pub fn advertises(id: &str) -> bool {
    CAPABILITY_CATALOGUE.contains(&id)
}

/// The catalogue as owned strings, for a wire payload.
#[must_use]
pub fn catalogue_strings() -> Vec<String> {
    CAPABILITY_CATALOGUE.iter().map(|s| (*s).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue is a SET: a duplicated string means one of the two
    /// spellings is dead and nobody can tell which.
    #[test]
    fn catalogue_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for id in CAPABILITY_CATALOGUE {
            assert!(seen.insert(*id), "duplicate capability id {id:?}");
        }
    }

    /// D17: the fleet ids append to the ONE catalogue. A fleet id that is
    /// advertised by `fleet/negotiate` but missing here would make the two
    /// handshakes disagree about what the same daemon serves.
    #[test]
    fn every_fleet_capability_is_in_the_one_catalogue() {
        for id in crate::fleet::FLEET_PROTOCOL_CAPABILITY_IDS {
            assert!(
                CAPABILITY_CATALOGUE.contains(id),
                "fleet capability {id:?} is not in the catalogue"
            );
        }
        assert_eq!(
            crate::fleet::FLEET_PROTOCOL_CAPABILITY_IDS.len(),
            25,
            "D17 names 25 fleet ids; the append rule is written against that count"
        );
    }

    /// Overlap is the contract, and the highest common version wins.
    #[test]
    fn negotiation_picks_the_highest_common_version() {
        let table = [
            (
                ProtocolRange { min: 1, max: 1 },
                ProtocolRange { min: 1, max: 1 },
                Some(1),
            ),
            (
                ProtocolRange { min: 1, max: 2 },
                ProtocolRange { min: 1, max: 1 },
                Some(1),
            ),
            (
                ProtocolRange { min: 1, max: 1 },
                ProtocolRange { min: 1, max: 3 },
                Some(1),
            ),
            (
                ProtocolRange { min: 2, max: 4 },
                ProtocolRange { min: 3, max: 9 },
                Some(4),
            ),
            (
                ProtocolRange { min: 1, max: 1 },
                ProtocolRange { min: 2, max: 3 },
                None,
            ),
            (
                ProtocolRange { min: 5, max: 6 },
                ProtocolRange { min: 1, max: 4 },
                None,
            ),
            (
                ProtocolRange { min: 3, max: 1 },
                ProtocolRange { min: 1, max: 4 },
                None,
            ),
            (
                ProtocolRange { min: 0, max: 0 },
                ProtocolRange { min: 1, max: 1 },
                None,
            ),
        ];
        for (client, daemon, want) in table {
            assert_eq!(negotiate(client, daemon), want, "{client:?} vs {daemon:?}");
        }
    }

    /// A peer that sent no `protocol` member speaks version 1 and only 1,
    /// pinned to the literal, so dropping version 1 later cannot relabel it.
    #[test]
    fn the_legacy_range_is_exactly_version_one() {
        assert_eq!(ProtocolRange::legacy(), ProtocolRange { min: 1, max: 1 });
        assert_eq!(ProtocolRange::default(), ProtocolRange::legacy());
    }

    /// This build's own range is non-empty and contains the current version.
    #[test]
    fn the_supported_range_is_valid() {
        let range = ProtocolRange::supported();
        assert!(range.is_valid());
        assert!(range.contains(PROTOCOL_VERSION));
        assert!(negotiate(ProtocolRange::legacy(), range).is_some());
    }

    /// The incompatible code sits in the JSON-RPC server-error range and is
    /// distinct from every other code this crate defines.
    #[test]
    fn protocol_incompatible_is_a_distinct_server_error_code() {
        assert!((-32099..=-32000).contains(&PROTOCOL_INCOMPATIBLE));
        assert_ne!(PROTOCOL_INCOMPATIBLE, crate::auth::UNAUTHORIZED);
        assert_ne!(PROTOCOL_INCOMPATIBLE, crate::STORE_UNAVAILABLE);
    }
}
