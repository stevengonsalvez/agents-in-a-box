//! Run the conformance axes against the REAL in-tree plugin binaries.
//!
//! The canary suite in `tests/axes.rs` proves the RUNTIME is conformant: it
//! builds a synthetic plugin per axis and drives it. It has never proved that
//! the plugins we actually ship satisfy the protocol they claim. This module
//! supplies the missing half:
//!
//! - [`IN_TREE_PLUGINS`]: the descriptors, one per shipped ABI-v2 plugin.
//! - [`resolve_binary`] : builds the plugin's `[[bin]]` and returns its path,
//!   so the axes run against the same artifact a user installs.
//! - [`RawPlugin`]      : a direct Content-Length/JSON-RPC stdio client, for
//!   the axes whose observable side effect (a `-32601` reply, a process exit
//!   status) is not reachable through `RuntimeHandle`.
//!
//! The host-side axes live in `tests/real_plugin_axes.rs`.

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_plugin_protocol::framing;
use serde_json::Value;

/// One shipped ABI-v2 subprocess plugin.
///
/// Membership rule: the crate depends on `ainb-plugin-sdk-rust` and ships a
/// manifest describing an `abi_version = 2` plugin. That excludes the
/// libraries (`-protocol`, `-runtime`, `-sdk-rust`, `-types-sessions`), the
/// test harnesses (`-testkit`, `-cts-v2`), and `-notifyd`, which is a
/// standalone notification daemon with no manifest and no SDK dependency;
/// it does not speak the plugin ABI at all.
#[derive(Debug, Clone, Copy)]
pub struct PluginUnderTest {
    /// Row label in the matrix; matches `[plugin].name` in the manifest.
    pub name: &'static str,
    /// Cargo package to build.
    pub package: &'static str,
    /// `[[bin]]` target inside that package.
    pub bin: &'static str,
    /// Manifest file, relative to the package directory.
    pub manifest_file: &'static str,
}

/// Every in-tree plugin the axes run against.
pub const IN_TREE_PLUGINS: &[PluginUnderTest] = &[
    PluginUnderTest {
        name: "hangar-tui",
        package: "ainb-plugin-hangar",
        bin: "ainb-plugin-hangar",
        manifest_file: "manifest.toml",
    },
    PluginUnderTest {
        name: "burndown",
        package: "ainb-plugin-burndown",
        bin: "ainb-plugin-burndown",
        manifest_file: "plugin.toml",
    },
    PluginUnderTest {
        name: "session-reader",
        package: "ainb-plugin-session-reader",
        bin: "ainb-plugin-session-reader",
        manifest_file: "manifest.toml",
    },
    PluginUnderTest {
        name: "witr",
        package: "ainb-plugin-witr",
        bin: "ainb-plugin-witr",
        manifest_file: "manifest.toml",
    },
    PluginUnderTest {
        name: "learnings",
        package: "ainb-plugin-learnings",
        bin: "ainb-plugin-learnings",
        manifest_file: "manifest.toml",
    },
    PluginUnderTest {
        name: "abtop",
        package: "ainb-plugin-abtop",
        bin: "ainb-plugin-abtop",
        manifest_file: "manifest.toml",
    },
];

impl PluginUnderTest {
    /// Directory holding this plugin's `Cargo.toml`.
    pub fn crate_dir(&self) -> PathBuf {
        crates_dir().join(self.package)
    }

    /// Absolute path to this plugin's manifest file.
    pub fn manifest_path(&self) -> PathBuf {
        self.crate_dir().join(self.manifest_file)
    }
}

/// `<workspace>/crates`, derived from this crate's own manifest dir.
fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("ainb-plugin-cts-v2 lives under crates/")
        .to_path_buf()
}

/// Cargo workspace root (`ainb-tui/`).
pub fn workspace_root() -> PathBuf {
    crates_dir()
        .parent()
        .expect("crates/ lives under the workspace root")
        .to_path_buf()
}

