//! CTS axis A17 canary: `host/unix_socket_dial`.
//!
//! Behaviour (per P3.4 spec):
//! 1. Manifests `unix_socket_dial = ["<tempdir>/hangar.sock"]` — the
//!    host-test passes the exact whitelisted path via CLI, so the manifest
//!    is built host-side with the right path (the cap-denied / bool-true /
//!    not-whitelisted variants override the grant).
//! 2. Driven entirely by CLI so the host can register the same binary
//!    under different manifest grants:
//!    - `dial <path>`     → dial `<path>`; stash the `stream_id`; emit
//!      `host/log` sentinel `SENTINEL_DIAL_OK` (anti-cheat: proves the
//!      cap-allowed handler ran), then print the `stream_id`.
//!    - `dialerr <path>`  → attempt to dial `<path>`; print the JSON-RPC
//!      error code (`-32001` cap denied / path not whitelisted, `-32003`
//!      bool-true grant rejected) or `0` on success.
//!    - `send <text>`     → write `<text>` to the dialled socket.
//!    - `rxcount`         → print how many `data` socket frames arrived.
//!    - `rxbytes`         → print the concatenated received bytes (UTF-8).
//!    - `close`           → close the dialled socket.
//!
//! On each `socket:<id>` `data` frame the canary emits `host/log`
//! `SENTINEL_RX_DATA` so the host can prove the read-loop delivery path
//! actually ran (not a faked counter).

use ainb_plugin_sdk::{
    Cell, CliOutput, Coord, HandleEventParams, HostClient, LogLevel, Plugin, RenderParams, Result,
    Server, UnixSocketEvent, UnixSocketEventKind, WireBuffer,
};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Shared canary state behind a `tokio::sync::Mutex`.
#[derive(Default)]
struct State {
    /// `stream_id` of the last successful dial (empty = none).
    stream_id: String,
    /// Count of `data` frames received.
    rx_count: u32,
    /// Concatenated received bytes.
    rx_bytes: Vec<u8>,
}

struct A17 {
    state: Arc<Mutex<State>>,
}

#[async_trait]
impl Plugin for A17 {
    fn manifest(&self) -> &'static str {
        // Manifest grant is overridden host-side; this default is only
        // used if the host registers the canary without a custom manifest.
        concat!(
            "[plugin]\n",
            "name = \"cts-a17\"\n",
            "version = \"0.0.1\"\n",
            "abi_version = 2\n",
            "[provides]\n",
            "cli_namespaces = [\"a17\"]\n",
        )
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("P"));
        Ok(b)
    }

    async fn handle_event(&mut self, host: &HostClient, params: HandleEventParams) -> Result<()> {
        if !params.topic.starts_with("socket:") {
            return Ok(());
        }
        let ev: UnixSocketEvent = match serde_json::from_slice(&params.payload) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        match ev.kind {
            UnixSocketEventKind::Data => {
                if let Some(bytes) = ev.bytes {
                    let mut st = self.state.lock().await;
                    st.rx_count += 1;
                    st.rx_bytes.extend_from_slice(&bytes);
                }
                // Anti-cheat sentinel: host reads this via host/log.
                host.log(LogLevel::Info, "SENTINEL_RX_DATA", None).await?;
            }
            UnixSocketEventKind::Eof => {
                host.log(LogLevel::Info, "SENTINEL_RX_EOF", None).await?;
            }
            UnixSocketEventKind::Error => {
                host.log(LogLevel::Info, "SENTINEL_RX_ERROR", None).await?;
            }
        }
        Ok(())
    }

    async fn cli_dispatch(
        &mut self,
        host: &HostClient,
        _namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        match argv.first().map(String::as_str) {
            Some("dial") => {
                let path = argv.get(1).cloned().unwrap_or_default();
                let res = host.unix_socket_dial(path).await?;
                {
                    let mut st = self.state.lock().await;
                    st.stream_id.clone_from(&res.stream_id);
                }
                host.log(LogLevel::Info, "SENTINEL_DIAL_OK", None).await?;
                Ok(CliOutput::ok(format!("{}\n", res.stream_id)))
            }
            Some("dialerr") => {
                let path = argv.get(1).cloned().unwrap_or_default();
                match host.unix_socket_dial(path).await {
                    Ok(_) => Ok(CliOutput::ok("0\n".to_string())),
                    Err(ainb_plugin_sdk::SdkError::Rpc(rpc)) => {
                        Ok(CliOutput::ok(format!("{}\n", rpc.code)))
                    }
                    Err(e) => Ok(CliOutput::ok(format!("err:{e}\n"))),
                }
            }
            Some("send") => {
                let text = argv.get(1).cloned().unwrap_or_default();
                let stream_id = self.state.lock().await.stream_id.clone();
                host.unix_socket_send(stream_id, text.into_bytes()).await?;
                Ok(CliOutput::ok("sent\n".to_string()))
            }
            Some("rxcount") => {
                let st = self.state.lock().await;
                Ok(CliOutput::ok(format!("{}\n", st.rx_count)))
            }
            Some("rxbytes") => {
                let st = self.state.lock().await;
                Ok(CliOutput::ok(format!(
                    "{}\n",
                    String::from_utf8_lossy(&st.rx_bytes)
                )))
            }
            Some("close") => {
                let stream_id = self.state.lock().await.stream_id.clone();
                host.unix_socket_close(stream_id).await?;
                Ok(CliOutput::ok("closed\n".to_string()))
            }
            _ => Ok(CliOutput::ok(b"ok".to_vec())),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A17 {
        state: Arc::new(Mutex::new(State::default())),
    })
    .run_stdio()
    .await
    .ok();
}
