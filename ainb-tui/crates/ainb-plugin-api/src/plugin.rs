//! Plugin trait — the SDK-side ergonomic API.
//!
//! Plugin authors implement this trait. A future `#[ainb::plugin]` macro will
//! generate the `_init` / `_handle_event` / `_render` / `_shutdown` C ABI
//! exports that the host calls. For Phase 1 the macro is not yet shipped, so
//! the trait stands alone as a documentation/SDK target.

use crate::events::PluginEvent;
use crate::manifest::Manifest;
use crate::render::{Rect, RenderTarget, WireBuffer};

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("init failed: {0}")]
    Init(String),
    #[error("event handler failed: {0}")]
    HandleEvent(String),
    #[error("render failed: {0}")]
    Render(String),
    #[error("decode error: {0}")]
    Decode(String),
}

pub type PluginResult<T> = Result<T, PluginError>;

/// Per-call context the host hands the plugin. Phase 1 is intentionally bare;
/// later phases will hang convenience methods (logging, data dir paths,
/// session lookups) off this struct so plugin authors don't touch raw
/// pointer/length pairs.
#[derive(Debug, Default)]
pub struct PluginContext {
    /// Wallclock time in milliseconds at the start of the call (host-provided).
    pub now_ms: u64,
    /// The plugin's stable id (`plugin.name` from the manifest).
    pub plugin_id: String,
}

/// What every plugin implements.
pub trait Plugin: Send {
    fn manifest(&self) -> &Manifest;

    fn init(&mut self, ctx: &mut PluginContext) -> PluginResult<()>;

    fn handle_event(
        &mut self,
        ctx: &mut PluginContext,
        event: PluginEvent,
    ) -> PluginResult<()>;

    /// Render to a [`WireBuffer`] which the host then converts to a ratatui
    /// Buffer and paints. The returned buffer's `width`/`height` should match
    /// `area`'s; the host clips otherwise.
    fn render(
        &mut self,
        ctx: &mut PluginContext,
        target: RenderTarget,
        area: Rect,
    ) -> PluginResult<WireBuffer>;

    fn shutdown(&mut self, _ctx: &mut PluginContext) {}
}
