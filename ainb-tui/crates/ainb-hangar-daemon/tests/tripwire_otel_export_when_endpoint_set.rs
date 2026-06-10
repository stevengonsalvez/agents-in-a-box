//! P8.7 e2e tripwire 3 — the `task.claim` service span is exported over OTLP
//! when the endpoint is set (feature-gated).
//!
//! ```text
//!  mock OTLP/HTTP receiver (random loopback port)
//!         ▲ POST /v1/traces (protobuf spans)
//!         │
//!  ainb-hangar-daemon (CLAIM ENABLED, OTEL_EXPORTER_OTLP_ENDPOINT set on child)
//!         │  claims seeded queued task
//!         ▼
//!  ClaimTaskService::claim_for_runtime  ──#[instrument(name="task.claim",
//!                                            fields(task_id, …))]──▶ span
//! ```
//!
//! Unlike P8.2's `it_otlp_export_when_endpoint_set.rs` (which installs the
//! subscriber in-process and emits a synthetic `p8_2_probe` span), this drives a
//! **real `task.claim` span end-to-end**: a genuine `ainb-hangar-daemon` binary
//! (built with `--features otlp`, so its `main` calls `OtlpOpts::from_env`) is
//! spawned with `OTEL_EXPORTER_OTLP_ENDPOINT` pointed at a mock receiver. We seed
//! one `queued` task; the daemon's claim loop calls
//! [`ClaimTaskService::claim_for_runtime`](ainb_hangar_store::service::claim),
//! which P8.3 instrumented with the `task.claim` span carrying a `task_id` field.
//! The mock receiver captures the exported protobuf bodies; we assert one carries
//! the span name `task.claim` and the `task_id` attribute key (both survive
//! verbatim as UTF-8 byte runs inside the OTLP protobuf).
//!
//! ## Determinism
//!
//! The endpoint env is set on the **child `Command` only** (never the test
//! process), so this needs no `ENV_LOCK` — there is no global env mutation to
//! serialise. The receiver binds a random loopback port. `OTEL_BSP_SCHEDULE_DELAY`
//! is set low on the child so the batch processor flushes the claim span well
//! within the receiver's wait budget.
//!
//! Gated behind `#![cfg(feature = "otlp")]`: without the feature the daemon binary
//! links no OTEL stack, so there is nothing to export and this file compiles to
//! nothing.
#![cfg(feature = "otlp")]
#![allow(clippy::duration_suboptimal_units)]

use std::path::Path;
use std::process::{Child, Command};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Spin a minimal HTTP/1.1 receiver on a random loopback port that accumulates
/// every request body it receives over a channel (the batch processor may POST
/// more than once). Returns `(base_url, body_receiver)`.
async fn spawn_otlp_receiver() -> (String, mpsc::Receiver<Vec<u8>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback receiver");
    let port = listener.local_addr().expect("local addr").port();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let mut content_len: Option<usize> = None;
                let mut header_end: Option<usize> = None;
                loop {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if header_end.is_none() {
                                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                    header_end = Some(pos + 4);
                                    let head = String::from_utf8_lossy(&buf[..pos]);
                                    for line in head.lines() {
                                        if let Some(v) = line
                                            .to_ascii_lowercase()
                                            .strip_prefix("content-length:")
                                        {
                                            content_len = v.trim().parse().ok();
                                        }
                                    }
                                }
                            }
                            if let (Some(he), Some(cl)) = (header_end, content_len) {
                                if buf.len() >= he + cl {
                                    let _ = tx.send(buf[he..he + cl].to_vec());
                                    break;
                                }
                            }
                        }
                    }
                }
                let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
                let _ = sock.flush().await;
            });
        }
    });

    (format!("http://127.0.0.1:{port}"), rx)
}

/// An RAII daemon child killed (by its own pid only) on drop.
struct DaemonChild(Child);
impl Drop for DaemonChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn task_claim_span_exported_over_otlp_when_endpoint_set() {
    // SKIP cleanly without the daemon binary (e.g. a stripped CI build).
    let daemon_bin = assert_cmd::cargo::cargo_bin("ainb-hangar-daemon");
    if !daemon_bin.exists() {
        eprintln!("SKIP: ainb-hangar-daemon binary not built; skipping OTLP task.claim tripwire");
        return;
    }

    let (base, rx) = spawn_otlp_receiver().await;

