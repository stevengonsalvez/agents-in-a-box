use ainb_plugin_sdk::{
    HostClient, Plugin, Result, Server,
    RenderParams, WireBuffer, Cell, Coord,
};
use async_trait::async_trait;

struct A05;

#[async_trait]
impl Plugin for A05 {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-a05\"\nversion = \"0.0.1\"\nabi_version = 2\n"
    }

    async fn render(&mut self, _host: &HostClient, p: RenderParams) -> Result<WireBuffer> {
        let w = p.viewport.width;
        let h = p.viewport.height;
        let mut b = WireBuffer::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let ch = (b'A' + ((x + y) % 26) as u8) as char;
                b.push(Coord::new(x, y), Cell::new(ch.to_string()));
            }
        }
        Ok(b)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A05).run_stdio().await.ok();
}
