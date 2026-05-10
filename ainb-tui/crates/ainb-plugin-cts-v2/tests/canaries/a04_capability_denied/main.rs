use ainb_plugin_sdk::{
    CliOutput, HostClient, Plugin, Result, SdkError, Server,
    RenderParams, RpcError, WireBuffer, Cell, Coord,
};
use async_trait::async_trait;

struct A04;

#[async_trait]
impl Plugin for A04 {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-a04\"\nversion = \"0.0.1\"\nabi_version = 2\n"
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("C"));
        Ok(b)
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        _namespace: &str,
        _argv: &[String],
    ) -> Result<CliOutput> {
        Err(SdkError::Rpc(Box::new(RpcError::capability_denied("fs.read"))))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A04).run_stdio().await.ok();
}
