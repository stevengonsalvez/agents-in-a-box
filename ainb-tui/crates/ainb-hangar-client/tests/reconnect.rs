//! Integration and unit tests for reconnecting daemon client (#P6).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ainb_hangar_client::{DaemonClient, FleetStreamEvent};
use ainb_hangar_client::reconnect::{BACKOFF_1S, BACKOFF_4S, BACKOFF_16S, ConnectionState, Timing};
use tokio::sync::watch;

fn daemon_bin() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("AINB_DAEMON_BIN") {
        let p = PathBuf::from(bin);
        if p.is_file() {
            return Some(p);
        }
    }
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
            manifest_dir.join("../../target")
        });
    let bin = target_dir.join("debug/ainb-hangar-daemon");
    if bin.is_file() {
        Some(bin)
    } else {
        eprintln!(
            "daemon binary not found at {}: skipping test",
            bin.display()
        );
        None
    }
}

struct DaemonProcess {
    child: Child,
}

impl DaemonProcess {
    fn spawn(home: &Path) -> Option<Self> {
        let bin = daemon_bin()?;
        let log_path = home.join("daemon.log");
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("open daemon log");
        let err_file = log_file.try_clone().expect("clone daemon log");

        let mut cmd = Command::new(&bin);
        cmd.env("AINB_HANGAR_HOME", home)
            .env("AINB_CODEX_MANAGED", "0")
            .stdin(Stdio::null())
            .stdout(log_file)
            .stderr(err_file);

        let child = cmd.spawn().expect("spawn daemon");
        Some(Self { child })
    }

