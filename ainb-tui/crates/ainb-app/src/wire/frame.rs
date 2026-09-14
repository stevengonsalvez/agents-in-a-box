// ABOUTME: The host half of the D15 renderer contract: named per-section frames,
// produced only for sections that changed and that the renderer subscribed to.
//
//   AppState ──versions()──▶ Mirror ──changed ∩ subscribed──▶ FrameBatch ──▶ channel
//
// A frame names its section, carries the section's version, the boot epoch that
// version counts in and the host it came from, and its body is `section_json`, so the redaction and the four leak
// checks from #983 apply to every byte a renderer receives. Nothing else in the
// crate builds a frame body.

use crate::app::AppState;
use crate::app::versioned::SectionId;
use crate::wire::{section_json, section_name};
use serde::{Deserialize, Serialize};

/// The host a frame or a row came from. One process-wide id per host; rows
/// read from another host's daemon carry that daemon's id instead.
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostId(String);

impl HostId {
    /// The id every surface on this machine uses until a daemon mints one.
    pub const LOCAL: &'static str = "local";

    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn local() -> Self {
        Self(Self::LOCAL.to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where a section's content was read, for sections fed by a daemon.
///
/// `clock_ms` is the daemon's own clock at the read. A renderer computes
/// an age as `clock_ms - since_ms`, both on the daemon's clock, and never
/// subtracts a remote timestamp from its local now.
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonRead {
    pub revision: i64,
    pub clock_ms: i64,
}

/// The boot epoch of this host process: minted once, on first use, from the
/// wall clock in nanoseconds, so a restarted host sends a larger epoch.
///
/// Section versions restart at zero with the process. A renderer that kept a
/// host's old versions would drop every frame of the new process as a replay
/// and stay silently stale; the epoch tells it the versions started over.
#[must_use]
pub fn host_epoch() -> u64 {
    static EPOCH: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |since| {
                u64::try_from(since.as_nanos()).unwrap_or(u64::MAX).max(1)
            })
    })
}

/// One section's state as a renderer receives it.
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    /// Stable wire name, [`section_name`].
    pub section: String,
    /// The section's [`Versioned`](crate::app::versioned::Versioned) version.
    /// Ordered only within one [`Self::epoch`].
    pub version: u64,
    /// The sending host process's [`host_epoch`]. A renderer that sees a larger
    /// epoch from a host drops everything it held from that host first.
    pub epoch: u64,
    pub host_id: HostId,
    /// Present on sections whose content comes from a daemon read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_read: Option<DaemonRead>,
    /// `section_json` for the section: redacted by construction. In TypeScript
    /// it is `unknown`; `SectionBodies[frame.section]` names its shape.
    ///
    /// Private: the host side can only fill it through [`Self::new`], so no
    /// frame body comes from anywhere but the redacting serializer.
    #[cfg_attr(feature = "typescript-bindings", specta(type = specta_typescript::Unknown))]
    body: serde_json::Value,
}

impl Frame {
    /// The frame for one section of `state`: its wire name, version, daemon
    /// read and redacted body, from this host process ([`HostId::local`],
    /// [`host_epoch`]).
    #[must_use]
    pub fn new(state: &AppState, id: SectionId) -> Self {
        Self {
            section: section_name(id).to_string(),
            version: state.versions()[id.index()],
            epoch: host_epoch(),
            host_id: HostId::local(),
            daemon_read: crate::wire::daemon_read(state, id),
            body: section_json(state, id),
        }
    }

    /// The redacted section body.
    #[must_use]
    pub const fn body(&self) -> &serde_json::Value {
        &self.body
    }

    /// Take the body out, for a store that keeps it.
    #[must_use]
    pub fn into_body(self) -> serde_json::Value {
        self.body
    }

    /// The section this frame names, if the wire name is one this build knows.
    #[must_use]
    pub fn section_id(&self) -> Option<SectionId> {
        section_id_from_name(&self.section)
    }
}

/// Everything one host tick sends down the channel.
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameBatch {
    pub frames: Vec<Frame>,
}

impl FrameBatch {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

/// The inverse of [`section_name`].
#[must_use]
pub fn section_id_from_name(name: &str) -> Option<SectionId> {
    SectionId::ALL.into_iter().find(|id| section_name(*id) == name)
}

/// The sections a renderer wants frames for.
///
/// On the wire, the list of wire names ([`section_name`]), so a remote renderer
/// sends its filter to the host. A name this build does not know is skipped:
/// a newer renderer can ask an older host for a section it lacks.
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[cfg_attr(feature = "typescript-bindings", specta(type = Vec<String>))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subscription([bool; SectionId::COUNT]);

impl Serialize for Subscription {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.sections().map(section_name))
    }
}

