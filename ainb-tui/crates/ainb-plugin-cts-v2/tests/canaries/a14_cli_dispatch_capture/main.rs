use ainb_plugin_sdk::{
    Cell, CliOutput, Coord, HostClient, Plugin, RenderParams, Result, Server, WireBuffer,
};
use async_trait::async_trait;

struct A14;

#[async_trait]
impl Plugin for A14 {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-a14\"\nversion = \"0.0.1\"\nabi_version = 2\n[provides]\ncli_namespaces = [\"echo\"]\n"
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("E"));
        Ok(b)
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        _namespace: &str,
        _argv: &[String],
    ) -> Result<CliOutput> {
        Ok(CliOutput::ok("hello\n".to_string()))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A14).run_stdio().await.ok();
}