/// Build `plugin`'s binary and return its path.
///
/// `CARGO_BIN_EXE_<name>` only covers targets in the *same* package, so the
/// axes have to build the plugin themselves. Cargo releases the build lock
/// before it executes test binaries, so this nested build is safe and, on a
/// warm target dir, near-instant. A failure is loud: a plugin whose binary
/// will not build cannot be silently dropped from the matrix.
pub fn resolve_binary(plugin: &PluginUnderTest) -> Result<PathBuf, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(workspace_root())
        .args([
            "build",
            "--package",
            plugin.package,
            "--bin",
            plugin.bin,
            "--message-format=json-render-diagnostics",
        ])
        // Cargo sets these for the test process; leaking them into the nested
        // build makes cargo think it is being invoked from a build script.
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_MAKEFLAGS")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if is_release_profile() {
        cmd.arg("--release");
    }

    let out = cmd
        .output()
        .map_err(|e| format!("spawn cargo build for {}: {e}", plugin.package))?;
    if !out.status.success() {
        return Err(format!(
            "cargo build --package {} --bin {} failed with {}",
            plugin.package, plugin.bin, out.status
        ));
    }

    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v.get("reason").and_then(Value::as_str) == Some("compiler-artifact"))
        .filter(|v| {
            v.pointer("/target/name").and_then(Value::as_str) == Some(plugin.bin)
                && v.pointer("/target/kind/0").and_then(Value::as_str) == Some("bin")
        })
        .filter_map(|v| v.get("executable").and_then(Value::as_str).map(PathBuf::from))
        .next_back()
        .ok_or_else(|| {
            format!(
                "cargo build emitted no executable artifact for {}::{}",
                plugin.package, plugin.bin
            )
        })
}

/// Whether the test binary itself was built in the release profile, so the
/// nested build produces artifacts alongside it rather than a second copy.
fn is_release_profile() -> bool {
    std::env::current_exe()
        .is_ok_and(|p| p.components().any(|c| c.as_os_str().eq_ignore_ascii_case("release")))
}

// =====================================================================
// Raw stdio client
// =====================================================================

/// A direct Content-Length/JSON-RPC client bolted onto a plugin's stdio.
///
/// Used for the axes whose observable side effect is not reachable through
/// `RuntimeHandle`: the `-32601` reply to an unknown method, the
/// `plugin/init` name/version echo, and the child's EXIT STATUS after a
/// graceful shutdown.
///
/// A background reader thread answers any host-bound request the plugin makes
/// (see [`stub_host_reply`]) so a plugin that calls the host during `on_init`
/// or `on_shutdown` never blocks the axis waiting for a host that is not
/// there.
pub struct RawPlugin {
    child: Child,
    stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
    replies: Receiver<Value>,
    next_id: i64,
}

