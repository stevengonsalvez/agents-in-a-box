//! CTS axis A15 canary: `host/event_stream_subscribe`.
//!
//! Behaviour (per P3.2 spec):
//! 1. Manifests `event_stream_subscribe = ["workspace:*"]`.
//! 2. On `plugin/init`, subscribes to `workspace:cts-canary-<UUID>` and
//!    stashes the host-minted `stream_id`.
//! 3. Each `plugin/handle_event` whose topic starts with `stream:` and
//!    matches our `stream_id` increments a counter and emits a
//!    `host/log` line `SENTINEL_RX_<n>` (the anti-cheat witness — the
//!    host-side test reads these, proving the cap-allowed delivery path
//!    actually ran rather than being silently bypassed).
//! 4. On the 4th event the canary sends `host/event_stream_cancel`. The
//!    host then publishes a 5th event; the canary must NOT log
//!    `SENTINEL_RX_5` (cancellation honoured).
//!
//! CLI surface (driven by the host harness):
//! - `topic`  → prints the subscribed topic so the host can publish to it.
//! - `count`  → prints the number of stream events received.

use ainb_plugin_sdk::{
    Cell, CliOutput, Coord, HandleEventParams, HostClient, InitContext, LogLevel, Plugin,
    RenderParams, Result, Server, WireBuffer,
};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Shared canary state behind a `tokio::sync::Mutex` (the SDK serialises
/// handler access but we hold `Arc` clones across handlers).
#[derive(Default)]
struct State {
    /// Topic this canary subscribed to (unique per process).
    topic: String,
    /// Host-minted stream id for our subscription.
    stream_id: String,
    /// Count of stream events observed.
    received: usize,
    /// Whether we've already issued the cancel (after the 4th event).
    cancelled: bool,
    /// JSON-RPC error code if the init-time subscribe was denied (e.g.
    /// `-32001` when the cap is missing). `0` means subscribe succeeded.
    subscribe_err: i32,
}

struct A15 {
    state: Arc<Mutex<State>>,
}

#[async_trait]
impl Plugin for A15 {
    fn manifest(&self) -> &'static str {
        concat!(
            "[plugin]\n",
            "name = \"cts-a15\"\n",
            "version = \"0.0.1\"\n",
            "abi_version = 2\n",
            "[capabilities]\n",
            "event_stream_subscribe = [\"workspace:*\"]\n",
            "[provides]\n",
            "cli_namespaces = [\"a15\"]\n",
        )
    }

    async fn on_init(&mut self, host: &HostClient, _ctx: InitContext<'_>) -> Result<()> {
        // Unique topic per process so parallel canaries never collide.
        let topic = format!("workspace:cts-canary-{}", std::process::id());
        // Tolerate a denied subscribe: the cap-omitted host-test variant
        // registers this same binary WITHOUT the grant and expects
        // -32001. Stashing the error (instead of returning it) keeps
        // `plugin/init` succeeding so the host can read the code via the
        // `suberr` CLI rather than the plugin being torn down on init
        // failure.
        match host.event_stream_subscribe(&topic, None).await {
            Ok(res) => {
                let mut st = self.state.lock().await;
                st.topic = topic;
                st.stream_id = res.stream_id;
            }
            Err(ainb_plugin_sdk::SdkError::Rpc(rpc)) => {
                let mut st = self.state.lock().await;
                st.topic = topic;
                st.subscribe_err = rpc.code;
            }
            Err(_) => {}
        }
        Ok(())
    }

    async fn handle_event(&mut self, host: &HostClient, p: HandleEventParams) -> Result<()> {
        // Only count our own stream's events: topic == "stream:<stream_id>".
        let expected = {
            let st = self.state.lock().await;
            format!("stream:{}", st.stream_id)
        };
        if p.topic != expected {
            return Ok(());
        }
        let (n, cancel_id) = {
            let mut st = self.state.lock().await;
            st.received += 1;
            // After the 4th event, cancel exactly once.
            let cancel = if st.received == 4 && !st.cancelled {
                st.cancelled = true;
                Some(st.stream_id.clone())
            } else {
                None
            };
            (st.received, cancel)
        };
        // Anti-cheat sentinel: host-side test reads these via host/log.
        host.log(LogLevel::Info, format!("SENTINEL_RX_{n}"), None).await?;
        if let Some(id) = cancel_id {
            host.event_stream_cancel(id).await?;
        }
        Ok(())
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("S"));
        Ok(b)
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        _namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        match argv.first().map(String::as_str) {
            Some("topic") => {
                let st = self.state.lock().await;
                Ok(CliOutput::ok(format!("{}\n", st.topic)))
            }
            Some("count") => {
                let st = self.state.lock().await;
                Ok(CliOutput::ok(format!("{}\n", st.received)))
            }
            Some("suberr") => {
                let st = self.state.lock().await;
                Ok(CliOutput::ok(format!("{}\n", st.subscribe_err)))
            }
            _ => Ok(CliOutput::ok(b"ok".to_vec())),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A15 {
        state: Arc::new(Mutex::new(State::default())),
    })
    .run_stdio()
    .await
    .ok();
}
