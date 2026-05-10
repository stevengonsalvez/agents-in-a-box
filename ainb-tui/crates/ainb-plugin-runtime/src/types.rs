//! Common runtime types: ids, outcomes, lifecycle states.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use ainb_plugin_protocol::params::CliDispatchResult;
use ainb_plugin_protocol::wire_buffer::WireBuffer;
use bytes::Bytes;

/// Newtype around the manifest `[plugin].name`. Cheap to clone.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginId(Arc<str>);

impl PluginId {
    /// Construct a [`PluginId`] from any string.
    #[must_use]
    pub fn new(s: impl Into<Arc<str>>) -> Self {
        Self(s.into())
    }

    /// Underlying name as a `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for PluginId {
    fn from(s: &str) -> Self {
        Self::new(s.to_owned())
    }
}

impl From<String> for PluginId {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Snapshot/event topic. Same shape as [`PluginId`] for now; kept
/// distinct so accidental cross-use is a compile error.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Topic(Arc<str>);

impl Topic {
    /// Construct a [`Topic`].
    #[must_use]
    pub fn new(s: impl Into<Arc<str>>) -> Self {
        Self(s.into())
    }

    /// Underlying topic name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Topic {
    fn from(s: &str) -> Self {
        Self::new(s.to_owned())
    }
}

impl From<String> for Topic {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Outcome of a `plugin/render` request, surfaced to the TUI thread
/// via the oneshot returned by [`crate::RuntimeHandle::render`].
#[derive(Debug)]
pub enum RenderOutcome {
    /// Render succeeded; cells live in the buffer.
    Ok(WireBuffer),
    /// Plugin rejected the request (decoded peer error).
    PluginError {
        /// JSON-RPC error code.
        code: i32,
        /// Human-readable message.
        message: String,
    },
    /// Local-side failure (process crashed, timeout, etc.).
    RuntimeError(String),
}

/// Outcome of a `plugin/cli_dispatch` request.
#[derive(Debug)]
pub enum CliOutcome {
    /// Plugin executed; payload is the captured `stdout`/`stderr`/`exit_code`.
    Ok(CliDispatchResult),
    /// Plugin returned an error envelope.
    PluginError {
        /// JSON-RPC error code.
        code: i32,
        /// Human-readable message.
        message: String,
    },
    /// Local-side failure.
    RuntimeError(String),
}

/// FSM state of a single plugin's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    /// Discovered but no process; waiting for first use.
    Idle,
    /// `Command::spawn` issued; awaiting `plugin/init` reply.
    Spawning,
    /// Init succeeded; serving requests.
    Running,
    /// Process died; backoff timer counting down before respawn.
    Backoff,
    /// Three failures inside the failure window — won't respawn until
    /// caller reloads explicitly.
    Quarantined,
    /// Idle reap: shutdown notification sent; awaiting graceful exit.
    ShuttingDown,
}

/// Action / response payload pair returned by `host/action/invoke`.
#[derive(Debug)]
pub enum ActionOutcome {
    /// Plugin returned a payload.
    Ok(Bytes),
    /// Plugin returned an error envelope.
    PluginError {
        /// JSON-RPC error code.
        code: i32,
        /// Human-readable message.
        message: String,
    },
    /// Timeout, no plugin registered for the action, etc.
    RuntimeError(String),
}

/// Tunables a host can pass into [`crate::Runtime::with_config`].
///
/// Values match the dispatch spec: 10-min idle reap, 1/4/16s exp
/// backoff, quarantine after 3 fails inside 60s.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeConfig {
    /// How long a plugin can sit unused before idle reap kicks in.
    pub idle_reap: Duration,
    /// Backoff schedule for the 1st, 2nd, 3rd respawn attempts.
    pub respawn_backoff: [Duration; 3],
    /// Window inside which `quarantine_failure_threshold` failures
    /// trip quarantine.
    pub failure_window: Duration,
    /// How many failures inside the window before quarantine.
    pub quarantine_failure_threshold: usize,
    /// Default render request timeout when caller doesn't override.
    pub default_render_timeout: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            idle_reap: Duration::from_secs(10 * 60),
            respawn_backoff: [
                Duration::from_secs(1),
                Duration::from_secs(4),
                Duration::from_secs(16),
            ],
            failure_window: Duration::from_secs(60),
            quarantine_failure_threshold: 3,
            default_render_timeout: Duration::from_secs(2),
        }
    }
}
