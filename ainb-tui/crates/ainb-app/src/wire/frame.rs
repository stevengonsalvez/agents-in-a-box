// ABOUTME: The host half of the D15 renderer contract: named per-section frames,
// produced only for sections that changed and that the renderer subscribed to.
//
//   AppState ──versions()──▶ Mirror ──changed ∩ subscribed──▶ FrameBatch ──▶ channel
//
// A frame names its section, carries the section's version and the host it
// came from, and its body is `section_json`, so the redaction and the four leak
// checks from #983 apply to every byte a renderer receives. Nothing else in the
// crate builds a frame body.

use crate::app::AppState;
use crate::app::versioned::SectionId;
use crate::wire::{section_json, section_name};
use serde::{Deserialize, Serialize};

/// The host a frame or a row came from. One process-wide id per host; rows
/// read from another host's daemon carry that daemon's id instead.
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
/// `read_clock_ms` is the daemon's own clock at the read. A renderer computes
/// an age as `read_clock_ms - since_ms`, both on the daemon's clock, and never
/// subtracts a remote timestamp from its local now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonRead {
    pub revision: i64,
    pub clock_ms: i64,
}

/// One section's state as a renderer receives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    /// Stable wire name, [`section_name`].
    pub section: String,
    /// The section's [`Versioned`](crate::app::versioned::Versioned) version.
    pub version: u64,
    pub host_id: HostId,
    /// Present on sections whose content comes from a daemon read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_read: Option<DaemonRead>,
    /// `section_json` for the section: redacted by construction.
    pub body: serde_json::Value,
}

impl Frame {
    /// The section this frame names, if the wire name is one this build knows.
    #[must_use]
    pub fn section_id(&self) -> Option<SectionId> {
        section_id_from_name(&self.section)
    }
}

/// Everything one host tick sends down the channel.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameBatch {
    pub frames: Vec<Frame>,
}

impl FrameBatch {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

/// The inverse of [`section_name`].
#[must_use]
pub fn section_id_from_name(name: &str) -> Option<SectionId> {
    SectionId::ALL.into_iter().find(|id| section_name(*id) == name)
}

/// The sections a renderer wants frames for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subscription([bool; SectionId::COUNT]);

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
    subscription: Subscription,
    sent: [Option<u64>; SectionId::COUNT],
    daemon_read: DaemonReadSource,
}

impl std::fmt::Debug for Mirror {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mirror")
            .field("host_id", &self.host_id)
            .field("subscription", &self.subscription)
            .field("sent", &self.sent)
            .finish_non_exhaustive()
    }
}

impl Mirror {
    /// A mirror for a renderer that has seen nothing yet.
    #[must_use]
    pub fn new(host_id: HostId, subscription: Subscription) -> Self {
        Self {
            host_id,
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
        let owed: Vec<SectionId> = self
            .subscription
            .sections()
            .filter(|id| self.sent[id.index()] != Some(versions[id.index()]))
            .collect();
        let frames = owed
            .into_iter()
            .map(|id| {
                self.sent[id.index()] = Some(versions[id.index()]);
                Frame {
                    section: section_name(id).to_string(),
                    version: versions[id.index()],
                    host_id: self.host_id.clone(),
                    daemon_read: (self.daemon_read)(state, id),
                    body: section_json(state, id),
                }
            })
            .collect();
        FrameBatch { frames }
    }
}
