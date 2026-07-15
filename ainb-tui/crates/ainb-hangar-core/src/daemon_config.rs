//! The single source of truth for the daemon's user-configurable knobs.
//!
//! Every genuine USER config value the daemon reads from the `daemon_config`
//! key/value table is described exactly once here as a [`ConfigDescriptor`]
//! (key, label, [`ConfigKind`], default, help). Both surfaces that expose the
//! set — the TUI Settings → Daemon section and the `ainb hangar daemon config`
//! CLI — iterate [`DAEMON_CONFIG_REGISTRY`] rather than hand-listing the fields,
//! so adding a knob here surfaces it in both places at once (and the parity
//! tests in each surface fail loudly if a surface ever falls behind the
//! registry).
//!
//! # What belongs here
//!
//! Only USER config — a knob a human deliberately sets. Internal STATE the
//! daemon writes for its own bookkeeping (e.g. `card_agent.last_used`, the
//! most-recently-picked agent) is **not** config and is deliberately excluded:
//! surfacing it as an editable row would let a human "set" a value the daemon
//! overwrites on the next dispatch, which reads as a broken control.
//!
//! # Validation
//!
//! [`ConfigDescriptor::validate`] is the one gate every write passes — the RPC
//! `hangar/daemon_config_set` handler, the CLI `set` verb, and the TUI editor
//! all call it. It parses + range/membership-checks the raw input and returns
//! the **normalized** string actually persisted (`true`/`false` for bools, a
//! trimmed integer for ints, the canonical lower-case token for enums), so the
//! stored value can never disagree with what the daemon's tolerant typed
//! accessors decode.

/// `daemon_config` key: the global auto-standup toggle (default OFF — opt-in).
pub const KEY_AUTOSTANDUP_ENABLED: &str = "autostandup.enabled";
/// `daemon_config` key: minutes a session must be stagnant before firing.
pub const KEY_AUTOSTANDUP_STAGNANT_MIN: &str = "autostandup.stagnant_min";
/// `daemon_config` key: per-session cooldown window in minutes.
pub const KEY_AUTOSTANDUP_COOLDOWN_MIN: &str = "autostandup.cooldown_min";
/// `daemon_config` key: max simultaneous in-flight standups.
pub const KEY_AUTOSTANDUP_MAX_CONCURRENT: &str = "autostandup.max_concurrent";
/// `daemon_config` key: the host-wide default card agent (F4 global tier).
pub const KEY_CARD_AGENT_DEFAULT: &str = "card_agent.default";

/// Declare a coded default ONCE, emitting both forms it has to exist in: the
/// typed const the daemon reads, and the canonical stored string the registry
/// descriptor displays.
///
/// The two forms previously duplicated the literal (`default: "false"` next to
/// `DEFAULT_AUTOSTANDUP_ENABLED = false`), which is a live drift vector: nothing
/// tied them together, so the registry could advertise a default the daemon does
/// not run. Deriving the string via `stringify!` makes that unrepresentable —
/// there is one literal, in one place.
macro_rules! coded_default {
    ($(#[$meta:meta])* $konst:ident: $ty:ty = $value:literal => $string:ident) => {
        $(#[$meta])*
        pub const $konst: $ty = $value;
        #[doc = concat!("[`", stringify!($konst), "`] as the canonical stored string,")]
        #[doc = "derived from the same literal so the two can never disagree."]
        const $string: &str = stringify!($value);
    };
}

coded_default! {
    /// Coded default: auto-standup is OFF (opt-in).
    DEFAULT_AUTOSTANDUP_ENABLED: bool = false => DEFAULT_AUTOSTANDUP_ENABLED_STR
}
coded_default! {
    /// Coded default: 15 minutes stagnant before a standup fires.
    DEFAULT_AUTOSTANDUP_STAGNANT_MIN: i64 = 15 => DEFAULT_AUTOSTANDUP_STAGNANT_MIN_STR
}
coded_default! {
    /// Coded default: 60-minute per-session cooldown.
    DEFAULT_AUTOSTANDUP_COOLDOWN_MIN: i64 = 60 => DEFAULT_AUTOSTANDUP_COOLDOWN_MIN_STR
}
coded_default! {
    /// Coded default: at most one concurrent standup.
    DEFAULT_AUTOSTANDUP_MAX_CONCURRENT: i64 = 1 => DEFAULT_AUTOSTANDUP_MAX_CONCURRENT_STR
}
/// Coded default: the host-wide default card agent is `claude`. Already a string,
/// so the registry wires this const through directly.
pub const DEFAULT_CARD_AGENT_DEFAULT: &str = "claude";

/// The minimum legal per-session cooldown, in minutes.
///
/// A cooldown of `0` is NOT merely "no cooldown" — it disarms auto-standup's two
/// safety gates at once. The cooldown check (`now - last < 0`) can never trip, and
/// the daemon derives its stale-in-flight window from the same value
/// (`cooldown * 60_000 * multiple`), so `0` makes every in-flight slot instantly
/// reclaimable and the concurrency cap meaningless. The watcher then writes
/// `/standup` into the session on EVERY reconcile tick, forever — exactly the
/// write-mode blast radius the D13 gates exist to bound. Floor it at 1.
///
/// The daemon clamps to this on load as well, so a row written before this bound
/// existed (or by hand) cannot resurrect the behaviour.
pub const MIN_AUTOSTANDUP_COOLDOWN_MIN: i64 = 1;

/// The shape of one configurable value — drives both the editor UI and the
/// validation gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    /// A boolean toggle. Accepts the tolerant token family
    /// (`1`/`true`/`yes`/`on` and their negatives), normalized to
    /// `true`/`false`.
    Bool,
    /// An integer bounded to `[min, max]`, editable in `step` increments (the
    /// step is the UI nudge size; any in-range integer is still accepted).
    Int {
        /// Inclusive lower bound.
        min: i64,
        /// Inclusive upper bound.
        max: i64,
        /// The increment a UI step applies.
        step: i64,
    },
    /// A closed set of string variants (case-insensitive on input, normalized
    /// to the listed spelling).
    Enum {
        /// The permitted variants, in cycle order.
        variants: &'static [&'static str],
    },
}

