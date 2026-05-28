//! [`WitrPlugin`] — the SDK `Plugin` trait impl that the binary serves
//! over stdio.
//!
//! cfx.1 ships the scaffolding only: the plugin echoes its manifest on
//! `plugin/init` and renders a viewport-shaped empty `WireBuffer` on
//! `plugin/render`. Subsequent beads layer in the detect / exec /
//! parse / render / CLI / event-bus surfaces.

use async_trait::async_trait;

use ainb_plugin_sdk::{
    HandleEventParams, HandleKeyParams, HostClient, Plugin, RenderParams, Result, WireBuffer,
};

/// The canonical manifest bytes. Compiled into the binary so the SDK's
/// `plugin/init` handler can echo `name`/`version` back to the host
/// without a runtime file read.
const MANIFEST_TOML: &str = include_str!("../manifest.toml");

/// Plugin state. cfx.1 carries no state — later beads will add a
/// detect cache (witr binary path + version) and an LRU snapshot cache.
#[derive(Debug, Default)]
pub struct WitrPlugin;

#[async_trait]
impl Plugin for WitrPlugin {
    fn manifest(&self) -> &'static str {
        MANIFEST_TOML
    }

    async fn render(&mut self, _host: &HostClient, params: RenderParams) -> Result<WireBuffer> {
        Ok(WireBuffer::new(
            params.viewport.width,
            params.viewport.height,
        ))
    }

    async fn handle_event(&mut self, _host: &HostClient, _params: HandleEventParams) -> Result<()> {
        Ok(())
    }

    async fn handle_key(&mut self, _host: &HostClient, _params: HandleKeyParams) -> Result<()> {
        Ok(())
    }
}
