//! CTS canary for the `event_bus` capability axis.
//!
//! Registered without a grant for the topic, it tries every snapshot-bus call
//! when the host runs `bus probe [topic]`, and prints what each returned:
//! `get:<code or ok> subscribe:<code or ok>`. It also publishes on the topic,
//! which the host must drop. The topic defaults to `cts.event_bus`.

use ainb_plugin_sdk::{
    Cell, CliOutput, Coord, HostClient, Plugin, RenderParams, Result, SdkError, Server, WireBuffer,
};
use async_trait::async_trait;

struct EventBusDenied;

fn code<T>(result: Result<T>) -> String {
    match result {
        Ok(_) => "ok".to_string(),
        Err(SdkError::Rpc(error)) => error.code.to_string(),
        Err(other) => format!("error({other})"),
    }
}

#[async_trait]
impl Plugin for EventBusDenied {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-event-bus-denied\"\nversion = \"0.0.1\"\nabi_version = 2\n[provides]\ncli_namespaces = [\"bus\"]\n"
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("B"));
        Ok(b)
    }

    async fn cli_dispatch(
        &mut self,
        host: &HostClient,
        _namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        if argv.first().map(String::as_str) != Some("probe") {
            return Ok(CliOutput::ok(b"ok".to_vec()));
        }
        let topic = argv.get(1).map_or("cts.event_bus", String::as_str);
        let get = code(host.snapshot_get(topic).await);
        let subscribe = code(host.snapshot_subscribe(topic).await);
        host.snapshot_publish(topic, b"leaked".to_vec()).await?;
        Ok(CliOutput::ok(format!("get:{get} subscribe:{subscribe}\n")))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(EventBusDenied).run_stdio().await.ok();
}
