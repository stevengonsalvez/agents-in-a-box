use ainb_plugin_sdk::{
    HostClient, Plugin, Result, Server,
    RenderParams, WireBuffer, Cell, Coord,
};
use async_trait::async_trait;

struct A13;

#[async_trait]
impl Plugin for A13 {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-a13\"\nversion = \"0.0.1\"\nabi_version = 2\n"
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("Q"));
        Ok(b)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A13).run_stdio().await.ok();
}