impl<'de> Deserialize<'de> for Subscription {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let names = Vec::<String>::deserialize(deserializer)?;
        let ids: Vec<SectionId> =
            names.iter().filter_map(|name| section_id_from_name(name)).collect();
        Ok(Self::only(&ids))
    }
}

impl Subscription {
    /// Every section.
    #[must_use]
    pub const fn all() -> Self {
        Self([true; SectionId::COUNT])
    }

    /// No section.
    #[must_use]
    pub const fn none() -> Self {
        Self([false; SectionId::COUNT])
    }

    /// Exactly `sections`.
    #[must_use]
    pub fn only(sections: &[SectionId]) -> Self {
        let mut subscription = Self::none();
        for id in sections {
            subscription.0[id.index()] = true;
        }
        subscription
    }

    #[must_use]
    pub const fn contains(&self, id: SectionId) -> bool {
        self.0[id.index()]
    }

    /// The subscribed sections, in [`SectionId::ALL`] order.
    pub fn sections(&self) -> impl Iterator<Item = SectionId> + '_ {
        SectionId::ALL.into_iter().filter(|id| self.contains(*id))
    }
}

/// Supplies the daemon read behind a section, when it has one.
pub type DaemonReadSource = fn(&AppState, SectionId) -> Option<DaemonRead>;

/// The host side of one renderer's channel.
///
/// Remembers the version of every subscribed section it last framed. A
/// section enters the next batch when its version moved or the renderer has
/// never been sent it; unsubscribed sections are never framed, whatever they
/// do.
pub struct Mirror {
    host_id: HostId,
    epoch: u64,
    subscription: Subscription,
    sent: [Option<u64>; SectionId::COUNT],
    daemon_read: DaemonReadSource,
}

impl std::fmt::Debug for Mirror {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mirror")
            .field("host_id", &self.host_id)
            .field("epoch", &self.epoch)
            .field("subscription", &self.subscription)
            .field("sent", &self.sent)
            .finish_non_exhaustive()
    }
}

impl Mirror {
    /// A mirror for a renderer that has seen nothing yet, stamped with this
    /// process's [`host_epoch`].
    #[must_use]
    pub fn new(host_id: HostId, subscription: Subscription) -> Self {
        Self::with_epoch(host_id, subscription, host_epoch())
    }

    /// A mirror stamped with an explicit epoch: a host restart in a test.
    #[must_use]
    pub fn with_epoch(host_id: HostId, subscription: Subscription, epoch: u64) -> Self {
        Self {
            host_id,
            epoch,
            subscription,
            sent: [None; SectionId::COUNT],
            daemon_read: crate::wire::daemon_read,
        }
    }

    #[must_use]
    pub const fn subscription(&self) -> Subscription {
        self.subscription
    }

    /// Change what the renderer wants. A newly added section is framed in full
    /// on the next batch; a dropped one stops.
    pub fn resubscribe(&mut self, subscription: Subscription) {
        for id in SectionId::ALL {
            if !subscription.contains(id) {
                self.sent[id.index()] = None;
            }
        }
        self.subscription = subscription;
    }

    /// The frames `state` owes this renderer, marking them sent.
    #[must_use]
    pub fn batch(&mut self, state: &AppState) -> FrameBatch {
        let versions = state.versions();
        let mut frames = Vec::new();
        for id in SectionId::ALL {
            let version = versions[id.index()];
            if !self.subscription.contains(id) || self.sent[id.index()] == Some(version) {
                continue;
            }
            self.sent[id.index()] = Some(version);
            frames.push(Frame {
                epoch: self.epoch,
                host_id: self.host_id.clone(),
                daemon_read: (self.daemon_read)(state, id),
                ..Frame::new(state, id)
            });
        }
        FrameBatch { frames }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subscription_travels_as_section_names_and_skips_unknown_ones() {
        let subscription = Subscription::only(&[SectionId::Shell, SectionId::AgentStatus]);
        let wire = serde_json::to_value(subscription).expect("serialises");
        assert_eq!(wire, serde_json::json!(["shell", "agent_status"]));

        let from_newer: Subscription = serde_json::from_value(serde_json::json!([
            "agent_status",
            "a_future_section",
            "shell"
        ]))
        .expect("deserialises");
        assert_eq!(from_newer, subscription);
    }
}
