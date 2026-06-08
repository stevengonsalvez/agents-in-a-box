//! CTS canary for the `plugin/handle_mouse` forwarding axis.
//!
//! Exercises the full host→plugin mouse path: the host-side axis test calls
//! `RuntimeHandle::send_mouse(..)`, which the runtime delivers over the
//! priority mouse channel as a `plugin/handle_mouse` notification. This canary
//! records the last mouse event it received in handler order and exposes it via
//! `cli_dispatch`:
//!
//! - `last` — echo the most recent mouse event as `<kind>:<col>,<row>:<mods>`
//!   (e.g. `down_left:7,2:0`), or `none` if no mouse event has arrived yet.
//! - `count` — number of mouse events received so far.
//!
//! Proves the wire shape round-trips and that coordinates arrive in the
//! plugin's own viewport space exactly as the host forwarded them.

use std::sync::Arc;

use ainb_plugin_sdk::{
    Cell, CliOutput, Coord, HandleMouseParams, HostClient, MouseButton, MouseKind, Plugin,
    RenderParams, Result, Server, WireBuffer,
};
use async_trait::async_trait;
use tokio::sync::Mutex;

#[derive(Clone)]
struct LastMouse {
    kind: String,
    col: u16,
    row: u16,
    mods: u8,
}

struct MouseForward {
    last: Arc<Mutex<Option<LastMouse>>>,
    count: Arc<Mutex<usize>>,
}

fn kind_label(kind: &MouseKind) -> String {
    fn btn(b: &MouseButton) -> &'static str {
        match b {
            MouseButton::Left => "left",
            MouseButton::Right => "right",
            MouseButton::Middle => "middle",
        }
    }
    match kind {
        MouseKind::Down { button } => format!("down_{}", btn(button)),
        MouseKind::Up { button } => format!("up_{}", btn(button)),
        MouseKind::Drag { button } => format!("drag_{}", btn(button)),
        MouseKind::Moved => "moved".into(),
        MouseKind::ScrollUp => "scroll_up".into(),
        MouseKind::ScrollDown => "scroll_down".into(),
        MouseKind::ScrollLeft => "scroll_left".into(),
        MouseKind::ScrollRight => "scroll_right".into(),
    }
}

#[async_trait]
impl Plugin for MouseForward {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-mouse-forward\"\nversion = \"0.0.1\"\nabi_version = 2\n[provides]\ncli_namespaces = [\"mouse\"]\n"
    }

    async fn handle_mouse(&mut self, _host: &HostClient, p: HandleMouseParams) -> Result<()> {
        *self.last.lock().await = Some(LastMouse {
            kind: kind_label(&p.mouse.kind),
            col: p.mouse.col,
            row: p.mouse.row,
            mods: p.mouse.mods,
        });
        *self.count.lock().await += 1;
        Ok(())
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("X"));
        Ok(b)
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        _namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        match argv.first().map(String::as_str) {
            Some("last") => {
                let out = match &*self.last.lock().await {
                    Some(m) => format!("{}:{},{}:{}\n", m.kind, m.col, m.row, m.mods),
                    None => "none\n".into(),
                };
                Ok(CliOutput::ok(out))
            }
            Some("count") => {
                let n = *self.count.lock().await;
                Ok(CliOutput::ok(format!("{n}\n")))
            }
            _ => Ok(CliOutput::ok(b"ok".to_vec())),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(MouseForward {
        last: Arc::new(Mutex::new(None)),
        count: Arc::new(Mutex::new(0)),
    })
    .run_stdio()
    .await
    .ok();
}
