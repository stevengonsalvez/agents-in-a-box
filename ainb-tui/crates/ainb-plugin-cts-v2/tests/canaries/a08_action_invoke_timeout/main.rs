use ainb_plugin_sdk::{
    Cell, CliOutput, Coord, HostClient, Plugin, RenderParams, Result, Server, WireBuffer,
};
use async_trait::async_trait;

struct A08;

#[async_trait]
impl Plugin for A08 {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-a08\"\nversion = \"0.0.1\"\nabi_version = 2\n"
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("T"));
        Ok(b)
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        _namespace: &str,
        _argv: &[String],
    ) -> Result<CliOutput> {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        Ok(CliOutput::ok(b"done".to_vec()))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A08).run_stdio().await.ok();
}
