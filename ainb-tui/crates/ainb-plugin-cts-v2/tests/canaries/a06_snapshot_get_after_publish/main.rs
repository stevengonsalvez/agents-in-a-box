use ainb_plugin_sdk::{
    Cell, Coord, HostClient, InitContext, Plugin, RenderParams, Result, Server, WireBuffer,
};
use async_trait::async_trait;

struct A06;

#[async_trait]
impl Plugin for A06 {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-a06\"\nversion = \"0.0.1\"\nabi_version = 2\n[provides]\nsnapshots = [\"cts.a06\"]\n"
    }

    async fn on_init(&mut self, host: &HostClient, _ctx: InitContext<'_>) -> Result<()> {
        host.snapshot_publish("cts.a06", bytes::Bytes::from_static(b"a06-payload"))
            .await
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("S"));
        Ok(b)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A06).run_stdio().await.ok();
}
