//! Notification channels and the per-kind channel SET a routed attention lands on
//! (tcp T5 — notification routing rules).
//!
//! Every input request that reaches the attention inbox (architecture §4.3) can
//! be fanned out to zero or more *outbound* channels, on top of always appearing
//! on the control-centre board. The four channels are the four bus consumers the
//! converged control centre wires (architecture D10 / D18):
//!
//! ```text
//! attention row ──▶ Phone  (bridge outbound → Telegram/Slack/Discord)
//!               ├──▶ Web    (ainb-web VAPID push)
//!               ├──▶ Os     (notifyd OS notification)
//!               └──▶ Atc    (ATC feed inclusion)
//! ```
//!
//! A [`ChannelSet`] is the resolved routing decision for one attention: the set
//! of channels it should be delivered to. The board (TUI) is NOT a channel — it
//! always shows every open row — so an EMPTY set means "board-only" (the
//! `waiting` default), never "dropped".
//!
//! This vocabulary is deliberately shared: the store's rule resolver, the proto
//! wire row, the daemon's emit-time resolution, and every consumer's delivery
//! gate all key off the SAME [`Channel`] / [`ChannelSet`], so there is exactly
//! one notion of "which channels" across the whole fan-out (the multi-vocabulary
//! trap the pre-T5 consumers fell into, where the bridge, web push, and notifyd
//! each classified a different way).

use serde::de::{SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One outbound notification channel a routed attention can be delivered to.
///
/// The board (control-centre kanban) is intentionally NOT a channel — it renders
/// every open attention row regardless of routing — so these four are the
/// *push* surfaces only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// The bridge outbound worker → phone (Telegram / Slack / Discord).
    Phone,
    /// The web app's VAPID / web-push delivery.
    Web,
    /// The notifyd OS-level notification (macOS `osascript` / Linux `notify-send`).
    Os,
    /// Inclusion in the ATC feed the air-traffic-control session consumes.
    Atc,
}

impl Channel {
    /// The lowercase wire / DB token (`phone` / `web` / `os` / `atc`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Phone => "phone",
            Self::Web => "web",
            Self::Os => "os",
            Self::Atc => "atc",
        }
    }

    /// Parse a wire / DB token into a [`Channel`], or `None` for an unknown token.
    /// Case- and whitespace-tolerant so a hand-edited rule value still resolves.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "phone" => Some(Self::Phone),
            "web" => Some(Self::Web),
            "os" => Some(Self::Os),
            "atc" => Some(Self::Atc),
            _ => None,
        }
    }

    /// Every channel, in canonical order — the order [`ChannelSet`] serialises and
    /// the settings grid renders its columns.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Phone, Self::Web, Self::Os, Self::Atc]
    }

    /// This channel's bit in a [`ChannelSet`]'s bitset.
    const fn bit(self) -> u8 {
        match self {
            Self::Phone => 1 << 0,
            Self::Web => 1 << 1,
            Self::Os => 1 << 2,
            Self::Atc => 1 << 3,
        }
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The resolved set of channels one routed attention is delivered to.
///
/// Backed by a 4-bit bitset so it is `Copy` and a membership test
/// ([`ChannelSet::contains`]) is a single AND — the delivery gate every consumer
/// runs per attention. An empty set is "board-only" (no push), NOT "suppressed
/// everywhere including the board".
///
/// - Wire form: a JSON array of channel tokens in [`Channel::all`] order
///   (`["web","os"]`), so it reads naturally on the RPC surface.
/// - DB form: a canonical comma-separated token string ([`ChannelSet::to_db`] /
///   [`ChannelSet::from_db`]), e.g. `"web,os"` — empty string for the empty set.
///
/// Both forms are order-canonical (always [`Channel::all`] order), so a rule's
/// stored value and its wire echo are byte-stable regardless of the order the
/// channels were toggled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChannelSet(u8);

impl ChannelSet {
    /// The empty set — board-only delivery (no push channels).
    pub const NONE: Self = Self(0);

    /// A set from an iterator of channels.
    #[must_use]
    pub fn from_channels(channels: impl IntoIterator<Item = Channel>) -> Self {
        let mut bits = 0u8;
        for c in channels {
            bits |= c.bit();
        }
        Self(bits)
    }

    /// Whether `channel` is in the set. The per-attention delivery gate.
    #[must_use]
    pub const fn contains(self, channel: Channel) -> bool {
        self.0 & channel.bit() != 0
    }

    /// The set with `channel` added (idempotent).
    #[must_use]
    pub const fn with(self, channel: Channel) -> Self {
        Self(self.0 | channel.bit())
    }

    /// The set with `channel` removed (idempotent).
    #[must_use]
    pub const fn without(self, channel: Channel) -> Self {
        Self(self.0 & !channel.bit())
    }

    /// The set with `channel` toggled — the settings-grid cell action.
    #[must_use]
    pub const fn toggled(self, channel: Channel) -> Self {
        Self(self.0 ^ channel.bit())
    }

    /// Whether the set is empty (board-only, no push).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The channels in the set, in canonical [`Channel::all`] order.
    #[must_use]
    pub fn channels(self) -> Vec<Channel> {
        Channel::all().into_iter().filter(|&c| self.contains(c)).collect()
    }

    /// The canonical DB string: channel tokens in [`Channel::all`] order joined by
    /// commas (`"web,os"`), or the empty string for the empty set.
    #[must_use]
    pub fn to_db(self) -> String {
        self.channels().iter().map(|c| c.as_str()).collect::<Vec<_>>().join(",")
    }

    /// Parse a DB string back into a set. Tolerant: whitespace and unknown tokens
    /// are ignored (a corrupt rule value degrades to the channels it CAN parse
    /// rather than erroring), and an empty / all-whitespace string is the empty
    /// set.
    #[must_use]
    pub fn from_db(s: &str) -> Self {
        Self::from_channels(s.split(',').filter_map(Channel::parse))
    }
}

// Wire form: a JSON array of channel tokens (`["web","os"]`), always in
// canonical order. A bespoke impl (rather than deriving over the bitset) keeps
// the wire human-readable and order-stable, and tolerates unknown tokens on the
// way in the same way `from_db` does.
impl Serialize for ChannelSet {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let chans = self.channels();
        let mut seq = serializer.serialize_seq(Some(chans.len()))?;
        for c in chans {
            seq.serialize_element(&c)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for ChannelSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SetVisitor;
        impl<'de> Visitor<'de> for SetVisitor {
            type Value = ChannelSet;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an array of channel tokens")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ChannelSet, A::Error> {
                let mut set = ChannelSet::NONE;
                while let Some(c) = seq.next_element::<Channel>()? {
                    set = set.with(c);
                }
                Ok(set)
            }
        }
        deserializer.deserialize_seq(SetVisitor)
    }
}

