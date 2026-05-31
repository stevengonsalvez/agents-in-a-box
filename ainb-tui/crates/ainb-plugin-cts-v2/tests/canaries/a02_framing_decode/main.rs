use ainb_plugin_sdk::{Cell, Coord, HostClient, Plugin, RenderParams, Result, Server, WireBuffer};
use async_trait::async_trait;

struct A02;

#[async_trait]
impl Plugin for A02 {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-a02\"\nversion = \"0.0.1\"\nabi_version = 2\n"
    }

    async fn render(&mut self, _host: &HostClient, p: RenderParams) -> Result<WireBuffer> {
        let w = p.viewport.width;
        let h = p.viewport.height;
        let mut b = WireBuffer::new(w, h);
        for y in 0..h {
            for x in 0..w {
                b.push(Coord::new(x, y), Cell::new(format!("{}", (x + y) % 10)));
            }
        }
        Ok(b)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A02).run_stdio().await.ok();
}
