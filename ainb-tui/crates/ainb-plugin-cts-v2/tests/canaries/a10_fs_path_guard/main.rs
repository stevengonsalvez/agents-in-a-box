use ainb_plugin_sdk::{
    Cell, CliOutput, Coord, HostClient, METHOD_NOT_FOUND, Plugin, RenderParams, Result, RpcError,
    SdkError, Server, WireBuffer,
};
use async_trait::async_trait;

struct A10;

#[async_trait]
impl Plugin for A10 {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-a10\"\nversion = \"0.0.1\"\nabi_version = 2\n"
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("F"));
        Ok(b)
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        _namespace: &str,
        _argv: &[String],
    ) -> Result<CliOutput> {
        Err(SdkError::Rpc(Box::new(RpcError::new(
            METHOD_NOT_FOUND,
            "host/fs/read_file not implemented",
        ))))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A10).run_stdio().await.ok();
}