    // Isolated $HOME with a seeded DB the daemon will claim from.
    let home = tempfile::tempdir().expect("isolated HOME");
    let hangar_dir = home.path().join(".ainb");
    std::fs::create_dir_all(&hangar_dir).expect("create ~/.ainb");
    let (runtime_id, task_id) = seed_claimable(&hangar_dir).await;

    // A fake-claude so the post-claim execute path doesn't error on a missing
    // binary (we only care that the claim span fires + exports).
    let fake_claude = write_fake_claude(home.path());

    // Spawn the OTLP-enabled daemon with the endpoint set ON THE CHILD ONLY (no
    // global env mutation → no ENV_LOCK needed). A low BSP schedule delay flushes
    // the claim span promptly.
    let child = Command::new(&daemon_bin)
        .env("HOME", home.path())
        .env_remove("AINB_HANGAR_HOME")
        .env("HANGAR_DAEMON_RUNTIME_ID", &runtime_id)
        .env("HANGAR_CLAUDE_PATH", &fake_claude)
        .env("HANGAR_DAEMON_POLL_MS", "200")
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", &base)
        .env("OTEL_BSP_SCHEDULE_DELAY", "500")
        .env("RUST_LOG", "info")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn otlp daemon");
    let _child = DaemonChild(child);

    // Accumulate exported bodies until one carries the `task.claim` span name +
    // the `task_id` attribute, or the budget elapses.
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut saw_claim_name = false;
    let mut saw_task_id = false;
    let mut saw_seeded_task_id = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining.min(Duration::from_secs(5))) {
            Ok(body) => {
                let hay = String::from_utf8_lossy(&body);
                if hay.contains("task.claim") {
                    saw_claim_name = true;
                }
                if hay.contains("task_id") {
                    saw_task_id = true;
                }
                if hay.contains(task_id.as_str()) {
                    saw_seeded_task_id = true;
                }
                if saw_claim_name && saw_task_id {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    assert!(
        saw_claim_name,
        "expected an exported OTLP span named `task.claim` (the P8.3-instrumented \
         claim path); none seen within budget"
    );
    assert!(
        saw_task_id,
        "expected the exported `task.claim` span to carry the `task_id` field key"
    );
    // Stronger end-to-end proof: the seeded task's id appears in the export,
    // confirming the span recorded the row it actually claimed.
    assert!(
        saw_seeded_task_id,
        "expected the seeded task id `{task_id}` in the exported span attributes"
    );
}

/// Seed the minimal world + one `queued` task the daemon can claim. Returns
/// `(runtime_id, task_id)`. `created_at` is wall-clock now so the queued-TTL
/// sweeper doesn't reap it first.
async fn seed_claimable(hangar_dir: &Path) -> (String, String) {
    let store = ainb_hangar_store::Store::open_in(hangar_dir).await.expect("open seed store");
    let pool = store.pool();
    let runtime_id = "rt-otel".to_string();
    let task_id = "task-otel-claim-1".to_string();

    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-otel")
        .bind("otel")
        .bind("Otel")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("user-otel")
        .bind("otel@example.com")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert user");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&runtime_id)
    .bind("ws-otel")
    .bind("daemon-otel")
    .bind("claude")
    .bind("local")
    .execute(pool)
    .await
    .expect("insert runtime");
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, visibility, owner_id, max_concurrent_tasks) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("agent-otel")
    .bind("ws-otel")
    .bind("Agent")
    .bind(&runtime_id)
    .bind("workspace")
    .bind("user-otel")
    .bind(5_i64)
    .execute(pool)
    .await
    .expect("insert agent");

    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    )
    .unwrap_or(i64::MAX);
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
         VALUES (?, ?, ?, ?, NULL, 'queued', ?)",
    )
    .bind(&task_id)
    .bind("ws-otel")
    .bind(&runtime_id)
    .bind("agent-otel")
    .bind(now_ms)
    .execute(pool)
    .await
    .expect("enqueue claimable task");

    (runtime_id, task_id)
}

/// Write an executable fake-`claude` (emits a result line, exits 0) so the
/// post-claim execute path completes cleanly.
fn write_fake_claude(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("fake-claude-otel.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\necho '{\"type\":\"system\",\"session_id\":\"otel-1\"}'\n\
         echo '{\"type\":\"result\",\"content\":\"ok\"}'\nexit 0\n",
    )
    .expect("write fake-claude");
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("stat").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
    }
    path
}
