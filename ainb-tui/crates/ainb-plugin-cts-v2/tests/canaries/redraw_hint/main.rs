//! CTS canary for the `RenderResult.redraw` self-animation hint axis.
//!
//! Reports `wants_redraw() == true` for its opening renders, then `false`.
//! `wants_redraw` is queried *after* `render` bumps the counter, so with
//! `FRAMES = 3` renders #0 and #1 request another frame and render #2
//! settles. The host-side axis asserts the runtime re-marks the plugin's
//! render-dirty flag while `redraw` is set (so the render loop would keep
//! calling `plugin/render` without input) and stops once it clears.
//!
//! Uses an `AtomicUsize` render counter — `wants_redraw` is a sync `&self`
//! method and can't await a `tokio::Mutex`, so the count must be lock-free.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ainb_plugin_sdk::{Cell, Coord, HostClient, Plugin, RenderParams, Result, Server, WireBuffer};
use async_trait::async_trait;

/// Renders #0..FRAMES-1 request another redraw (count is read post-increment).
const FRAMES: usize = 3;

struct RedrawHint {
    renders: Arc<AtomicUsize>,
}

#[async_trait]
impl Plugin for RedrawHint {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-redraw-hint\"\nversion = \"0.0.1\"\nabi_version = 2\n"
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        self.renders.fetch_add(1, Ordering::SeqCst);
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("R"));
        Ok(b)
    }

    fn wants_redraw(&self) -> bool {
        // Animate for the first FRAMES renders, then settle.
        self.renders.load(Ordering::SeqCst) < FRAMES
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(RedrawHint {
        renders: Arc::new(AtomicUsize::new(0)),
    })
    .run_stdio()
    .await
    .ok();
}
