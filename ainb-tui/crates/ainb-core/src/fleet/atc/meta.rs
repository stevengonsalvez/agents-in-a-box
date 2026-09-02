// ABOUTME: `AtcMeta` — the persisted configuration record for one ATC instance.
//
// Written to `~/.agents-in-a-box/atc/<name>/meta.json` on setup; read back by
// `status` / `list` / `heartbeat`. Defaults match the goal: 15-minute heartbeat,
// 60-minute idle-pause.

use serde::{Deserialize, Serialize};

use crate::fleet::atc::supervisor::{DEFAULT_PROVIDER, SupervisorMode};

/// Default heartbeat cadence in minutes.
pub const DEFAULT_HEARTBEAT_INTERVAL_MIN: u32 = 15;

/// Default idle-pause threshold in minutes. After this long with the fleet quiet
/// (nothing needing attention) the heartbeat body becomes a cheap idle ping so
/// ATC spends no tokens deciding.
pub const DEFAULT_IDLE_PAUSE_MIN: u32 = 60;

/// Persisted configuration for a single ATC instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtcMeta {
    /// Instance name. Also the suffix of the spawned tmux session (`tmux_<name>`).
    pub name: String,

    /// Whether the OS-timer heartbeat is installed/active.
    pub heartbeat_enabled: bool,

    /// Heartbeat cadence in minutes.
    pub heartbeat_interval_min: u32,

    /// Minutes of fleet quiet before the heartbeat downgrades to a cheap idle ping.
    pub idle_pause_min: u32,

    /// Which supervisor owns this fleet. Exactly one is active at a time; see
    /// [`crate::fleet::atc::supervisor`].
    ///
    /// `#[serde(default)]` is load-bearing for compatibility: an instance
    /// provisioned before modes existed has no such key, and it was a full LLM
    /// ATC. [`SupervisorMode::default`] is `Full`, so reading an old meta.json
    /// keeps that instance behaving exactly as it did.
    #[serde(default)]
    pub mode: SupervisorMode,

    /// The provider driving the full-mode brain. Ignored in lite mode, which
    /// runs no brain — but retained across a switch so toggling back does not
    /// silently reset the operator's choice.
    ///
    /// Defaults to `claude` for the same compatibility reason as `mode`.
    #[serde(default = "default_provider")]
    pub provider: String,
}

/// Serde default for [`AtcMeta::provider`] — the pre-existing behaviour.
fn default_provider() -> String {
    DEFAULT_PROVIDER.to_string()
}

impl AtcMeta {
    /// Construct a meta with default heartbeat knobs.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            heartbeat_enabled: true,
            heartbeat_interval_min: DEFAULT_HEARTBEAT_INTERVAL_MIN,
            idle_pause_min: DEFAULT_IDLE_PAUSE_MIN,
            mode: SupervisorMode::default(),
            provider: default_provider(),
        }
    }

    /// The controller currently permitted to send actions for this instance.
    #[must_use]
    pub const fn owner(&self) -> crate::fleet::atc::supervisor::Controller {
        self.mode.owner()
    }

    /// The tmux session name ATC runs under (`ainb run --name <name>` →
    /// `tmux_<name>`).
    ///
    /// Uses the same sanitization the real spawner ([`crate::tmux::TmuxSession`])
    /// applies, so names containing a space, `.`, `/`, or `:` resolve to the
    /// identical session for status/teardown instead of targeting the wrong one.
    #[must_use]
    pub fn tmux_session(&self) -> String {
        crate::tmux::sanitize_session_name(&self.name)
    }

    /// Serialize to pretty JSON for `meta.json`.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Parse from `meta.json` contents.
    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }

    /// Heartbeat interval in whole seconds, clamped to a sane floor so a
    /// misconfigured `0` cannot spin the timer.
    #[must_use]
    pub fn interval_secs(&self) -> u64 {
        let mins = self.heartbeat_interval_min.max(1);
        u64::from(mins) * 60
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_documented_defaults() {
        let m = AtcMeta::new("tower");
        assert_eq!(m.name, "tower");
        assert!(m.heartbeat_enabled);
        assert_eq!(m.heartbeat_interval_min, 15);
        assert_eq!(m.idle_pause_min, 60);
        // A fresh instance is a full LLM ATC on Claude — the pre-existing
        // behaviour, unchanged by the mode feature.
        assert_eq!(m.mode, SupervisorMode::Full);
        assert_eq!(m.provider, "claude");
    }

    #[test]
    fn a_meta_written_before_modes_existed_reads_back_as_full_claude() {
        // The compatibility contract. An on-disk meta.json from an older ainb
        // has neither key; deserializing it must NOT silently downgrade a
        // running LLM ATC to the no-LLM scanner.
        let legacy = r#"{
            "name": "tower",
            "heartbeat_enabled": true,
            "heartbeat_interval_min": 10,
            "idle_pause_min": 45
        }"#;
        let m = AtcMeta::from_json(legacy).expect("legacy meta must still parse");
        assert_eq!(m.mode, SupervisorMode::Full);
        assert_eq!(m.provider, "claude");
        assert_eq!(m.heartbeat_interval_min, 10, "existing knobs survive");
    }

    #[test]
    fn mode_and_provider_persist_across_a_write_read_cycle() {
        let mut m = AtcMeta::new("tower");
        m.mode = SupervisorMode::Lite;
        m.provider = "codex".into();
        let back = AtcMeta::from_json(&m.to_json().unwrap()).unwrap();
        assert_eq!(back.mode, SupervisorMode::Lite);
        assert_eq!(back.provider, "codex");
        assert_eq!(back.owner(), SupervisorMode::Lite.owner());
    }

    #[test]
    fn json_round_trips_exactly() {
        let m = AtcMeta {
            name: "alpha".into(),
            heartbeat_enabled: false,
            heartbeat_interval_min: 5,
            idle_pause_min: 30,
            mode: SupervisorMode::Lite,
            provider: "codex".into(),
        };
        let json = m.to_json().expect("serialize");
        let back = AtcMeta::from_json(&json).expect("deserialize");
        assert_eq!(m, back);
    }

    #[test]
    fn tmux_session_prefixes_name() {
        assert_eq!(AtcMeta::new("tower").tmux_session(), "tmux_tower");
    }

    #[test]
    fn tmux_session_sanitizes_unsafe_chars_like_the_spawner() {
        // A name with space/./ /: must resolve to the SAME session the real
        // spawner (TmuxSession::sanitize_name) produces, so status/teardown
        // never target the wrong session.
        let m = AtcMeta::new("test.name/with:chars");
        assert_eq!(m.tmux_session(), "tmux_test_name_with_chars");
        assert_eq!(
            m.tmux_session(),
            crate::tmux::session::TmuxSession::sanitize_name("test.name/with:chars")
        );
    }

    #[test]
    fn interval_secs_clamps_zero_to_one_minute() {
        let m = AtcMeta {
            name: "z".into(),
            heartbeat_enabled: true,
            heartbeat_interval_min: 0,
            idle_pause_min: 60,
            ..AtcMeta::new("z")
        };
        assert_eq!(m.interval_secs(), 60);
    }

    #[test]
    fn interval_secs_scales_minutes() {
        let m = AtcMeta::new("tower");
        assert_eq!(m.interval_secs(), 900);
    }
}
