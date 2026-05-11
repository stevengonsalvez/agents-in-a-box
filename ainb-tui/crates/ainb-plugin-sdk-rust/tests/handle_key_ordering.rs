//! Integration test for `plugin/handle_key` ordering.
//!
//! Asserts that the SDK server preserves send-order across a 5-key
//! notification burst. Inline dispatch (see [`Server`] internals) is
//! load-bearing here: a tokio-spawned-per-handler implementation would
//! re-order acquisitions of the per-plugin mutex and the plugin would
//! observe `Tab` before `Char('2')`, breaking interactive UI semantics.
//!
//! Mirrors the regression guard the in-tree `handle_event` ordering fix
//! introduced (commit `4ad7b46`) but covers the new key path.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use ainb_plugin_sdk::{
    methods, HandleEventParams, HandleKeyParams, HostClient, KeyCode, KeyEvent, Plugin,
    RenderParams, Result, Server, WireBuffer,
};

/// Records every `handle_key` invocation in arrival order.
struct RecorderPlugin {
    keys: Arc<Mutex<Vec<KeyEvent>>>,
}

#[async_trait]
impl Plugin for RecorderPlugin {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"recorder\"\nversion = \"0.0.0\"\nabi_version = 2\n"
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        Ok(WireBuffer::new(0, 0))
    }

    async fn handle_event(&mut self, _host: &HostClient, _p: HandleEventParams) -> Result<()> {
        Ok(())
    }

    async fn handle_key(&mut self, _host: &HostClient, p: HandleKeyParams) -> Result<()> {
        self.keys.lock().await.push(p.key);
        Ok(())
    }
}

fn frame_notification(method: &str, params: Value) -> Vec<u8> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    ainb_plugin_protocol::framing::encode(&bytes)
}

fn key_params(screen_id: &str, code: KeyCode, generation: u64) -> Value {
    serde_json::to_value(HandleKeyParams {
        screen_id: screen_id.into(),
        key: KeyEvent {
            code,
            mods: 0,
            kind: Default::default(),
        },
        generation,
    })
    .unwrap()
}

#[tokio::test]
async fn five_sequential_handle_keys_preserve_order() {
    let (mut host_side, plugin_side) = tokio::io::duplex(64 * 1024);
    let (plugin_read, plugin_write) = tokio::io::split(plugin_side);

    let keys: Arc<Mutex<Vec<KeyEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let plugin = RecorderPlugin { keys: keys.clone() };
    let server_task = tokio::spawn(Server::new(plugin).run(plugin_read, plugin_write));

    // Five notifications: '1' -> '2' -> '3' -> Tab -> Esc.
    // generation bumps so the plugin can echo it via the next render
    // (we don't render here — just assert receive order).
    let sequence = [
        KeyCode::Char { ch: '1' },
        KeyCode::Char { ch: '2' },
        KeyCode::Char { ch: '3' },
        KeyCode::Tab,
        KeyCode::Esc,
    ];

    for (i, code) in sequence.iter().enumerate() {
        host_side
            .write_all(&frame_notification(
                methods::PLUGIN_HANDLE_KEY,
                key_params("ainb_analytics", code.clone(), i as u64),
            ))
            .await
            .unwrap();
    }
    host_side.shutdown().await.unwrap();

    // Drain anything the plugin writes back (nothing expected — handle_key
    // is a notification with no response and the plugin emits no logs).
    let mut host_read = BufReader::new(host_side);
    let mut buf = Vec::new();
    let _ = tokio::io::AsyncReadExt::read_to_end(&mut host_read, &mut buf).await;

    let _ = server_task.await.unwrap();

    let observed = keys.lock().await.clone();
    assert_eq!(observed.len(), 5, "expected 5 handle_key calls, got {}", observed.len());
    let observed_codes: Vec<KeyCode> = observed.into_iter().map(|k| k.code).collect();
    assert_eq!(
        observed_codes,
        sequence.to_vec(),
        "handle_key order was not preserved — sequence broke at: {observed_codes:?}"
    );
}
