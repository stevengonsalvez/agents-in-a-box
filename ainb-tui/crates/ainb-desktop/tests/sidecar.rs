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
use ainb_hangar_proto::protocol::ProtocolRange;
use tokio::sync::watch;

mod skew_support;

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
    config.daemon_bin = PathBuf::from("false");
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

    // What the webview is told names no path and no pid, and offers the log.
    let view = serde_json::to_value(state.borrow().view()).expect("view serialises");
    assert_eq!(view["state"], "degraded");
    assert_eq!(view["has_log"], true);
    let text = view.to_string();
    assert!(!text.contains('/'), "a path reached the webview: {text}");
    assert!(ainb_desktop::sidecar::log_tail(&config, 1024).is_some());
}

/// A daemon lost more times in a row than the backoff has steps leaves the app
/// degraded, and Retry from there connects again.
#[tokio::test(flavor = "multi_thread")]
async fn repeated_losses_past_the_backoff_leave_the_app_degraded_until_retry() {
    let world = World::new();
    let mut config = world.config();
    config.reconnect_backoff = vec![Duration::from_millis(50); 2];
    let sidecar = Sidecar::start(config);
    let mut state = sidecar.state();

    for loss in 1..=3 {
        let SidecarState::Connected {
            daemon_pid: Some(pid),
            ..
        } = wait_for(&mut state, "connected", connected).await
        else {
            panic!("connected without a daemon pid before loss {loss}");
        };
        kill(pid, nix::sys::signal::Signal::SIGKILL);
        if loss < 3 {
            wait_for(&mut state, "reconnecting", |state| {
                matches!(state, SidecarState::Reconnecting { .. })
            })
            .await;
        }
    }
    let SidecarState::Degraded { error, .. } = wait_for(&mut state, "degraded", |state| {
        matches!(state, SidecarState::Degraded { .. })
    })
    .await
    else {
        unreachable!("matched degraded");
    };
    assert!(error.contains("lost 3 times"), "{error}");

    sidecar.retry();
    wait_for(&mut state, "connected after retry", connected).await;
}

#[test]
fn scrub_paths_hides_every_path_and_keeps_the_words() {
    use ainb_desktop::sidecar::scrub_paths;
    assert_eq!(
        scrub_paths(
            "no daemon answered on /home/me/.agents-in-a-box/hangar.sock within 60s: refused"
        ),
        "no daemon answered on <path> within 60s: refused"
    );
    assert_eq!(
        scrub_paths("could not start the bundled daemon ~/bin/ainb-hangar-daemon: missing"),
        "could not start the bundled daemon <path>: missing"
    );
    assert_eq!(
        scrub_paths("the daemon exited with exit status: 1"),
        "the daemon exited with exit status: 1"
    );
}

/// A fixture "daemon" that stays up without ever answering hello. With
/// `own_lock` it first writes its pid to the home's ownership lock, as the real
/// daemon's flock does, and `exec`s so the child pid is the one it wrote.
fn slow_daemon(world: &World, own_lock: bool) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let script = world.dir.path().join(if own_lock { "owner.sh" } else { "stray.sh" });
    let lock = if own_lock {
        "mkdir -p \"$AINB_HANGAR_HOME/hangar\" && echo $$ > \"$AINB_HANGAR_HOME/hangar/daemon.lock\"\n"
    } else {
        ""
    };
    std::fs::write(&script, format!("#!/bin/sh\n{lock}exec sleep 120\n")).expect("fixture");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    script
}

/// A daemon that holds the home's lock but is slower to answer than the
/// budget is still the only daemon for that home: the app degrades and the
/// daemon lives on for Retry to attach to.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_booting_daemon_that_owns_the_home_is_never_killed() {
    let world = World::new();
    let mut config = world.config();
    config.daemon_bin = slow_daemon(&world, true);
    config.grace = Duration::from_millis(300);
    config.hello_budget = Duration::from_secs(1);
    let sidecar = Sidecar::start(config);
    let mut state = sidecar.state();

    let SidecarState::Degraded { error, .. } = wait_for(&mut state, "degraded", |state| {
        matches!(state, SidecarState::Degraded { .. })
    })
    .await
    else {
        unreachable!("matched degraded");
    };
    assert!(error.contains("still starting"), "{error}");
    let owner = daemon_pid(&world.home()).expect("the fixture wrote its lock");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(alive(owner), "the booting owner of the home was not killed");
}

/// A child that never took the lock and never answers is stopped, on every
/// attempt, rather than left running.
#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_never_owned_the_home_is_stopped() {
    let world = World::new();
    let mut config = world.config();
    config.daemon_bin = slow_daemon(&world, false);
    config.grace = Duration::from_millis(300);
    config.hello_budget = Duration::from_secs(1);
    config.max_spawn_attempts = 2;
    let sidecar = Sidecar::start(config);
    let mut state = sidecar.state();

    let SidecarState::Degraded { error, .. } = wait_for(&mut state, "degraded", |state| {
        matches!(state, SidecarState::Degraded { .. })
    })
    .await
    else {
        unreachable!("matched degraded");
    };
    assert!(error.contains("attempt 2"), "{error}");
    let script = world.dir.path().join("stray.sh");
    let strays = std::process::Command::new("pgrep")
        .args(["-f", &script.display().to_string()])
        .output()
        .expect("pgrep");
    assert!(
        String::from_utf8_lossy(&strays.stdout).trim().is_empty(),
        "a child that never owned the home is still running"
    );
}

/// A daemon that owns the home and refuses this build's protocol range is
/// answered today by a spawn: the child loses the flock and exits 0, the
/// supervisor polls hello for the whole budget against a daemon that keeps
/// refusing, and the app degrades with "no daemon answered", which is the
/// wrong sentence for a daemon that answered every time. Pinned as it stands
/// so the fix is a visible diff.
#[tokio::test(flavor = "multi_thread")]
async fn a_refusing_daemon_is_answered_by_a_spawn_then_degraded() {
    let world = World::new();
    let _daemon = skew_support::listen(
        &world.home(),
        skew_support::Hello::Refuse {
            protocol: ProtocolRange { min: 5, max: 6 },
            daemon_version: Some("9.9.9".into()),
        },
    );
    let spawned = world.dir.path().join("spawned");
    let mut config = world.config();
    config.daemon_bin = skew_support::recording_daemon(world.dir.path(), &spawned);
    config.grace = Duration::from_millis(300);
    config.hello_budget = Duration::from_secs(1);
    let sidecar = Sidecar::start(config);
    let mut state = sidecar.state();

    let SidecarState::Degraded { error, .. } = wait_for(&mut state, "degraded", |state| {
        matches!(state, SidecarState::Degraded { .. })
    })
    .await
    else {
        unreachable!("matched degraded");
    };
    assert!(error.contains("no daemon answered"), "{error}");
    assert!(
        spawned.is_file(),
        "the bundled daemon was spawned against a daemon that answered"
    );
}
