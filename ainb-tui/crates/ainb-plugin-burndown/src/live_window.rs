//! Plugin-local mirror of ainb-core::models::live_window.
//!
//! The 3-tier cascade (statusline cache + local 5h aggregation) lives
//! on the host; the plugin receives a pre-computed `LiveWindow` value
//! through the plugin context (or via a host-fn call) and renders it.
//! Phase 3 ships the type + an empty default — wiring the cascade
//! across the WASM boundary is a follow-up host-fn so we don't expand
//! the plugin's capability surface.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    Tier1Cache,
    Tier2Local,
    #[default]
    None,
}

#[derive(Debug, Clone, Default)]
pub struct LiveWindow {
    pub five_hour_pct: Option<u8>,
    pub seven_day_pct: Option<u8>,
    pub today_cost_usd: Option<f64>,
    pub resets_in: Option<Duration>,
    pub context_pct: Option<u8>,
    pub model: Option<String>,
    pub source: Source,
}

impl LiveWindow {
    pub fn empty() -> Self {
        Self {
            source: Source::None,
            ..Default::default()
        }
    }
}

/// Phase 3 stub: returns an empty live window (Source::None). The host
/// will hand a populated `LiveWindow` to the plugin via PluginEvent in
/// a follow-up wiring commit; for now the UI renders the "wire the
/// statusline" CTA fallback path.
pub fn current() -> LiveWindow {
    LiveWindow::empty()
}
