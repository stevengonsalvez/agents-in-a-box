//! CTS canary for the `read_paths` capability + `[config]` injection axis.
//!
//! Declares a `read_paths` allow-list and a `[config]` schema. On a host
//! `host/fs/read_file` it routes the read through the host so the runtime's
//! path guard can enforce the `read_paths` envelope:
//!
//! - `read <abs-path>` — issue `host.read_file(<path>)`; reply `OK:<bytes>` on
//!   success, or `DENIED:<code>` / `ERR:<code>` when the host rejects it. The
//!   host-side axis test passes both an in-envelope and an out-of-envelope path
//!   and asserts the runtime allows the former and returns `-32001` for the
//!   latter.
//! - `config` — echo the JSON config the host injected at `plugin/init`, so the
//!   host-side axis can assert the resolved `[plugins.<name>]` table arrived.
//!
//! The `read_paths` allow-list is *not* baked into the static manifest string —
//! the host-side test stamps a per-run temp dir onto the registered `Manifest`,
//! mirroring how the runtime resolves the grant for real plugins.

use std::sync::Arc;

use ainb_plugin_sdk::{
    Cell, CliOutput, Coord, HostClient, InitContext, Plugin, RenderParams, Result, SdkError,
    Server, WireBuffer,
};
use async_trait::async_trait;
use tokio::sync::Mutex;

struct ReadPathsConfig {
    /// The JSON config the host injected at init, stashed so `cli_dispatch`
    /// can echo it back to the host-side axis test.
    injected_config: Arc<Mutex<serde_json::Value>>,
}

#[async_trait]
impl Plugin for ReadPathsConfig {
    fn manifest(&self) -> &'static str {
        // read_paths is granted via the registered Manifest the host stamps,
        // not this static string (the allow-list is a per-run temp dir).
        "[plugin]\nname = \"cts-read-paths-config\"\nversion = \"0.0.1\"\nabi_version = 2\n[provides]\ncli_namespaces = [\"rpc\"]\n"
    }

    async fn on_init(&mut self, _host: &HostClient, ctx: InitContext<'_>) -> Result<()> {
        *self.injected_config.lock().await = ctx.config.clone();
        Ok(())
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("R"));
        Ok(b)
    }

    async fn cli_dispatch(
        &mut self,
        host: &HostClient,
        _namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        match argv.first().map(String::as_str) {
            // `read <abs-path>` — route the read through the host so the
            // runtime's read_paths guard decides allow vs deny.
            Some("read") => {
                let path = argv.get(1).cloned().unwrap_or_default();
                match host.read_file(path).await {
                    Ok(res) => Ok(CliOutput::ok(format!("OK:{}\n", res.bytes.len()))),
                    Err(SdkError::Rpc(rpc)) => Ok(CliOutput::ok(format!("DENIED:{}\n", rpc.code))),
                    Err(other) => Ok(CliOutput::ok(format!("ERR:{other}\n"))),
                }
            }
            // `config` — echo back exactly the JSON the host injected at init.
            Some("config") => {
                let cfg = self.injected_config.lock().await.clone();
                Ok(CliOutput::ok(format!("{cfg}\n")))
            }
            _ => Ok(CliOutput::ok(b"ok".to_vec())),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(ReadPathsConfig {
        injected_config: Arc::new(Mutex::new(serde_json::Value::Null)),
    })
    .run_stdio()
    .await
    .ok();
}
