use ainb_plugin_sdk::{
    Cell, Coord, HostClient, LogLevel, Plugin, RenderParams, Result, Server, WireBuffer,
};
use async_trait::async_trait;

struct A09;

#[async_trait]
impl Plugin for A09 {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-a09\"\nversion = \"0.0.1\"\nabi_version = 2\n"
    }

    async fn on_init(&mut self, host: &HostClient, _caps: &[String]) -> Result<()> {
        host.log(LogLevel::Trace, "trace-msg".to_string(), None).await?;
        host.log(LogLevel::Debug, "debug-msg".to_string(), None).await?;
        host.log(LogLevel::Info, "info-msg".to_string(), None).await?;
        host.log(LogLevel::Warn, "warn-msg".to_string(), None).await?;
        host.log(LogLevel::Error, "error-msg".to_string(), None).await?;
        Ok(())
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("L"));
        Ok(b)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A09).run_stdio().await.ok();
}