/// One user-configurable daemon knob: its key, human label, [`ConfigKind`],
/// default (as the canonical stored string), and one-line help.
#[derive(Debug, Clone, Copy)]
pub struct ConfigDescriptor {
    /// The `daemon_config` table key this describes.
    pub key: &'static str,
    /// A short human label for the row.
    pub label: &'static str,
    /// The value's shape (drives editing + validation).
    pub kind: ConfigKind,
    /// The default value, as the canonical stored string, applied when the key
    /// has no row (matches the daemon's coded default).
    pub default: &'static str,
    /// One-line help describing what the knob does.
    pub help: &'static str,
}

impl ConfigDescriptor {
    /// Validate + normalize `raw` for this descriptor, returning the exact
    /// string to persist, or a human-readable error explaining the rejection.
    ///
    /// This is the single validation gate: bools normalize to `true`/`false`,
    /// ints are range-checked and trimmed, enums are matched case-insensitively
    /// and normalized to their canonical spelling.
    ///
    /// # Errors
    ///
    /// Returns `Err` with a caller-facing message when `raw` is not a legal
    /// value for the descriptor's [`ConfigKind`] (bad bool token, out-of-range
    /// or non-integer int, or an enum value outside the variant set).
    pub fn validate(&self, raw: &str) -> Result<String, String> {
        let trimmed = raw.trim();
        match self.kind {
            ConfigKind::Bool => parse_bool_token(trimmed)
                .map(|b| if b { "true" } else { "false" }.to_string())
                .ok_or_else(|| {
                    format!(
                        "`{}` expects a boolean (true/false/1/0/yes/no/on/off), got {raw:?}",
                        self.key
                    )
                }),
            ConfigKind::Int { min, max, .. } => {
                let n: i64 = trimmed
                    .parse()
                    .map_err(|_| format!("`{}` expects an integer, got {raw:?}", self.key))?;
                if n < min || n > max {
                    return Err(format!(
                        "`{}` must be between {min} and {max}, got {n}",
                        self.key
                    ));
                }
                Ok(n.to_string())
            }
            ConfigKind::Enum { variants } => variants
                .iter()
                .find(|v| v.eq_ignore_ascii_case(trimmed))
                .map(|v| (*v).to_string())
                .ok_or_else(|| {
                    format!(
                        "`{}` must be one of [{}], got {raw:?}",
                        self.key,
                        variants.join(", ")
                    )
                }),
        }
    }