impl RawPlugin {
    /// Spawn `bin` with piped stdio and start the reader thread.
    pub fn spawn(bin: &Path) -> Result<Self, String> {
        let mut child = Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Not piped: nothing drains it, and a full stderr pipe would
            // wedge the plugin mid-axis.
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", bin.display()))?;

        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = Arc::new(Mutex::new(Some(child.stdin.take().expect("piped stdin"))));
        let (tx, replies) = channel();
        let writer = Arc::clone(&stdin);

        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(frame)) = framing::read_frame(&mut reader) {
                let Ok(value) = serde_json::from_slice::<Value>(&frame) else {
                    continue;
                };
                match (
                    value.get("method").and_then(Value::as_str),
                    value.get("id").cloned(),
                ) {
                    // Host-bound request: answer it so the plugin proceeds.
                    (Some(method), Some(id)) => {
                        let mut ack = serde_json::json!({"jsonrpc": "2.0", "id": id});
                        match stub_host_reply(method) {
                            Ok(result) => ack["result"] = result,
                            Err((code, message)) => {
                                ack["error"] = serde_json::json!({
                                    "code": code, "message": message
                                });
                            }
                        }
                        write_frame(&writer, &ack);
                    }
                    // Reply to one of our requests.
                    (None, Some(_)) => {
                        if tx.send(value).is_err() {
                            break;
                        }
                    }
                    // Host-bound notification (host/log, ...), or a frame with
                    // neither a method nor an id: nothing to answer.
                    (Some(_) | None, _) => {}
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            replies,
            next_id: 1,
        })
    }

    /// Send a request and wait for its reply.
    ///
    /// Returns `Ok(result)` on success and `Err((code, message))` for a
    /// JSON-RPC error reply.
    pub fn request(
        &mut self,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Result<Value, (i64, String)>, String> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        });
        write_frame(&self.stdin, &frame);

        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let reply = match self.replies.recv_timeout(remaining) {
                Ok(v) => v,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("no reply to {method} within {timeout:?}"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!("plugin closed stdout before replying to {method}"));
                }
            };
            if reply.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            if let Some(err) = reply.get("error") {
                let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
                let message =
                    err.get("message").and_then(Value::as_str).unwrap_or_default().to_owned();
                return Ok(Err((code, message)));
            }
            return Ok(Ok(reply.get("result").cloned().unwrap_or(Value::Null)));
        }
    }

    /// Close the plugin's stdin, which is how the host signals "no more
    /// frames"; a conformant plugin then drains and exits.
    pub fn close_stdin(&self) {
        drop(self.stdin.lock().expect("stdin mutex").take());
    }

    /// Wait for the child to exit, returning its exit code.
    pub fn wait_exit(&mut self, timeout: Duration) -> Result<Option<i32>, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Ok(status.code()),
                Ok(None) => {}
                Err(e) => return Err(format!("try_wait: {e}")),
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!("plugin still running after {timeout:?}"));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for RawPlugin {
    fn drop(&mut self) {
        self.close_stdin();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The capability keys a manifest actually asks for.
///
/// A key counts as asked-for when its grant is bool-true or a non-empty list.
/// Used as `plugin/init`'s `granted_capabilities`, so the raw probe hands the
/// plugin the same set a permissive host would.
pub fn declared_capability_keys(manifest: &ainb_plugin_protocol::Manifest) -> Vec<String> {
    let Ok(Value::Object(map)) = serde_json::to_value(&manifest.capabilities) else {
        return Vec::new();
    };
    map.into_iter()
        .filter(|(_, v)| match v {
            Value::Bool(b) => *b,
            Value::Array(a) => !a.is_empty(),
            _ => false,
        })
        .map(|(k, _)| k)
        .collect()
}

/// What the stub host answers for a plugin-initiated request.
///
/// The raw probe is a MINIMAL host. It answers the pure bookkeeping calls a
/// plugin makes while coming up (snapshot get/subscribe, stream subscribe,
/// workspace reads) with the default-shaped result those methods return, and
/// denies the calls that would require the host to actually DO something it
/// cannot here (spawn a child, dial a socket, read the user's filesystem,
/// open the keychain). A plugin must survive a denial on those: a real host
/// denies them too whenever the capability is not granted.
///
/// Anything unlisted gets `-32601`, which is the honest answer from a host
/// that does not implement the method.
fn stub_host_reply(method: &str) -> Result<Value, (i32, String)> {
    use ainb_plugin_protocol::{errors, methods};
    match method {
        methods::HOST_SNAPSHOT_GET => {
            Ok(serde_json::json!({ "payload": Value::Null, "version": 0 }))
        }
        methods::HOST_SNAPSHOT_SUBSCRIBE
        | methods::HOST_WORKSPACE_GET_ACTIVE
        | methods::HOST_WORKSPACE_SET_ACTIVE
        | methods::HOST_WORKSPACE_SET_DEFAULT => Ok(serde_json::json!({})),
        methods::HOST_EVENT_STREAM_SUBSCRIBE => {
            Ok(serde_json::json!({ "stream_id": "cts-stub-stream", "version": 0 }))
        }
        methods::HOST_WORKSPACE_LIST => Ok(serde_json::json!({ "workspaces": [] })),
        methods::HOST_SPAWN_MANAGED_SUBPROCESS
        | methods::HOST_UNIX_SOCKET_DIAL
        | methods::HOST_UNIX_SOCKET_SEND
        | methods::HOST_FS_READ_FILE
        | methods::HOST_FS_READ_DIR
        | methods::HOST_NETWORK_FETCH
        | methods::HOST_SECRET_STORE_GET
        | methods::HOST_ACTION_INVOKE
        | methods::HOST_WORKSPACE_CREATE
        | methods::HOST_WORKSPACE_DELETE => Err((
            errors::CAPABILITY_DENIED,
            format!("{method} is not available from the CTS stub host"),
        )),
        other => Err((
            errors::METHOD_NOT_FOUND,
            format!("CTS stub host does not implement {other}"),
        )),
    }
}

/// Write one Content-Length framed JSON value, ignoring a closed pipe.
fn write_frame(stdin: &Arc<Mutex<Option<std::process::ChildStdin>>>, value: &Value) {
    let Ok(mut guard) = stdin.lock() else {
        return;
    };
    let Some(w) = guard.as_mut() else {
        return;
    };
    let body = serde_json::to_vec(value).expect("serialize frame");
    if framing::write_frame(w, &body).is_ok() {
        let _ = w.flush();
    }
}
