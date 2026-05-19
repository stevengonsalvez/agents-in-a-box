use ainb_plugin_sdk::{
    Cell, CliOutput, Coord, HandleEventParams, HostClient, Plugin, RenderParams, Result, Server,
    WireBuffer,
};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

type EventLog = Vec<(String, Vec<u8>)>;

struct A07 {
    received: Arc<Mutex<EventLog>>,
}

#[async_trait]
impl Plugin for A07 {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-a07\"\nversion = \"0.0.1\"\nabi_version = 2\n[subscribes]\nsnapshots = [\"cts.a07\"]\n"
    }

    async fn on_init(&mut self, host: &HostClient, _caps: &[String]) -> Result<()> {
        host.snapshot_subscribe("cts.a07").await?;
        Ok(())
    }

    async fn handle_event(&mut self, _host: &HostClient, p: HandleEventParams) -> Result<()> {
        self.received.lock().await.push((p.topic.clone(), p.payload.to_vec()));
        Ok(())
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let events = self.received.lock().await;
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new(format!("{}", events.len())));
        Ok(b)
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        _namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        if argv.first().map(String::as_str) == Some("count") {
            let n = self.received.lock().await.len();
            Ok(CliOutput::ok(format!("{n}\n")))
        } else if argv.first().map(String::as_str) == Some("dump") {
            let events = self.received.lock().await;
            let mut out = String::new();
            for (topic, payload) in events.iter() {
                use std::fmt::Write;
                let _ = writeln!(out, "{topic}:{}", String::from_utf8_lossy(payload));
            }
            Ok(CliOutput::ok(out))
        } else {
            Ok(CliOutput::ok(b"ok".to_vec()))
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A07 {
        received: Arc::new(Mutex::new(Vec::new())),
    })
    .run_stdio()
    .await
    .ok();
}
