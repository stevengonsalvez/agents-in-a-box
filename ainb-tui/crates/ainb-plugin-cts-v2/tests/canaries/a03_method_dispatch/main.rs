use ainb_plugin_sdk::{Cell, Coord, HostClient, Plugin, RenderParams, Result, Server, WireBuffer};
use async_trait::async_trait;

struct A03;

#[async_trait]
impl Plugin for A03 {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-a03\"\nversion = \"0.0.1\"\nabi_version = 2\n"
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("M"));
        Ok(b)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A03).run_stdio().await.ok();
}
