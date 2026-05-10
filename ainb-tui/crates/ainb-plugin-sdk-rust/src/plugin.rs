//! The [`Plugin`] trait every ainb plugin implements.
//!
//! Concrete plugins are author-defined types stored as the
//! [`Server`](crate::server::Server)'s state. Each handler receives a
//! `&mut self` (the server serialises access via a [`tokio::sync::Mutex`])
//! and a borrowed [`HostClient`] for reverse-direction calls (snapshot
//! publish, action invoke, log) that the plugin wants to issue mid-handler.
//!
//! The `Send + 'static` bound is required so the runtime can park the
//! plugin on a tokio task. `#[async_trait]` makes the futures `Send` by
//! default.

use async_trait::async_trait;

use crate::{HostClient, Result};
use ainb_plugin_protocol::{
    params::{HandleEventParams, RenderParams},
    wire_buffer::WireBuffer,
};

/// Captured stdout / stderr / exit code from a CLI dispatch.
///
/// Mirrors the wire shape in
/// [`ainb_plugin_protocol::params::CliDispatchResult`] but lets handlers
/// stay primitive — the SDK adapts to bytes when serialising.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOutput {
    /// Stdout bytes captured by the plugin (UTF-8 by convention).
    pub stdout: Vec<u8>,
    /// Stderr bytes captured by the plugin.
    pub stderr: Vec<u8>,
    /// Process-style exit code. `0` = success.
    pub exit_code: i32,
}

impl CliOutput {
    /// Convenience: an exit-0 success with `stdout` set and empty stderr.
    pub fn ok(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: Vec::new(),
            exit_code: 0,
        }
    }

    /// Convenience: an exit-1 failure with `stderr` set and empty stdout.
    pub fn err(stderr: impl Into<Vec<u8>>) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: stderr.into(),
            exit_code: 1,
        }
    }
}

/// Trait implemented by every ainb plugin.
///
/// All handler methods take `&mut self` — the [`Server`](crate::Server)
/// guarantees serialised access via an internal [`tokio::sync::Mutex`].
/// `host` is the same [`HostClient`] for the lifetime of the plugin —
/// passing it explicitly avoids the `Option<HostClient>`-stash trap.
///
/// Default implementations for `handle_event`, `cli_dispatch`, `on_init`,
/// and `on_shutdown` cover plugins that don't need them.
#[async_trait]
pub trait Plugin: Send + 'static {
    /// Returns the plugin's manifest TOML as a static string. The
    /// canonical implementation is `include_str!("../manifest.toml")`.
    /// The Server uses this on
    /// [`PLUGIN_INIT`](ainb_plugin_protocol::methods::PLUGIN_INIT) to
    /// echo `name`/`version` back to the host so spawn vs manifest can
    /// be cross-checked.
    fn manifest(&self) -> &'static str;

    /// Render the plugin's UI for the requested viewport.
    ///
    /// `params.generation` is plugin-defined — the host treats it as
    /// opaque. Plugins are free to use it as a redraw token.
    async fn render(&mut self, host: &HostClient, params: RenderParams) -> Result<WireBuffer>;

    /// Receive a snapshot/event push from the host.
    ///
    /// Notification — the response is not waited on. Errors are logged
    /// by the runtime but do not surface to the host that produced the
    /// event.
    async fn handle_event(&mut self, host: &HostClient, params: HandleEventParams) -> Result<()> {
        let _ = (host, params);
        Ok(())
    }

    /// Dispatch a CLI subcommand the plugin advertised in its manifest's
    /// `[provides] cli_namespaces` list. Default implementation returns
    /// exit-2 (`unknown command`).
    async fn cli_dispatch(
        &mut self,
        host: &HostClient,
        namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        let _ = (host, argv);
        Ok(CliOutput {
            stdout: Vec::new(),
            stderr: format!("plugin does not handle namespace `{namespace}`\n").into_bytes(),
            exit_code: 2,
        })
    }

    /// Called once after
    /// [`PLUGIN_INIT`](ainb_plugin_protocol::methods::PLUGIN_INIT).
    ///
    /// Default is a no-op. Plugins are free to override for one-shot
    /// setup (open caches, subscribe to topics via `host`, ...). The
    /// granted-capabilities list is supplied so the plugin can refuse
    /// to start if a required capability was withheld — return
    /// [`crate::SdkError::plugin`] in that case.
    async fn on_init(&mut self, host: &HostClient, granted_capabilities: &[String]) -> Result<()> {
        let _ = (host, granted_capabilities);
        Ok(())
    }

    /// Called immediately before the server returns from
    /// [`PLUGIN_SHUTDOWN`](ainb_plugin_protocol::methods::PLUGIN_SHUTDOWN).
    /// Default no-op.
    async fn on_shutdown(&mut self, host: &HostClient) -> Result<()> {
        let _ = host;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_output_helpers() {
        let ok = CliOutput::ok(b"hello".to_vec());
        assert_eq!(ok.exit_code, 0);
        assert_eq!(ok.stdout, b"hello");
        assert!(ok.stderr.is_empty());

        let err = CliOutput::err(b"boom".to_vec());
        assert_eq!(err.exit_code, 1);
        assert!(err.stdout.is_empty());
        assert_eq!(err.stderr, b"boom");
    }

    #[allow(dead_code)]
    fn requires_send_static<T: Send + 'static>() {}

    #[test]
    fn plugin_is_send_static() {
        // Compile-time check: a boxed Plugin satisfies the bounds.
        requires_send_static::<Box<dyn Plugin>>();
    }
}
