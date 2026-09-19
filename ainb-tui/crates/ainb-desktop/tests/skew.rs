//! The desktop leg of the skew harness (spec D17, goal D4a): the sidecar
//! supervisor against daemon N-1's committed frames and against a daemon
//! whose range does not meet this build's, with no daemon binary ever run.
//!
//! ```text
//!                         daemon N-1 ({} ack)      daemon refusing (-32007)
//!  Sidecar                Connected, no version    Incompatible, no spawn
//! ```
//!
//! "daemon N-1" is the daemon crate's fixture file, included so the N-1
//! contract has one copy; "a daemon from the future" is a listener answering
//! exactly what `rpc/auth.rs` answers, with a range above this build's.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ainb_desktop::sidecar::{Sidecar, SidecarConfig, SidecarState};
use ainb_hangar_proto::protocol::ProtocolRange;
use tokio::sync::watch;

mod skew_support;
use skew_support::{Hello, listen, recording_daemon};

/// A private hangar home with a "daemon binary" that records whether it ran.
struct World {
    dir: tempfile::TempDir,
    spawned: PathBuf,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("scratch home");
        let spawned = dir.path().join("spawned");
        Self { dir, spawned }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join(".agents-in-a-box")
    }

    fn config(&self) -> SidecarConfig {
        let mut config =
            SidecarConfig::new(self.home(), recording_daemon(self.dir.path(), &self.spawned));
        config.grace = Duration::from_millis(300);
        config.hello_budget = Duration::from_secs(2);
        config
    }

    fn daemon_was_run(&self) -> bool {
        self.spawned.is_file()
    }
}

async fn wait_for(
    state: &mut watch::Receiver<SidecarState>,
    what: &str,
    matches: impl Fn(&SidecarState) -> bool,
) -> SidecarState {
    tokio::time::timeout(Duration::from_secs(20), async {
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

fn future_daemon() -> Hello {
    Hello::Refuse {
        protocol: ProtocolRange { min: 5, max: 6 },
        daemon_version: Some("9.9.9".into()),
    }
}

/// Daemon N-1 answers a bare `{}`: the sidecar attaches, and Connected says
/// so honestly, with no version and the legacy range, and nothing is spawned.
#[tokio::test(flavor = "multi_thread")]
async fn daemon_n_minus_1_frames_connect_with_no_version_and_the_legacy_range() {
    let world = World::new();
    let _daemon = listen(&world.home(), Hello::NMinusOne);
    let sidecar = Sidecar::start(world.config());
    let mut state = sidecar.state();

    let SidecarState::Connected {
        spawned,
        daemon_version,
        protocol,
        ..
    } = wait_for(&mut state, "connected", |state| {
        matches!(state, SidecarState::Connected { .. })
    })
    .await
    else {
        unreachable!("matched connected");
    };
    assert!(!spawned, "the listener owned the home");
    assert_eq!(daemon_version, None, "a daemon that cannot say its version");
    assert_eq!(protocol, ProtocolRange::legacy());
    assert!(!world.daemon_was_run(), "a daemon answered, so none was started");

    let view = serde_json::to_value(state.borrow().view()).expect("view serialises");
    assert_eq!(view["state"], "connected");
    assert_eq!(view["daemon_version"], serde_json::Value::Null);
    assert_eq!(view["protocol"]["min"], 1);
    assert_eq!(view["protocol"]["max"], 1);
}

/// A daemon from the future refuses on the first frame: the state reaches
/// Incompatible within one backoff step, the daemon binary never runs, and
/// the app knows it is the older side.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_from_the_future_is_incompatible_at_once_and_nothing_is_spawned() {
    let world = World::new();
    let _daemon = listen(&world.home(), future_daemon());
    let config = world.config();
    let first_backoff = config.reconnect_backoff[0];
    let started = Instant::now();
    let sidecar = Sidecar::start(config);
    let mut state = sidecar.state();

    let SidecarState::Incompatible {
        message,
        daemon_is_newer,
    } = wait_for(&mut state, "incompatible", |state| {
        matches!(state, SidecarState::Incompatible { .. })
    })
    .await
    else {
        unreachable!("matched incompatible");
    };
    assert!(
        started.elapsed() < first_backoff,
        "read on the first frame, not after {:?}",
        started.elapsed()
    );
    assert!(daemon_is_newer, "5-6 sits above {:?}", ProtocolRange::supported());
    assert!(
        message.contains("daemon protocol 5-6") && message.contains("restart from the newer binary"),
        "the daemon's own sentence, verbatim: {message}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!world.daemon_was_run(), "the daemon binary must never run on a refusal");
}

/// A daemon from the past refuses the same way, and the app knows it is the
/// newer side: the daemon is the binary to move.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_from_the_past_is_incompatible_and_the_app_is_the_newer_side() {
    let world = World::new();
    let _daemon = listen(
        &world.home(),
        Hello::Refuse {
            protocol: ProtocolRange { min: 0, max: 0 },
            daemon_version: Some("0.0.1".into()),
        },
    );
    let sidecar = Sidecar::start(world.config());
    let mut state = sidecar.state();

    let SidecarState::Incompatible {
        daemon_is_newer, ..
    } = wait_for(&mut state, "incompatible", |state| {
        matches!(state, SidecarState::Incompatible { .. })
    })
    .await
    else {
        unreachable!("matched incompatible");
    };
    assert!(!daemon_is_newer, "0-0 sits below {:?}", ProtocolRange::supported());
    assert!(!world.daemon_was_run());
}

/// Once the operator has moved the daemon, Retry probes again and attaches to
/// whatever now owns the home; nothing happened in between on its own.
#[tokio::test(flavor = "multi_thread")]
async fn retry_after_the_operator_moved_the_daemon_attaches_to_the_new_owner() {
    let world = World::new();
    let refusing = listen(&world.home(), future_daemon());
    let sidecar = Sidecar::start(world.config());
    let mut state = sidecar.state();
    wait_for(&mut state, "incompatible", |state| {
        matches!(state, SidecarState::Incompatible { .. })
    })
    .await;

    drop(refusing);
    let _replacement = listen(&world.home(), Hello::NMinusOne);
    sidecar.retry();

    let SidecarState::Connected { daemon_version, .. } =
        wait_for(&mut state, "connected after retry", |state| {
            matches!(state, SidecarState::Connected { .. })
        })
        .await
    else {
        unreachable!("matched connected");
    };
    assert_eq!(daemon_version, None);
    assert!(!world.daemon_was_run());
}
