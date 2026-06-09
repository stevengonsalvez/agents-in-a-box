//! CTS axis A18 canary: `host/secret_store_get` (scope/key contract, P5.2).
//!
//! Behaviour:
//! 1. Manifests `"secrets:read" = [...keys...]` — the host-test builds the
//!    manifest with the right grant form (list / bool-true / omitted) so a
//!    single binary covers every cap variant.
//! 2. Driven entirely by CLI so the host can register the same binary under
//!    different manifest grants. Args take a `scope` (`global`/`workspace`),
//!    an optional `workspace_id` (use `-` for none), and a `key`:
//!    - `get <scope> <workspace_id|-> <key>`    → read the secret; on
//!      success emit `host/log` sentinel `SECRET_READ_OK` (anti-cheat:
//!      proves the cap-allowed handler actually hit the backend, not a faked
//!      value), then print the decoded UTF-8 secret.
//!    - `geterr <scope> <workspace_id|-> <key>` → attempt the read; print
//!      the JSON-RPC error code (`-32001` cap denied / key not whitelisted,
//!      `-32004` not found, `-32005` not implemented) or `0` on success.
//!
//! The `SECRET_READ_OK` sentinel is emitted ONLY on the success path, so a
//! host-side assertion of its presence proves the backend hit happened —
//! and its ABSENCE on the not-found / denied paths proves the handler did
//! not silently fabricate a secret.

use ainb_plugin_sdk::{
    Cell, CliOutput, Coord, HostClient, LogLevel, Plugin, RenderParams, Result, Server, WireBuffer,
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};

struct A18;

/// Decode the `<workspace_id|->` positional into an `Option<String>`.
fn parse_ws(raw: &str) -> Option<String> {
    if raw == "-" || raw.is_empty() {
        None
    } else {
        Some(raw.to_owned())
    }
}

#[async_trait]
impl Plugin for A18 {
    fn manifest(&self) -> &'static str {
        // Manifest grant is overridden host-side; this default is only used
        // if the host registers the canary without a custom manifest.
        concat!(
            "[plugin]\n",
            "name = \"cts-a18\"\n",
            "version = \"0.0.1\"\n",
            "abi_version = 2\n",
            "[provides]\n",
            "cli_namespaces = [\"a18\"]\n",
        )
    }

    async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
        let mut b = WireBuffer::new(1, 1);
        b.push(Coord::new(0, 0), Cell::new("P"));
        Ok(b)
    }

    async fn cli_dispatch(
        &mut self,
        host: &HostClient,
        _namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        match argv.first().map(String::as_str) {
            Some("get") => {
                let scope = argv.get(1).cloned().unwrap_or_default();
                let ws = parse_ws(argv.get(2).map_or("-", String::as_str));
                let key = argv.get(3).cloned().unwrap_or_default();
                let res = host.secret_store_get(scope, ws, key).await?;
                // Anti-cheat sentinel: host reads this via host/log. Only the
                // genuine backend-hit success path reaches here.
                host.log(LogLevel::Info, "SECRET_READ_OK", None).await?;
                let bytes = B64.decode(res.value.as_bytes()).unwrap_or_default();
                Ok(CliOutput::ok(format!(
                    "{}\n",
                    String::from_utf8_lossy(&bytes)
                )))
            }
            Some("geterr") => {
                let scope = argv.get(1).cloned().unwrap_or_default();
                let ws = parse_ws(argv.get(2).map_or("-", String::as_str));
                let key = argv.get(3).cloned().unwrap_or_default();
                match host.secret_store_get(scope, ws, key).await {
                    Ok(_) => Ok(CliOutput::ok("0\n".to_string())),
                    Err(ainb_plugin_sdk::SdkError::Rpc(rpc)) => {
                        Ok(CliOutput::ok(format!("{}\n", rpc.code)))
                    }
                    Err(e) => Ok(CliOutput::ok(format!("err:{e}\n"))),
                }
            }
            _ => Ok(CliOutput::ok(b"ok".to_vec())),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A18).run_stdio().await.ok();
}
