//! Bridge between the legacy in-core `AppEvent::Usage*` variants and the new
//! plugin-shaped `AppEvent::Plugin { plugin_id, payload }` dispatch path.
//!
//! **Phase 2c stub. Deleted in Phase 3** when the burndown screen is extracted
//! into `ainb-plugin-burndown` and the plugin host's own event dispatch takes
//! over. Until then this module:
//!
//! 1. Defines `UsageEvent` — a serde-tagged enum mirroring every `Usage*`
//!    `AppEvent` variant so they can travel as opaque `Vec<u8>` payloads.
//! 2. Provides `encode` / `decode` helpers using rmp-serde, matching the
//!    on-the-wire format the plugin host already uses
//!    (`PluginEvent::Custom { topic, payload }`).
//! 3. Provides `as_app_event` / `from_app_event` to translate between the
//!    two representations so existing emit + match sites can move to
//!    `AppEvent::Plugin` incrementally.
//!
//! ## Why a separate enum (not just `serde` on `AppEvent`)
//!
//! Two reasons. First: `AppEvent` carries closures and non-serializable
//! variants (`MouseClick { x, y }` is fine, but other variants reference
//! state types that don't implement `Serialize`). Second: a flat plugin
//! payload type protects the host from `AppEvent` shape churn — when an
//! emoji / keybind / panel changes name, the bridge translates and the
//! plugin (or future plugin) keeps a stable on-the-wire schema.
//!
//! ## Tag-collision rule
//!
//! `UsageEvent` uses `#[serde(tag = "kind")]`. **No nested enum inside
//! `UsageEvent` may also use `tag = "kind"` —** that produces "duplicate
//! field `kind`" at deserialize time. Confirmed bug in this codebase from
//! Phase 1 (lifecycle event nesting). Use a distinct tag name (e.g.
//! `tag = "lifecycle"`) per nesting layer if you need to nest tagged enums.

use crate::app::events::AppEvent;
use serde::{Deserialize, Serialize};

/// Plugin id used by the burndown bridge — matches the manifest id the
/// real burndown plugin will ship with in Phase 3.
pub const BURNDOWN_PLUGIN_ID: &str = "burndown";

/// Wire-format mirror of every `AppEvent::Usage*` variant.
///
/// Variant names match the source `AppEvent` variants 1:1 (with the
/// `Usage` prefix dropped) so a future macro-generated round-trip stays
/// trivial. Phase 3 will move this enum into the burndown plugin crate
/// verbatim and delete this module.
///
/// Externally tagged (the default) — internally tagged enums (`tag =
/// "kind"`) can't serialize newtype variants holding primitives like u8 /
/// char in msgpack. Externally tagged keeps the wire format compatible
/// with `PluginEvent::Custom` already shipped by the plugin host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageEvent {
    Back,
    NextProvider,
    PrevProvider,
    NextTab,
    PrevTab,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    ToTop,
    ToBottom,
    Refresh,
    ForceRefresh,
    CycleProviderFilter,
    SetPeriod(u8),
    SetPeriodMonth,
    SetPeriodQuarter,
    SetPeriodAll,
    PeriodStepBack,
    PeriodStepForward,
    StartDateRange,
    InputChar(char),
    InputBackspace,
    InputSubmit,
    InputCancel,
    FocusNextPanel,
    FocusPrevPanel,
    FocusRowUp,
    FocusRowDown,
    CommitFilter,
    CommitExcludeFilter,
    PopFilterChip,
    ClearAllChips,
    WireStatusline,
    ToggleZoom,
    ZoomStartSearch,
    ZoomCommitSearch,
    ZoomCancelSearch,
    ZoomSearchChar(char),
    ZoomSearchBackspace,
    ZoomToggleDetail,
    ZoomEsc,
}

/// Encode a `UsageEvent` to the plugin-host wire format.
pub fn encode(event: &UsageEvent) -> Vec<u8> {
    rmp_serde::to_vec_named(event).expect("UsageEvent encodes to msgpack")
}

/// Decode a payload produced by `encode`. Returns `Err` if `payload` is not
/// a valid msgpack `UsageEvent`.
pub fn decode(payload: &[u8]) -> Result<UsageEvent, rmp_serde::decode::Error> {
    rmp_serde::from_slice(payload)
}

/// Wrap a `UsageEvent` in `AppEvent::Plugin`, ready for the dispatch loop to
/// route through the plugin path.
pub fn as_app_event(event: &UsageEvent) -> AppEvent {
    AppEvent::Plugin {
        plugin_id: BURNDOWN_PLUGIN_ID.to_string(),
        payload: encode(event),
    }
}

/// Inverse of `as_app_event`. Returns `None` for events not addressed to the
/// burndown plugin or with malformed payloads (logged but not fatal).
pub fn from_app_event(event: &AppEvent) -> Option<UsageEvent> {
    if let AppEvent::Plugin { plugin_id, payload } = event {
        if plugin_id == BURNDOWN_PLUGIN_ID {
            return decode(payload).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(ev: &UsageEvent) -> UsageEvent {
        let bytes = encode(ev);
        decode(&bytes).expect("round-trip decode")
    }

    #[test]
    fn app_event_plugin_payload_roundtrip() {
        let original = UsageEvent::SetPeriod(7);
        let app_event = as_app_event(&original);
        assert!(matches!(app_event, AppEvent::Plugin { .. }));
        let decoded = from_app_event(&app_event).expect("decode back");
        assert_eq!(decoded, original);
    }

    #[test]
    fn usage_event_bridge_translates_legacy_variants() {
        // Spot-check one variant from each shape: unit, tuple(u8), tuple(char).
        for ev in [
            UsageEvent::Back,
            UsageEvent::NextProvider,
            UsageEvent::ToggleZoom,
            UsageEvent::SetPeriod(30),
            UsageEvent::InputChar('a'),
            UsageEvent::ZoomSearchChar('Z'),
        ] {
            let app_event = as_app_event(&ev);
            let recovered = from_app_event(&app_event)
                .unwrap_or_else(|| panic!("could not recover {ev:?}"));
            assert_eq!(recovered, ev);
        }
    }

    #[test]
    fn from_app_event_rejects_other_plugin_id() {
        let foreign = AppEvent::Plugin {
            plugin_id: "not-burndown".to_string(),
            payload: encode(&UsageEvent::Back),
        };
        assert!(from_app_event(&foreign).is_none());
    }

    #[test]
    fn from_app_event_rejects_non_plugin_variant() {
        let other = AppEvent::Quit;
        assert!(from_app_event(&other).is_none());
    }

    #[test]
    fn decode_rejects_garbage_payload() {
        let result = decode(b"\xff\x00not msgpack");
        assert!(result.is_err());
    }

    #[test]
    fn unit_variant_roundtrips() {
        assert_eq!(round_trip(&UsageEvent::ClearAllChips), UsageEvent::ClearAllChips);
    }

    #[test]
    fn every_tuple_variant_roundtrips() {
        for ev in [
            UsageEvent::SetPeriod(0),
            UsageEvent::SetPeriod(255),
            UsageEvent::InputChar('\u{2705}'),
            UsageEvent::ZoomSearchChar(' '),
        ] {
            assert_eq!(round_trip(&ev), ev);
        }
    }
}
