//! The sidecar supervisor against the real `ainb-hangar-daemon` binary, each
//! test in its own hangar home.
//!
//! The binary comes from `AINB_DESKTOP_DAEMON_BIN`, else the parent workspace's
//! `target/debug/ainb-hangar-daemon` (`cargo build -p ainb-hangar-daemon`).
//! Every daemon a test starts is killed by its exact pid when the test ends;
//! nothing is matched by name.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ainb_desktop::sidecar::{Sidecar, SidecarConfig, SidecarState, daemon_pid};
use ainb_hangar_client::DaemonClient;
use ainb_hangar_proto::connections::SurfaceKind;
use tokio::sync::watch;

/// A cold runner needs time to migrate a fresh store and mint the token.
const BOOT_BUDGET: Duration = Duration::from_secs(90);

fn daemon_bin() -> PathBuf {
    let bin = std::env::var_os("AINB_DESKTOP_DAEMON_BIN").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ainb-hangar-daemon"),
        PathBuf::from,
    );
    assert!(
        bin.is_file(),
        "no daemon binary at {}: run `cargo build -p ainb-hangar-daemon` in ainb-tui \
         or set AINB_DESKTOP_DAEMON_BIN",
        bin.display()
    );
    bin
}

/// A private hangar home, and the daemons started in it killed on drop.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        // The codex manager's boot reaper signals processes outside the home;
        // keep it out of every daemon these tests start.
        std::env::set_var("AINB_CODEX_MANAGED", "0");
        Self {
            dir: tempfile::tempdir().expect("scratch home"),
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join(".agents-in-a-box")
    }

    fn config(&self) -> SidecarConfig {
        let mut config = SidecarConfig::new(self.home(), daemon_bin());
        config.hello_budget = BOOT_BUDGET;
        config
    }

    async fn desktop_rows(&self) -> usize {
        let token = std::fs::read_to_string(ainb_hangar_proto::auth::token_file_in(&self.home()))
            .expect("daemon token");
        let client = DaemonClient::with_parts(
            ainb_hangar_client::socket_path_in(&self.home()),
            token.trim().to_string(),
        );
        client
            .connections_list()
            .await
            .expect("connections_list")
            .connections
            .iter()
            .filter(|row| row.surface.kind == SurfaceKind::Desktop)
            .count()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(pid) = daemon_pid(&self.home()) {
            kill(pid, nix::sys::signal::Signal::SIGKILL);
        }
    }
}

fn kill(pid: u32, signal: nix::sys::signal::Signal) {
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).expect("pid")),
        signal,
    );
}

fn alive(pid: u32) -> bool {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).expect("pid")),
        None,
    )
    .is_ok()
}

async fn wait_for(
    state: &mut watch::Receiver<SidecarState>,
    what: &str,
    matches: impl Fn(&SidecarState) -> bool,
) -> SidecarState {
    tokio::time::timeout(BOOT_BUDGET, async {
        loop {
            let current = state.borrow_and_update().clone();
            if matches(&current) {
                return current;
            }
            state.changed().await.expect("supervisor running");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("never {what}; last state {:?}", *state.borrow()))
}

async fn eventually<F: std::future::Future<Output = bool>>(what: &str, probe: impl Fn() -> F) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !probe().await {
        assert!(tokio::time::Instant::now() < deadline, "never {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn connected(state: &SidecarState) -> bool {
    matches!(state, SidecarState::Connected { .. })
}

/// A cold start spawns the daemon and lists one desktop row. Closing the app
/// removes the row and leaves the daemon running.
#[tokio::test(flavor = "multi_thread")]
async fn a_cold_start_spawns_the_daemon_which_outlives_the_app() {
    let world = World::new();
    let sidecar = Sidecar::start(world.config());
    let mut state = sidecar.state();

    let SidecarState::Connected {
        daemon_pid: Some(pid),
        spawned,
    } = wait_for(&mut state, "connected", connected).await
    else {
        panic!("connected without a daemon pid");
    };
    assert!(spawned, "nothing owned the home, so this start spawned it");
    assert_eq!(
        world.desktop_rows().await,
        1,
        "one desktop row while the app runs"
    );

    drop(sidecar);

    eventually("the desktop row went", || async {
        world.desktop_rows().await == 0
    })
    .await;
    assert!(alive(pid), "the daemon survives the app closing");
}

/// Two hosts starting on one cold home end on one daemon: the flock picks the
/// winner and the other attaches to it.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_host_attaches_to_the_winner_instead_of_spawning_another() {
    let world = World::new();
    let first = Sidecar::start(world.config());
    let second = Sidecar::start(world.config());

    let (mut a, mut b) = (first.state(), second.state());
    let a = wait_for(&mut a, "first connected", connected).await;
    let b = wait_for(&mut b, "second connected", connected).await;

    let (
        SidecarState::Connected {
            daemon_pid: pid_a,
            spawned: spawned_a,
        },
        SidecarState::Connected {
            daemon_pid: pid_b,
            spawned: spawned_b,
        },
    ) = (a, b)
    else {
        unreachable!("both matched connected");
    };
    assert_eq!(pid_a, pid_b, "both hosts are on the same daemon");
    assert_eq!(
        u8::from(spawned_a) + u8::from(spawned_b),
        1,
        "exactly one host's child became the daemon"
    );
}

/// A daemon killed under the app leaves it reconnecting, and it comes back on
/// a fresh daemon.
#[tokio::test(flavor = "multi_thread")]
async fn a_killed_daemon_leaves_the_app_reconnecting_then_connected_again() {
    let world = World::new();
    let sidecar = Sidecar::start(world.config());
    let mut state = sidecar.state();
    let SidecarState::Connected {
        daemon_pid: Some(first),
        ..
    } = wait_for(&mut state, "connected", connected).await
    else {
        panic!("connected without a daemon pid");
    };

    kill(first, nix::sys::signal::Signal::SIGKILL);

    wait_for(&mut state, "reconnecting", |state| {
        matches!(state, SidecarState::Reconnecting { .. })
    })
    .await;
    let SidecarState::Connected {
        daemon_pid: Some(second),
        ..
    } = wait_for(&mut state, "connected again", connected).await
    else {
        panic!("reconnected without a daemon pid");
    };
    assert_ne!(first, second, "a fresh daemon took the home");
    assert_eq!(world.desktop_rows().await, 1);
}

/// A daemon binary that crashes on every start leaves the app degraded after
/// the retries, naming the log.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_that_keeps_crashing_leaves_the_app_degraded() {
    let world = World::new();
    let mut config = world.config();
    config.daemon_bin = PathBuf::from("/bin/false");
    let sidecar = Sidecar::start(config.clone());
    let mut state = sidecar.state();

    let SidecarState::Degraded { error, log } = wait_for(&mut state, "degraded", |state| {
        matches!(state, SidecarState::Degraded { .. })
    })
    .await
    else {
        unreachable!("matched degraded");
    };
    assert!(error.contains("attempt 3 of 3"), "{error}");
    assert_eq!(log, config.log_path());
    assert!(log.is_file(), "the log a user is sent to exists");
}
