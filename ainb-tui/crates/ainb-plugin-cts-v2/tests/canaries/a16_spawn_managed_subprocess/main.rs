//! CTS axis A16 canary: `host/spawn_managed_subprocess`.
//!
//! Behaviour (per P3.3 spec):
//! 1. Manifests `spawn_managed_subprocess = ["sleep", "sh"]` (list form;
//!    the host-test cap-denied / bool-true variants override this).
//! 2. Driven entirely by CLI (no auto-spawn on init) so the host can
//!    register the same binary under different manifest grants:
//!    - `spawn`        → spawn `sleep 30`; stash handle+pid; emit
//!      `host/log` sentinel `SENTINEL_SPAWN_OK` (anti-cheat: proves the
//!      cap-allowed handler actually ran), then print the pid.
//!    - `pid`          → print the last spawned child's pid (or `0`).
//!    - `spawnerr <bin>` → attempt to spawn `<bin>`; print the JSON-RPC
//!      error code (e.g. `-32001` cap denied / bin not whitelisted,
//!      `-32003` bool-true grant rejected) or `0` on success.
//!    - `spawnenv <out>` → spawn `sh -c 'printenv > <out>'` with
//!      `env_allowlist = ["CTS_MANAGED_KEEP"]`; print the spawned pid.
//!      The host reads `<out>` and asserts only the allow-listed var
//!      survived.
//!
//! The actual leak-guard reap (child dies on plugin shutdown) is asserted
//! host-side: the host learns the pid via `pid`, then drops the runtime
//! and probes the pid with `kill(pid, 0)`.

use ainb_plugin_sdk::{
    Cell, CliOutput, Coord, HostClient, LogLevel, Plugin, RenderParams, Result, Server, WireBuffer,
};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Shared canary state behind a `tokio::sync::Mutex`.
#[derive(Default)]
struct State {
    /// pid of the last successfully spawned managed child (`0` = none).
    last_pid: u32,
}

struct A16 {
    state: Arc<Mutex<State>>,
}

#[async_trait]
impl Plugin for A16 {
    fn manifest(&self) -> &'static str {
        concat!(
            "[plugin]\n",
            "name = \"cts-a16\"\n",
            "version = \"0.0.1\"\n",
            "abi_version = 2\n",
            "[capabilities]\n",
            "spawn_managed_subprocess = [\"sleep\", \"sh\"]\n",
            "[provides]\n",
            "cli_namespaces = [\"a16\"]\n",
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
            Some("spawn") => {
                let res = host
                    .spawn_managed_subprocess("sleep", vec!["30".into()], vec![], None)
                    .await?;
                {
                    let mut st = self.state.lock().await;
                    st.last_pid = res.pid;
                }
                // Anti-cheat sentinel: host-side test reads this via host/log.
                host.log(LogLevel::Info, "SENTINEL_SPAWN_OK", None).await?;
                Ok(CliOutput::ok(format!("{}\n", res.pid)))
            }
            Some("pid") => {
                let st = self.state.lock().await;
                Ok(CliOutput::ok(format!("{}\n", st.last_pid)))
            }
            Some("spawnerr") => {
                let bin = argv.get(1).cloned().unwrap_or_default();
                match host
                    .spawn_managed_subprocess(bin, vec![], vec![], None)
                    .await
                {
                    Ok(_) => Ok(CliOutput::ok("0\n".to_string())),
                    Err(ainb_plugin_sdk::SdkError::Rpc(rpc)) => {
                        Ok(CliOutput::ok(format!("{}\n", rpc.code)))
                    }
                    Err(e) => Ok(CliOutput::ok(format!("err:{e}\n"))),
                }
            }
            Some("spawnenv") => {
                let out = argv.get(1).cloned().unwrap_or_default();
                let script = format!("printenv > {out}");
                let res = host
                    .spawn_managed_subprocess(
                        "sh",
                        vec!["-c".into(), script],
                        vec!["CTS_MANAGED_KEEP".into()],
                        None,
                    )
                    .await?;
                Ok(CliOutput::ok(format!("{}\n", res.pid)))
            }
            _ => Ok(CliOutput::ok(b"ok".to_vec())),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Server::new(A16 {
        state: Arc::new(Mutex::new(State::default())),
    })
    .run_stdio()
    .await
    .ok();
}
