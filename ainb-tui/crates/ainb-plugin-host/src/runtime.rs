//! Per-plugin wasmi state.
//!
//! Each loaded plugin owns its own [`wasmi::Store`] parameterised over
//! [`HostState`]. The Store keeps the plugin's linear memory, table, and
//! per-call mutable state (like the most recent error message captured by
//! `ainb_log`). The host fns in [`crate::host_fns`] read/write this state.

use ainb_plugin_api::CapabilitySet;

/// Mutable state the host carries alongside each plugin's wasmi `Store`.
///
/// `last_error` is set by host-fns that fail validation; tests assert on it.
#[derive(Debug, Default)]
pub struct HostState {
    pub plugin_id: String,
    pub capabilities: CapabilitySet,
    /// Captured `ainb_log` payloads; bounded to `LOG_RING_CAPACITY` entries to
    /// keep tests deterministic and avoid unbounded growth in long-running
    /// hosts.
    pub log_ring: Vec<LoggedLine>,
    /// Set by any host-fn that returned a negative status; cleared by the
    /// caller before each plugin call.
    pub last_error: Option<String>,
}

const LOG_RING_CAPACITY: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedLine {
    pub level: i32,
    pub msg: String,
}

impl HostState {
    #[must_use]
    pub fn new(plugin_id: impl Into<String>, caps: CapabilitySet) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            capabilities: caps,
            log_ring: Vec::new(),
            last_error: None,
        }
    }

    pub fn push_log(&mut self, level: i32, msg: String) {
        if self.log_ring.len() >= LOG_RING_CAPACITY {
            self.log_ring.remove(0);
        }
        self.log_ring.push(LoggedLine { level, msg });
    }
}