impl FromIterator<Channel> for ChannelSet {
    fn from_iter<I: IntoIterator<Item = Channel>>(iter: I) -> Self {
        Self::from_channels(iter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_token_round_trip() {
        for c in Channel::all() {
            assert_eq!(Channel::parse(c.as_str()), Some(c));
        }
        assert_eq!(Channel::parse("  Phone "), Some(Channel::Phone));
        assert_eq!(Channel::parse("PagerDuty"), None);
    }

    #[test]
    fn set_membership_and_mutation() {
        let s = ChannelSet::from_channels([Channel::Web, Channel::Os]);
        assert!(s.contains(Channel::Web));
        assert!(s.contains(Channel::Os));
        assert!(!s.contains(Channel::Phone));
        assert!(!s.contains(Channel::Atc));

        // with / without are idempotent.
        assert_eq!(s.with(Channel::Web), s);
        assert_eq!(s.without(Channel::Phone), s);
        assert!(s.with(Channel::Phone).contains(Channel::Phone));
        assert!(!s.without(Channel::Web).contains(Channel::Web));

        // toggled flips exactly one channel.
        assert!(s.toggled(Channel::Phone).contains(Channel::Phone));
        assert!(!s.toggled(Channel::Web).contains(Channel::Web));
    }

    #[test]
    fn empty_set_is_board_only() {
        assert!(ChannelSet::NONE.is_empty());
        assert_eq!(ChannelSet::NONE.to_db(), "");
        assert!(ChannelSet::NONE.channels().is_empty());
    }

    #[test]
    fn db_string_is_canonical_regardless_of_insert_order() {
        // Inserted os-then-web and web-then-os both canonicalise to `phone`-order.
        let a = ChannelSet::from_channels([Channel::Os, Channel::Web]);
        let b = ChannelSet::from_channels([Channel::Web, Channel::Os]);
        assert_eq!(a.to_db(), "web,os");
        assert_eq!(a.to_db(), b.to_db());
        assert_eq!(ChannelSet::from_db("web,os"), a);
        // Full set is canonical `phone,web,os,atc`.
        let all = ChannelSet::from_channels(Channel::all());
        assert_eq!(all.to_db(), "phone,web,os,atc");
    }

    #[test]
    fn from_db_is_tolerant() {
        // Whitespace and unknown tokens are dropped, not fatal.
        assert_eq!(
            ChannelSet::from_db(" web , pager , os "),
            ChannelSet::from_channels([Channel::Web, Channel::Os])
        );
        assert_eq!(ChannelSet::from_db(""), ChannelSet::NONE);
        assert_eq!(ChannelSet::from_db("   "), ChannelSet::NONE);
    }

    #[test]
    fn wire_form_is_a_token_array_in_canonical_order() {
        let s = ChannelSet::from_channels([Channel::Atc, Channel::Phone]);
        let json = serde_json::to_string(&s).unwrap();
        // Canonical order: phone before atc.
        assert_eq!(json, r#"["phone","atc"]"#);
        let back: ChannelSet = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        // Empty set round-trips as [].
        assert_eq!(serde_json::to_string(&ChannelSet::NONE).unwrap(), "[]");
        assert_eq!(
            serde_json::from_str::<ChannelSet>("[]").unwrap(),
            ChannelSet::NONE
        );
    }
}