    /// A short type/range descriptor for the CLI `list` column (e.g. `bool`,
    /// `int 1..1440`, `enum claude|codex|copilot`).
    #[must_use]
    pub fn type_hint(&self) -> String {
        match self.kind {
            ConfigKind::Bool => "bool".to_string(),
            ConfigKind::Int { min, max, .. } => format!("int {min}..{max}"),
            ConfigKind::Enum { variants } => format!("enum {}", variants.join("|")),
        }
    }
}

/// Parse a boolean token, tolerating the common spellings (matches the store's
/// `daemon_config` decode so display + validation never disagree with the
/// daemon). `None` for an unrecognized token.
#[must_use]
pub fn parse_bool_token(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// The card-agent enum variants (`claude`/`codex`/`copilot`), matching
/// [`crate::agent_kind::AgentKind::all`]. Kept as a `const` so the registry
/// entry is `const`; a unit test asserts it stays in lock-step with `AgentKind`.
const CARD_AGENT_VARIANTS: &[&str] = &["claude", "codex", "copilot"];

/// The authoritative list of user-configurable daemon knobs. Both the TUI and
/// the CLI iterate this; adding an entry surfaces the knob in both.
pub const DAEMON_CONFIG_REGISTRY: &[ConfigDescriptor] = &[
    ConfigDescriptor {
        key: KEY_AUTOSTANDUP_ENABLED,
        label: "Auto-standup",
        kind: ConfigKind::Bool,
        default: DEFAULT_AUTOSTANDUP_ENABLED_STR,
        help: "Globally enable the auto-standup watcher (opt-in).",
    },
    ConfigDescriptor {
        key: KEY_AUTOSTANDUP_STAGNANT_MIN,
        label: "Stagnant minutes",
        kind: ConfigKind::Int {
            min: 1,
            max: 1440,
            step: 5,
        },
        default: DEFAULT_AUTOSTANDUP_STAGNANT_MIN_STR,
        help: "Minutes a session must be idle before a standup fires.",
    },
    ConfigDescriptor {
        key: KEY_AUTOSTANDUP_COOLDOWN_MIN,
        label: "Cooldown minutes",
        kind: ConfigKind::Int {
            min: MIN_AUTOSTANDUP_COOLDOWN_MIN,
            max: 1440,
            step: 5,
        },
        default: DEFAULT_AUTOSTANDUP_COOLDOWN_MIN_STR,
        help: "Per-session minutes between successive standups.",
    },
    ConfigDescriptor {
        key: KEY_AUTOSTANDUP_MAX_CONCURRENT,
        label: "Max concurrent",
        kind: ConfigKind::Int {
            min: 1,
            max: 64,
            step: 1,
        },
        default: DEFAULT_AUTOSTANDUP_MAX_CONCURRENT_STR,
        help: "Maximum simultaneous in-flight standups.",
    },
    ConfigDescriptor {
        key: KEY_CARD_AGENT_DEFAULT,
        label: "Default agent",
        kind: ConfigKind::Enum {
            variants: CARD_AGENT_VARIANTS,
        },
        default: DEFAULT_CARD_AGENT_DEFAULT,
        help: "Host-wide default provider backend for new cards.",
    },
];

/// Look up the descriptor for `key`, or `None` when the key is not a known
/// user-config knob (an internal-state key, or an unknown key).
#[must_use]
pub fn descriptor(key: &str) -> Option<&'static ConfigDescriptor> {
    DAEMON_CONFIG_REGISTRY.iter().find(|d| d.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_kind::AgentKind;

    #[test]
    fn every_default_passes_its_own_validation() {
        // A descriptor whose default fails its own gate would ship a knob that
        // can't round-trip — assert every default is a legal, normalized value.
        for d in DAEMON_CONFIG_REGISTRY {
            let normalized = d
                .validate(d.default)
                .unwrap_or_else(|e| panic!("default for `{}` rejected: {e}", d.key));
            assert_eq!(
                normalized, d.default,
                "default for `{}` not normalized",
                d.key
            );
        }
    }

    /// The loop the branch left open: `every_default_passes_its_own_validation`
    /// only checks a default against ITSELF, so it stays green even if the
    /// registry advertises `"false"` while the daemon runs `true`. Decode each
    /// descriptor's default string back to the TYPED const the daemon actually
    /// reads and assert they agree — that is the drift this closes.
    ///
    /// Auto-standup is the reason this matters: it writes `/standup` into live
    /// sessions, a recent change made it opt-in, and a stale registry string would
    /// silently tell users the opposite of what the daemon does.
    #[test]
    fn every_default_string_decodes_to_the_daemons_typed_const() {
        assert_eq!(
            parse_bool_token(descriptor(KEY_AUTOSTANDUP_ENABLED).unwrap().default),
            Some(DEFAULT_AUTOSTANDUP_ENABLED),
            "autostandup.enabled default must decode to the daemon's const"
        );
        assert!(
            !DEFAULT_AUTOSTANDUP_ENABLED,
            "auto-standup must stay OFF by default — it writes into live sessions"
        );
        assert_eq!(
            descriptor(KEY_AUTOSTANDUP_STAGNANT_MIN).unwrap().default.parse::<i64>(),
            Ok(DEFAULT_AUTOSTANDUP_STAGNANT_MIN)
        );
        assert_eq!(
            descriptor(KEY_AUTOSTANDUP_COOLDOWN_MIN).unwrap().default.parse::<i64>(),
            Ok(DEFAULT_AUTOSTANDUP_COOLDOWN_MIN)
        );
        assert_eq!(
            descriptor(KEY_AUTOSTANDUP_MAX_CONCURRENT).unwrap().default.parse::<i64>(),
            Ok(DEFAULT_AUTOSTANDUP_MAX_CONCURRENT)
        );
        assert_eq!(
            descriptor(KEY_CARD_AGENT_DEFAULT).unwrap().default,
            DEFAULT_CARD_AGENT_DEFAULT
        );
    }

    /// The cooldown floor is a SAFETY bound, not a UI nicety: `0` defeats both the
    /// cooldown gate and the stale-in-flight reclaim window the daemon derives
    /// from it, degenerating auto-standup into writing `/standup` into the session
    /// every tick. The registry must refuse to express it.
    #[test]
    fn cooldown_zero_is_rejected_by_the_registry_gate() {
        let d = descriptor(KEY_AUTOSTANDUP_COOLDOWN_MIN).unwrap();
        assert!(
            d.validate("0").is_err(),
            "cooldown 0 makes auto-standup fire every tick — it must not validate"
        );
        assert_eq!(
            d.validate("1").unwrap(),
            "1",
            "the floor itself is still settable"
        );
        assert!(matches!(
            d.kind,
            ConfigKind::Int {
                min: MIN_AUTOSTANDUP_COOLDOWN_MIN,
                ..
            }
        ));
    }

    #[test]
    fn keys_are_unique() {
        for (i, a) in DAEMON_CONFIG_REGISTRY.iter().enumerate() {
            for b in &DAEMON_CONFIG_REGISTRY[i + 1..] {
                assert_ne!(a.key, b.key, "duplicate registry key {}", a.key);
            }
        }
    }

    #[test]
    fn card_agent_variants_match_agent_kind() {
        let from_kind: Vec<&str> = AgentKind::all().iter().map(|k| k.as_str()).collect();
        assert_eq!(CARD_AGENT_VARIANTS, from_kind.as_slice());
    }

    #[test]
    fn bool_validation_is_tolerant_and_normalizes() {
        let d = descriptor(KEY_AUTOSTANDUP_ENABLED).unwrap();
        for on in ["1", "true", "YES", "On"] {
            assert_eq!(d.validate(on).unwrap(), "true");
        }
        for off in ["0", "false", "no", "OFF"] {
            assert_eq!(d.validate(off).unwrap(), "false");
        }
        assert!(d.validate("maybe").is_err());
    }

    #[test]
    fn int_validation_enforces_range() {
        let d = descriptor(KEY_AUTOSTANDUP_STAGNANT_MIN).unwrap();
        assert_eq!(d.validate(" 30 ").unwrap(), "30");
        assert!(d.validate("0").is_err(), "below min");
        assert!(d.validate("99999").is_err(), "above max");
        assert!(d.validate("abc").is_err(), "not an integer");
    }

    #[test]
    fn enum_validation_normalizes_case() {
        let d = descriptor(KEY_CARD_AGENT_DEFAULT).unwrap();
        assert_eq!(d.validate("CODEX").unwrap(), "codex");
        assert_eq!(d.validate("Claude").unwrap(), "claude");
        assert!(d.validate("gemini").is_err());
    }

    #[test]
    fn descriptor_lookup_excludes_internal_state() {
        // `card_agent.last_used` is internal state, never a registry knob.
        assert!(descriptor("card_agent.last_used").is_none());
        assert!(descriptor(KEY_CARD_AGENT_DEFAULT).is_some());
    }
}