    fn kill_sigkill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn wait_for_daemon_ready(home: &Path) -> (PathBuf, String) {
    let socket = ainb_hangar_client::socket_path_in(home);
    let token_path = ainb_hangar_proto::auth::token_file_in(home);
    let deadline = Instant::now() + Duration::from_secs(30);

    while Instant::now() < deadline {
        if socket.exists() && token_path.exists() {
            if let Ok(token) = std::fs::read_to_string(&token_path) {
                let trimmed = token.trim();
                if !trimmed.is_empty() {
                    let client = DaemonClient::with_parts(socket.clone(), trimmed.to_string());
                    if client.hello().await.is_ok() {
                        return (socket, trimmed.to_string());
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "daemon at {} did not become ready within 30s",
        home.display()
    );
}

async fn wait_for_condition<F>(
    state_rx: &mut watch::Receiver<ConnectionState>,
    timeout: Duration,
    mut condition: F,
) -> ConnectionState
where
    F: FnMut(&ConnectionState) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        {
            let current = state_rx.borrow();
            if condition(&current) {
                return current.clone();
            }
        }
        let now = Instant::now();
        if now >= deadline {
            let current = state_rx.borrow();
            panic!("condition not met before timeout (state: {:?})", *current);
        }
        let remaining = deadline - now;
        tokio::select! {
            res = state_rx.changed() => {
                if res.is_err() {
                    panic!("state channel closed unexpectedly");
                }
            }
            () = tokio::time::sleep(remaining) => {}
        }
    }
}

/// Unit test verifying that connection states drive renderer banners and stale badge behavior.
#[test]
fn test_renderer_frozen_with_stale_badge_unit_test() {
    let connected = ConnectionState::Connected;
    assert!(connected.is_connected());
    assert!(!connected.is_reconnecting());
    assert!(!connected.is_closed());
    assert_eq!(connected.renderer_view().banner, None);
    assert!(!connected.sections_stale_and_frozen());

    let connected_view = connected.renderer_view();
    assert_eq!(connected_view.banner, None);
    assert!(!connected_view.stale_badge);
    assert!(!connected_view.frozen);

    let reconnecting = ConnectionState::Reconnecting {
        delay: Duration::from_secs(1),
        attempt: 1,
        error: Some("socket dropped".to_string()),
    };
    assert!(!reconnecting.is_connected());
    assert!(reconnecting.is_reconnecting());
    assert!(!reconnecting.is_closed());
    assert_eq!(reconnecting.renderer_view().banner, Some("reconnecting"));
    assert!(reconnecting.sections_stale_and_frozen());

    let rec_view = reconnecting.renderer_view();
    assert_eq!(rec_view.banner, Some("reconnecting"));
    assert!(rec_view.stale_badge);
    assert!(rec_view.frozen);

    // Named constants match spec
    assert_eq!(BACKOFF_1S, Duration::from_secs(1));
    assert_eq!(BACKOFF_4S, Duration::from_secs(4));
    assert_eq!(BACKOFF_16S, Duration::from_secs(16));

    let timing = Timing::default();
    assert_eq!(timing.delay_for_attempt(1), Duration::from_secs(1));
    assert_eq!(timing.delay_for_attempt(2), Duration::from_secs(4));
    assert_eq!(timing.delay_for_attempt(3), Duration::from_secs(16));
    assert_eq!(timing.delay_for_attempt(4), Duration::from_secs(16));
}

/// Test that when a daemon socket vanishes with no daemon returning,
/// state stays reconnecting, banner stays reconnecting, stale badge stays,
/// and nothing panics or spins.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_daemon_socket_vanishes_stays_reconnecting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join(".agents-in-a-box");
    std::fs::create_dir_all(&home).expect("create home");

    let mut daemon = match DaemonProcess::spawn(&home) {
        Some(d) => d,
        None => return,
    };
    let (socket, token) = wait_for_daemon_ready(&home).await;
    let client = DaemonClient::with_parts(socket, token);

    let sub = client.reconnecting_fleet_subscription(0);
    let mut state_rx = sub.state();

    wait_for_condition(&mut state_rx, Duration::from_secs(5), |s| s.is_connected()).await;

    // Kill daemon and delete socket
    daemon.kill_sigkill();

    // Verify it transitions to reconnecting
    let rec_state = wait_for_condition(&mut state_rx, Duration::from_secs(5), |s| {
        s.is_reconnecting()
    })
    .await;

    assert!(rec_state.is_reconnecting());
    assert_eq!(rec_state.renderer_view().banner, Some("reconnecting"));
    assert!(rec_state.sections_stale_and_frozen());
    assert!(rec_state.renderer_view().stale_badge);
    assert!(rec_state.renderer_view().frozen);

    // Let it stay reconnecting for 2 seconds
    tokio::time::sleep(Duration::from_secs(2)).await;

    let current = state_rx.borrow().clone();
    assert!(current.is_reconnecting());
    assert_eq!(current.renderer_view().banner, Some("reconnecting"));
    assert!(current.sections_stale_and_frozen());

    sub.close().await;
}

/// Criterion 2 proof: kill a real daemon mid-subscription, verify 1s/4s/16s backoff delays
/// and state transitions, banner and stale badge, restart daemon, verify backoff reset and
/// contiguous replay on resync.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_daemon_sigkill_reconnect_delays_and_resync() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join(".agents-in-a-box");
    std::fs::create_dir_all(&home).expect("create home");

    let mut daemon = match DaemonProcess::spawn(&home) {
        Some(d) => d,
        None => return,
    };
    let (socket, token) = wait_for_daemon_ready(&home).await;
    let client = DaemonClient::with_parts(socket.clone(), token);

    let mut sub = client.reconnecting_fleet_subscription(0);
    let mut state_rx = sub.state();

    // 1. Initial connection (bootstrap yields ResyncRequired)
    wait_for_condition(&mut state_rx, Duration::from_secs(5), |s| s.is_connected()).await;
    assert!(state_rx.borrow().is_connected());
    let init_ev = tokio::time::timeout(Duration::from_secs(5), sub.next_event())
        .await
        .expect("initial next_event timeout")
        .expect("initial next_event");
    assert_eq!(init_ev, FleetStreamEvent::ResyncRequired);

    // 2. Kill daemon mid-stream
    daemon.kill_sigkill();

    // Query max revision while daemon is down and set covered revision
    let db_path = home.join("hangar.db");
    let init_sql = "INSERT INTO fleet_session (session_key, discovered_at, last_observed_at) \
         VALUES ('sess-resync-test', 1000, 1000) \
         ON CONFLICT DO NOTHING;\n\
         INSERT INTO fleet_event (event_id, session_key, observed_at, authority, event_type, payload, session_version, applied) \
         VALUES ('ev-baseline', 'sess-resync-test', 1000, 'authoritative', 'Heartbeat', '{}', 1, 1) \
         ON CONFLICT DO NOTHING;";
    let init_res = Command::new("sqlite3")
        .arg(&db_path)
        .arg(init_sql)
        .output()
        .expect("init baseline fleet_event");
    assert!(init_res.status.success(), "init sqlite3 failed: {:?}", init_res);

    let output = Command::new("sqlite3")
        .arg(&db_path)
        .arg("SELECT COALESCE(MAX(revision), 0) FROM fleet_event;")
        .output()
        .expect("query max revision from sqlite3");
    assert!(output.status.success(), "sqlite3 query failed");
    let covered: i64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("parse covered revision");
    assert!(covered >= 1, "covered revision must be >= 1");
    sub.set_after_revision(covered);

    // Seed fleet revisions while daemon is down
    let seed_sql = format!(
        "INSERT INTO fleet_event (revision, event_id, session_key, observed_at, authority, event_type, payload, session_version, applied) \
         VALUES ({rev1}, 'ev-seed-1', 'sess-resync-test', 1001, 'authoritative', 'Heartbeat', '{{}}', 1, 1),\
                ({rev2}, 'ev-seed-2', 'sess-resync-test', 1002, 'authoritative', 'Heartbeat', '{{}}', 2, 1);",
        rev1 = covered + 1,
        rev2 = covered + 2,
    );
    let seed_res = Command::new("sqlite3")
        .arg(&db_path)
        .arg(&seed_sql)
        .output()
        .expect("seed fleet_event into sqlite3");
    assert!(seed_res.status.success(), "seed sqlite3 failed: {:?}", seed_res);

    // 3. Observe 1st reconnect attempt (1s backoff)
    let s1 = wait_for_condition(&mut state_rx, Duration::from_secs(5), |s| {
        matches!(s1_attempt(s), Some(1))
    })
    .await;

    let delay1 = match s1 {
        ConnectionState::Reconnecting { delay, .. } => delay,
        _ => unreachable!(),
    };
    assert_eq!(delay1, BACKOFF_1S);
    let t1 = Instant::now();

    // 4. Observe 2nd reconnect attempt (4s backoff)
    let s2 = wait_for_condition(&mut state_rx, Duration::from_secs(5), |s| {
        matches!(s1_attempt(s), Some(2))
    })
    .await;

    let delay2 = match s2 {
        ConnectionState::Reconnecting { delay, .. } => delay,
        _ => unreachable!(),
    };
    assert_eq!(delay2, BACKOFF_4S);
    let elapsed1 = t1.elapsed();
    assert!(
        elapsed1 >= Duration::from_millis(900) && elapsed1 <= Duration::from_millis(1600),
        "elapsed between attempt 1 and 2 was {elapsed1:?}, expected 900-1600ms"
    );
    let t2 = Instant::now();

    // 5. Observe 3rd reconnect attempt (16s backoff)
    let s3 = wait_for_condition(&mut state_rx, Duration::from_secs(8), |s| {
        matches!(s1_attempt(s), Some(3))
    })
    .await;

    let delay3 = match s3 {
        ConnectionState::Reconnecting { delay, .. } => delay,
        _ => unreachable!(),
    };
    assert_eq!(delay3, BACKOFF_16S);
    let elapsed2 = t2.elapsed();
    assert!(
        elapsed2 >= Duration::from_millis(3600) && elapsed2 <= Duration::from_millis(5200),
        "elapsed between attempt 2 and 3 was {elapsed2:?}, expected 3600-5200ms"
    );

    // Verify banner and stale badge during reconnect
    assert_eq!(s3.renderer_view().banner, Some("reconnecting"));
    assert!(s3.sections_stale_and_frozen());
    let view = s3.renderer_view();
    assert_eq!(view.banner, Some("reconnecting"));
    assert!(view.stale_badge);
    assert!(view.frozen);

    let t3 = Instant::now();

    // Observe 4th reconnect attempt (16s backoff leg)
    let s4 = wait_for_condition(&mut state_rx, Duration::from_secs(22), |s| {
        matches!(s1_attempt(s), Some(4))
    })
    .await;

    let delay4 = match s4 {
        ConnectionState::Reconnecting { delay, .. } => delay,
        _ => unreachable!(),
    };
    assert_eq!(delay4, BACKOFF_16S);
    let elapsed3 = t3.elapsed();
    assert!(
        elapsed3 >= Duration::from_millis(14_000) && elapsed3 <= Duration::from_millis(20_000),
        "elapsed for 16s leg was {elapsed3:?}, expected 14-20s"
    );

    // 6. Restart daemon in same home
    let _daemon2 = match DaemonProcess::spawn(&home) {
        Some(d) => d,
        None => return,
    };

    // 7. Wait for attempt 3 to redial, hello to succeed, backoff to reset, and state to reconnect
    let reconnected =
        wait_for_condition(&mut state_rx, Duration::from_secs(25), |s| s.is_connected()).await;

    assert!(reconnected.is_connected());
    assert_eq!(reconnected.renderer_view().banner, None);
    assert!(!reconnected.sections_stale_and_frozen());

    // 8. Assert events received after reconnect start at covered plus one with no hole
    let ev1 = tokio::time::timeout(Duration::from_secs(5), sub.next_event())
        .await
        .expect("timed out waiting for ev1")
        .expect("next_event ev1");
    let r1 = match ev1 {
        FleetStreamEvent::Revision(e) => {
            assert_eq!(e.revision, covered + 1);
            assert_eq!(e.event_id, "ev-seed-1");
            e.revision
        }
        other => panic!("expected Revision ev-seed-1, got {other:?}"),
    };

    let ev2 = tokio::time::timeout(Duration::from_secs(5), sub.next_event())
        .await
        .expect("timed out waiting for ev2")
        .expect("next_event ev2");
    let r2 = match ev2 {
        FleetStreamEvent::Revision(e) => {
            assert_eq!(e.revision, covered + 2);
            assert_eq!(e.event_id, "ev-seed-2");
            e.revision
        }
        other => panic!("expected Revision ev-seed-2, got {other:?}"),
    };

    assert_eq!(r2, r1 + 1, "revisions must be contiguous with no hole");

    sub.close().await;
}

fn s1_attempt(state: &ConnectionState) -> Option<u32> {
    match state {
        ConnectionState::Reconnecting { attempt, .. } => Some(*attempt),
        _ => None,
    }
}
