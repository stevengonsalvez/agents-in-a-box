use ainb_plugin_sdk::{
    HostClient, Plugin, Result, Server,
    RenderParams, WireBuffer, Cell, Coord,
};
use async_trait::async_trait;

struct A01;

#[async_trait]
impl Plugin for A01 {
    fn manifest(&self) -> &'static str {
        include_str!("manifest.toml")
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("A"));
        Ok(b)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A01).run_stdio().await.ok();
}
