//! End-to-end: a successful scan publishes `witr.snapshot` on the
//! event bus.
//!
//! Spawns the real plugin binary with a stub `witr` on `PATH`, drives
//! the JSON-RPC handshake + a key sequence (`t` → type a target →
//! Enter → `r`), and asserts a `host/snapshot/publish` reverse-call
//! frame appears with topic `witr.snapshot` and a non-empty payload.
//!
//! This proves the *wiring* (scan → `host.snapshot_publish`, correct
//! topic). The payload's byte-determinism + msgpack round-trip are
//! covered by `src/publish.rs` unit tests.
//!
//! cfg(unix): the stub is a `#!/bin/sh` script (chmod 755), and we
//! prepend its dir to the child's `PATH` so `which witr` resolves it.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use ainb_plugin_protocol::framing;
use serde_json::{Value, json};

fn binary_path() -> &'static str {
    env!("CARGO_BIN_EXE_ainb-plugin-witr")
}

const SAMPLE_JSON: &str = r#"{"Target":{"Type":"name","Value":"x"},"ResolvedTarget":"x","Process":{"PID":4242,"PPID":1,"Command":"x"},"RestartCount":0,"Ancestry":[],"Source":{"Type":"systemd","Name":"x"},"Warnings":[]}"#;

/// Stage a stub `witr` that satisfies detection (`--version` →
/// v0.3.2) and scans (`--json …` → canned snapshot). Returns the
/// directory to prepend to PATH.
fn stage_witr_stub(dir: &std::path::Path) {
    let json_file = dir.join("snap.json");
    std::fs::write(&json_file, SAMPLE_JSON).expect("write json");
    let bin = dir.join("witr");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  printf 'witr v0.3.2 (commit abc1234, built 2025-04-10T00:00:00Z)\\n'\n  exit 0\nfi\nif [ \"$1\" = \"--json\" ]; then\n  cat \"{json}\"\n  exit 0\nfi\nprintf 'stub passthrough\\n'\nexit 0\n",
        json = json_file.display(),
    );
    std::fs::write(&bin, &script).expect("write stub");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&bin).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).expect("chmod");
}

fn write_frame(w: &mut ChildStdin, body: &Value) {
    let bytes = serde_json::to_vec(body).expect("serialize");
    w.write_all(&framing::encode(&bytes)).expect("write");
    w.flush().expect("flush");
}

/// Spawn a reader thread that decodes frames off `stdout` and pushes
/// them onto a channel until EOF. Lets the test wait on frames with a
/// timeout (`recv_timeout`) instead of blocking forever on a frame
/// that never arrives.
fn spawn_frame_reader(stdout: ChildStdout) -> Receiver<Value> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut r = BufReader::new(stdout);
        loop {
            match read_one_frame(&mut r) {
                Some(v) => {
                    if tx.send(v).is_err() {
                        return; // receiver dropped — test done
                    }
                }
                None => return, // EOF
            }
        }
    });
    rx
}

fn read_one_frame<R: BufRead>(r: &mut R) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line).ok()?;
        if n == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let len = content_length?;
            let mut body = vec![0u8; len];
            std::io::Read::read_exact(r, &mut body).ok()?;
            return serde_json::from_slice(&body).ok();
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = value.trim().parse().ok();
            }
        }
    }
}

/// Pull frames off the channel until `pred` matches one or the overall
/// deadline elapses. Returns the matching frame or `None` on timeout.
fn wait_for(
    rx: &Receiver<Value>,
    timeout: Duration,
    pred: impl Fn(&Value) -> bool,
) -> Option<Value> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match rx.recv_timeout(remaining) {
            Ok(v) if pred(&v) => return Some(v),
            Ok(_) => continue,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// A `plugin/handle_key` notification. `HandleKeyParams` requires
/// `screen_id` + `key` + `generation` — omitting any makes the SDK
/// reject the frame with `-32602 missing field`.
fn key_frame(code: Value, generation: u64) -> Value {
    json!({
        "jsonrpc":"2.0","method":"plugin/handle_key",
        "params":{
            "screen_id":"witr",
            "key":{"code":code},
            "generation":generation
        }
    })
}

fn key_char(ch: char, generation: u64) -> Value {
    key_frame(json!({"type":"char","ch":ch.to_string()}), generation)
}

fn key_enter(generation: u64) -> Value {
    key_frame(json!({"type":"enter"}), generation)
}

#[test]
fn successful_scan_publishes_witr_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    stage_witr_stub(dir.path());
    let stub_path = format!("{}:/usr/bin:/bin", dir.path().display());

    let mut child = Command::new(binary_path())
        .env("PATH", &stub_path)
        .env("RUST_LOG", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn witr plugin");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout: ChildStdout = child.stdout.take().expect("stdout");
    let rx = spawn_frame_reader(stdout);

    // plugin/init — on_init runs detect against the stub (--version),
    // landing Ready. Reply is the manifest echo.
    write_frame(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":1,"method":"plugin/init",
            "params":{"manifest_path":"/tmp/witr/manifest.toml",
                "granted_capabilities":["spawn_subprocess","event_bus"],
                "abi_version":2}
        }),
    );

    // Wait for the init response (id=1). on_init issues no reverse
    // calls in v1, so it comes straight back.
    let init = wait_for(&rx, Duration::from_secs(10), |f| {
        f.get("id") == Some(&json!(1))
    })
    .expect("init response within 10s");
    assert_eq!(init["result"]["name"], "witr", "init echoes manifest");

    // Drive: t (enter target mode), 'x' (type a target), Enter
    // (commit), r (refresh → scan → publish). Targeting a name "x";
    // the stub returns the canned snapshot regardless.
    write_frame(&mut stdin, &key_char('t', 1));
    write_frame(&mut stdin, &key_char('x', 2));
    write_frame(&mut stdin, &key_enter(3));
    write_frame(&mut stdin, &key_char('r', 4));

    // Expect a host/snapshot/publish reverse call with our topic.
    let publish = wait_for(&rx, Duration::from_secs(10), |f| {
        f.get("method") == Some(&json!("host/snapshot/publish"))
    })
    .expect("a host/snapshot/publish frame after `r` scan");

    assert_eq!(
        publish["params"]["topic"], "witr.snapshot",
        "publish topic: {publish}"
    );
    // The wire codec (bytes_serde) base64-encodes the payload, so it's
    // always a non-empty JSON string here.
    let payload = &publish["params"]["payload"];
    assert!(
        payload.as_str().is_some_and(|s| !s.is_empty()),
        "payload must be a non-empty base64 string: {publish}"
    );

    // Shutdown.
    write_frame(
        &mut stdin,
        &json!({"jsonrpc":"2.0","method":"plugin/shutdown","params":{}}),
    );
    drop(stdin);
    let exit_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => return,
            None if Instant::now() > exit_deadline => {
                let _ = child.kill();
                return;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}
