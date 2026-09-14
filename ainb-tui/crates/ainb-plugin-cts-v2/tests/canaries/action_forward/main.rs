//! CTS canary for the `plugin/handle_action` axis.
//!
//! The host-side axis calls `RuntimeHandle::send_action(..)`, which the
//! runtime delivers as a `plugin/handle_action` notification. This canary
//! records each action in handler order, then publishes its view state on
//! the reserved `ui.state` topic, the path a renderer that draws the plugin's
//! screen itself reads what the action changed from. It exposes the record
//! via `cli_dispatch`:
//!
//! - `last`: the most recent action as `<action_id> <payload json>`, or
//!   `none` if no action has arrived yet.
//! - `count`: number of actions received so far.

use std::sync::Arc;

use ainb_plugin_sdk::{
    Cell, CliOutput, Coord, HandleActionParams, HostClient, Plugin, RenderParams, Result, Server,
    WireBuffer, topics,
};
use async_trait::async_trait;
use tokio::sync::Mutex;

struct ActionForward {
    last: Arc<Mutex<Option<String>>>,
    count: Arc<Mutex<usize>>,
}

#[async_trait]
impl Plugin for ActionForward {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"cts-action-forward\"\nversion = \"0.0.1\"\nabi_version = 2\n[provides]\ncli_namespaces = [\"action\"]\nsnapshots = [\"ui.state\"]\n"
    }

    async fn handle_action(&mut self, host: &HostClient, p: HandleActionParams) -> Result<()> {
        *self.last.lock().await = Some(format!("{} {}", p.action_id, p.payload));
        let count = {
            let mut count = self.count.lock().await;
            *count += 1;
            *count
        };
        let view = serde_json::json!({ "actions": count, "last": p.action_id });
        host.snapshot_publish(topics::UI_STATE, view.to_string().into_bytes())
            .await
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("A"));
        Ok(b)
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        _namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        match argv.first().map(String::as_str) {
            Some("last") => {
                let out = self
                    .last
                    .lock()
                    .await
                    .clone()
                    .unwrap_or_else(|| "none".to_string());
                Ok(CliOutput::ok(format!("{out}\n")))
            }
            Some("count") => {
                let n = *self.count.lock().await;
                Ok(CliOutput::ok(format!("{n}\n")))
            }
            _ => Ok(CliOutput::ok(b"ok".to_vec())),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(ActionForward {
        last: Arc::new(Mutex::new(None)),
        count: Arc::new(Mutex::new(0)),
    })
    .run_stdio()
    .await
    .ok();
}
