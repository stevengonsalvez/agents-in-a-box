//! The P6e resolver's guarantees, each against a daemon that misbehaves in
//! one specific way: the kill switch (criterion 9), bounded blocking and no
//! nested lock (criterion 10), and a delete through the daemon that the next
//! reconcile pass does not bring back.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use chrono::Utc;
use uuid::Uuid;

use ainb::cli::util::{self, SESSION_RPC_DEADLINE, SessionSource};
use ainb::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use ainb::models::session::SessionAgentType;
use ainb_hangar_store::repo::sessions::SessionsRepo;

#[path = "support/fake_session_daemon.rs"]
mod fake_session_daemon;
#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;
use fake_session_daemon::{ACCEPTED, UPSERT_DELAY_MS, UPSERTS, fake_daemon, ready_list, upsert_ok};
use fleet_hangar::{EnvGuard, FleetHangar};

/// `AINB_HOME`, `AINB_HANGAR_HOME` and `AINB_SESSION_SOURCE` are process-wide.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn make_session(name: &str) -> SessionMetadata {
    SessionMetadata {
        session_id: Uuid::new_v4(),
        tmux_session_name: name.to_string(),
        worktree_path: PathBuf::from(format!("/tmp/work/{name}")),
        workspace_name: "ws".to_string(),
        created_at: Utc::now(),
        agent_type: SessionAgentType::Claude,
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        model_source: ModelSource::LegacyTyped,
        codex_model: None,
        codex_thread_id: None,
    }
}

/// An isolated `AINB_HOME` and hangar home, both named in the environment.
struct Homes {
    _root: tempfile::TempDir,
    ainb: PathBuf,
    hangar: PathBuf,
    _env: Vec<EnvGuard>,
}

impl Homes {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let ainb = root.path().join("home");
        let hangar = root.path().join("hangar");
        fs::create_dir_all(ainb.join(".agents-in-a-box")).unwrap();
        fs::create_dir_all(hangar.join("hangar")).unwrap();
        let env = vec![
            EnvGuard::set("AINB_HOME", &ainb),
            EnvGuard::set("AINB_HANGAR_HOME", &hangar),
        ];
        Self {
            _root: root,
            ainb,
            hangar,
            _env: env,
        }
    }

    fn sessions_json(&self) -> PathBuf {
        self.ainb.join(".agents-in-a-box").join("sessions.json")
    }

    fn write_file_store(&self, sessions: &[&SessionMetadata]) {
        let mut store = SessionStore::default();
        for s in sessions {
            store.upsert((*s).clone());
        }
        fs::write(
            self.sessions_json(),
            serde_json::to_vec_pretty(&store).unwrap(),
        )
        .unwrap();
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// Criterion 9: with `AINB_SESSION_SOURCE=file`, `resolve` answers `File`
/// and the daemon's socket accepted no connection at all; without it, the
/// same setup answers `Daemon`. Red if the variable is read after dialing or
/// not read.
#[test]
fn the_kill_switch_answers_file_before_any_dial() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let homes = Homes::new();
    util::advertise_workspace_sessions_for_tests(true);
    let rt = rt();
    fake_daemon(&rt, &homes.hangar, |_, _| Some(ready_list()));

    let forced = {
        let _switch = EnvGuard::set(util::SESSION_SOURCE_ENV, "file");
        rt.block_on(SessionSource::resolve())
    };
    assert!(matches!(forced, SessionSource::File), "{forced:?}");
    assert_eq!(
        ACCEPTED.load(Ordering::SeqCst),
        0,
        "the kill switch dialed the daemon"
    );

    let normal = rt.block_on(SessionSource::resolve());
    util::advertise_workspace_sessions_for_tests(false);
    assert!(matches!(normal, SessionSource::Daemon(_)), "{normal:?}");
    assert!(ACCEPTED.load(Ordering::SeqCst) > 0);
}

/// Criterion 10: a daemon that answers hello and the resolve probe, then
/// never answers a session RPC. `load` and `mutate` each return an error
/// within `SESSION_RPC_DEADLINE` plus 250 ms instead of hanging.
#[test]
fn a_daemon_that_stops_answering_is_an_error_within_the_deadline() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let homes = Homes::new();
    let rt = rt();
    let socket = fake_daemon(&rt, &homes.hangar, |_, n| (n == 0).then(ready_list));
    let source = rt.block_on(SessionSource::resolve_at(socket, "t".to_string()));
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");

    // Each call runs under the test's own timeout, so a call that hangs
    // fails here instead of hanging CI.
    let bound = SESSION_RPC_DEADLINE + Duration::from_millis(250);
    let guard = bound + Duration::from_secs(2);
    let started = Instant::now();
    let err = rt
        .block_on(async { tokio::time::timeout(guard, source.load()).await })
        .expect("load hung past the deadline")
        .expect_err("a hung list must fail");
    assert!(
        started.elapsed() < bound,
        "load took {:?}",
        started.elapsed()
    );
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");

    let started = Instant::now();
    let err = rt
        .block_on(async {
            tokio::time::timeout(
                guard,
                source.mutate(|s| s.upsert(make_session("sess-hung"))),
            )
            .await
        })
        .expect("mutate hung past the deadline")
        .expect_err("a hung mutate must fail");
    assert!(
        started.elapsed() < bound,
        "mutate took {:?}",
        started.elapsed()
    );
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
}

/// Criterion 10: holding `SessionStore::lock` and then reading or writing the
/// store through the resolver is an error within 1 s, never a hang on the
/// second `flock`.
#[test]
fn a_nested_lock_is_an_error_not_a_hang() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let _homes = Homes::new();
    // On a thread of its own, so a nested call that hangs on the second
    // `flock` fails the test at the channel's deadline instead of hanging CI.
    let (done, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let held = SessionStore::lock().expect("take the lock");
        let started = Instant::now();
        let write = util::mutate_session_store(|s| s.upsert(make_session("sess-nested")));
        let read = util::load_session_store().map(|_| ());
        let _ = done.send((write, read, started.elapsed()));
        drop(held);
    });
    let (write, read, took) = answer
        .recv_timeout(Duration::from_secs(5))
        .expect("a nested call hung on the lock this thread holds");
    let err = write.expect_err("a nested mutate must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock, "{err}");
    let err = read.expect_err("a nested load must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock, "{err}");
    assert!(took < Duration::from_secs(1), "{took:?}");
}

/// A session deleted through the daemon loses its file row too, so the next
/// reconcile pass has nothing to bring back (P6e, "Mixed versions").
#[test]
fn a_delete_through_the_daemon_is_not_brought_back_by_the_next_pass() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let homes = Homes::new();
    let gone = make_session("sess-gone");
    let kept = make_session("sess-kept");
    homes.write_file_store(&[&gone, &kept]);

    ainb_hangar_daemon::rpc::auth::advertise_workspace_sessions_for_tests(true);
    let hangar = FleetHangar::start(&homes.hangar);
    let path = homes.sessions_json();
    let reconcile = || {
        hangar.block_on(async {
            ainb_hangar_daemon::session_import::import_sessions_if_needed(hangar.pool(), &path)
                .await
                .unwrap();
            ainb_hangar_daemon::session_import::reconcile_sessions(hangar.pool(), &path)
                .await
                .unwrap();
        });
    };
    reconcile();

    let token = fs::read_to_string(ainb_hangar_proto::auth::token_file_in(&homes.hangar))
        .unwrap()
        .trim()
        .to_string();
    let rt = rt();
    let source = rt.block_on(SessionSource::resolve_at(
        ainb_hangar_daemon::rpc::socket_path_in(&homes.hangar),
        token,
    ));
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");
    rt.block_on(source.mutate(|s| s.remove_by_session_id(gone.session_id)))
        .expect("delete through the daemon");

    reconcile();
    let ids: Vec<String> = hangar.block_on(async {
        SessionsRepo::list(hangar.pool(), None, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.session_id)
            .collect()
    });
    assert_eq!(
        ids,
        vec![kept.session_id.to_string()],
        "the pass brought the session back"
    );
    assert!(!SessionStore::load().sessions.contains_key("sess-gone"));
}

/// The client's RPC deadline must outlast the daemon's first-pass wait: a
/// daemon that has just restarted holds a read for that long before it
/// answers not-ready, and that answer has to arrive before the client gives
/// up, or a retry reads as a timeout.
#[test]
fn the_rpc_deadline_outlasts_the_daemons_first_pass_wait() {
    assert!(
        SESSION_RPC_DEADLINE > ainb_hangar_daemon::session_import::FIRST_PASS_WAIT,
        "SESSION_RPC_DEADLINE {SESSION_RPC_DEADLINE:?} must exceed FIRST_PASS_WAIT {:?}",
        ainb_hangar_daemon::session_import::FIRST_PASS_WAIT
    );
}

/// The daemon's reconcile pass holds the `sessions.json` lock for up to its
/// flock wait plus its store write, on every boot. A writer's lock wait must
/// outlast that, or `ainb run` rolls back a live session because a daemon was
/// reconciling.
#[test]
fn the_lock_wait_outlasts_the_daemons_longest_pass() {
    let pass = ainb_hangar_daemon::session_import::SESSIONS_FLOCK_BOUND
        + ainb_hangar_daemon::session_import::RECONCILE_STORE_BOUND;
    assert!(
        util::SESSIONS_LOCK_WAIT > pass,
        "SESSIONS_LOCK_WAIT {:?} must exceed the pass's {pass:?}",
        util::SESSIONS_LOCK_WAIT
    );
}

/// A write of two sessions whose second upsert never answers fails within
/// the writes' one deadline, and the file is put back as it was.
#[test]
fn a_later_write_that_hangs_fails_the_mutate_and_restores_the_file() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let homes = Homes::new();
    homes.write_file_store(&[&make_session("sess-kept")]);
    let before = fs::read(homes.sessions_json()).unwrap();
    UPSERT_DELAY_MS.store(0, Ordering::SeqCst);
    let rt = rt();
    let socket = fake_daemon(&rt, &homes.hangar, |method, _| match method {
        "workspace/session_upsert" if UPSERTS.load(Ordering::SeqCst) >= 2 => None,
        "workspace/session_upsert" => Some(upsert_ok()),
        _ => Some(ready_list()),
    });
    let source = rt.block_on(SessionSource::resolve_at(socket, "t".to_string()));
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");

    let started = Instant::now();
    let err = rt
        .block_on(async {
            tokio::time::timeout(
                util::MUTATE_WRITES_DEADLINE + Duration::from_secs(3),
                source.mutate(|s| {
                    s.upsert(make_session("sess-one"));
                    s.upsert(make_session("sess-two"));
                }),
            )
            .await
        })
        .expect("the mutate hung")
        .expect_err("a hung second write must fail the mutate");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
    assert!(
        started.elapsed() < util::MUTATE_WRITES_DEADLINE + Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        fs::read(homes.sessions_json()).unwrap(),
        before,
        "the file was not put back"
    );
}

/// Writes that each answer inside the per-RPC deadline but together run past
/// the writes' one deadline fail at that deadline, not at the sum.
#[test]
fn a_run_of_slow_writes_hits_one_overall_deadline() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let homes = Homes::new();
    homes.write_file_store(&[&make_session("sess-kept")]);
    let before = fs::read(homes.sessions_json()).unwrap();
    // Each write takes 40 percent of the deadline: four take 160 percent.
    let each = util::MUTATE_WRITES_DEADLINE * 2 / 5;
    UPSERT_DELAY_MS.store(each.as_millis() as usize, Ordering::SeqCst);
    let rt = rt();
    let socket = fake_daemon(&rt, &homes.hangar, |method, _| match method {
        "workspace/session_upsert" => Some(upsert_ok()),
        _ => Some(ready_list()),
    });
    let source = rt.block_on(SessionSource::resolve_at(socket, "t".to_string()));

    let started = Instant::now();
    let err = rt
        .block_on(source.mutate(|s| {
            for name in ["sess-a", "sess-b", "sess-c", "sess-d"] {
                s.upsert(make_session(name));
            }
        }))
        .expect_err("four slow writes must run past the one deadline");
    UPSERT_DELAY_MS.store(0, Ordering::SeqCst);
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
    assert!(
        started.elapsed() < util::MUTATE_WRITES_DEADLINE + Duration::from_millis(500),
        "it waited for the sum, {:?}",
        started.elapsed()
    );
    assert_eq!(fs::read(homes.sessions_json()).unwrap(), before);
}

/// A `sessions.json` that does not parse is refused, never cut down to the
/// rows a write touches: the write fails, sends nothing to the table, and the
/// file's bytes are unchanged.
#[test]
fn a_corrupt_file_is_refused_not_rewritten() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let homes = Homes::new();
    fs::write(homes.sessions_json(), b"{ \"sessions\": { not json").unwrap();
    let before = fs::read(homes.sessions_json()).unwrap();
    UPSERT_DELAY_MS.store(0, Ordering::SeqCst);
    let rt = rt();
    let socket = fake_daemon(&rt, &homes.hangar, |method, _| match method {
        "workspace/session_upsert" => Some(upsert_ok()),
        _ => Some(ready_list()),
    });
    let source = rt.block_on(SessionSource::resolve_at(socket, "t".to_string()));

    let err = rt
        .block_on(source.mutate(|s| s.upsert(make_session("sess-new"))))
        .expect_err("a corrupt file must refuse the write");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
    assert_eq!(UPSERTS.load(Ordering::SeqCst), 0, "a table write was sent");
    assert_eq!(fs::read(homes.sessions_json()).unwrap(), before);
}

fn daemon_source(rt: &tokio::runtime::Runtime, homes: &Homes) -> (FleetHangar, SessionSource) {
    ainb_hangar_daemon::rpc::auth::advertise_workspace_sessions_for_tests(true);
    let hangar = FleetHangar::start(&homes.hangar);
    reconcile(&hangar, homes);
    let token = fs::read_to_string(ainb_hangar_proto::auth::token_file_in(&homes.hangar))
        .unwrap()
        .trim()
        .to_string();
    let source = rt.block_on(SessionSource::resolve_at(
        ainb_hangar_daemon::rpc::socket_path_in(&homes.hangar),
        token,
    ));
    (hangar, source)
}

/// The boot import and a reconcile pass of `homes`' file, as a daemon runs.
fn reconcile(hangar: &FleetHangar, homes: &Homes) -> usize {
    let path = homes.sessions_json();
    hangar.block_on(async {
        ainb_hangar_daemon::session_import::import_sessions_if_needed(hangar.pool(), &path)
            .await
            .unwrap();
        ainb_hangar_daemon::session_import::reconcile_sessions(hangar.pool(), &path)
            .await
            .unwrap()
            .deleted
            .len()
    })
}

fn table_names(hangar: &FleetHangar) -> Vec<String> {
    let mut names: Vec<String> = hangar.block_on(async {
        SessionsRepo::list(hangar.pool(), None, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.tmux_session_name)
            .collect()
    });
    names.sort_unstable();
    names
}

/// A delete made while degraded reaches the file only. It is not undone: the
/// next pass removes the table row, the file being the authority on which
/// sessions exist until the flip.
#[test]
fn a_delete_made_while_degraded_is_not_undone_by_the_daemon() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let homes = Homes::new();
    homes.write_file_store(&[&make_session("sess-gone"), &make_session("sess-kept")]);
    let rt = rt();
    let (hangar, _) = daemon_source(&rt, &homes);
    assert_eq!(table_names(&hangar), vec!["sess-gone", "sess-kept"]);

    let degraded = rt.block_on(SessionSource::resolve_at(
        homes.hangar.join("no-daemon.sock"),
        "t".to_string(),
    ));
    assert!(degraded.is_degraded(), "{degraded:?}");
    rt.block_on(degraded.mutate(|s| {
        s.sessions.remove("sess-gone");
    }))
    .expect("a degraded delete goes to the file");

    assert_eq!(reconcile(&hangar, &homes), 1);
    assert_eq!(table_names(&hangar), vec!["sess-kept"]);
}

/// A multi-row write whose later row the daemon refuses is reverted in the
/// file, but the earlier row already reached the table. The next pass
/// deletes that row, because the file does not have it.
#[test]
fn a_reverted_multi_row_write_leaves_nothing_after_the_next_pass() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let homes = Homes::new();
    homes.write_file_store(&[&make_session("sess-kept")]);
    let before = fs::read(homes.sessions_json()).unwrap();
    let rt = rt();
    let (hangar, source) = daemon_source(&rt, &homes);
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");

    // Written in tmux-name order: "sess-a1" lands, then the daemon refuses
    // "sess-z9" (a relative worktree path fails validation).
    let mut refused = make_session("sess-z9");
    refused.worktree_path = PathBuf::from("relative/z9");
    rt.block_on(source.mutate(|s| {
        s.upsert(make_session("sess-a1"));
        s.upsert(refused.clone());
    }))
    .expect_err("the refused row fails the write");
    assert_eq!(
        fs::read(homes.sessions_json()).unwrap(),
        before,
        "the file was not put back"
    );
    assert_eq!(
        table_names(&hangar),
        vec!["sess-a1", "sess-kept"],
        "the first row reached the table before the refusal"
    );

    assert_eq!(reconcile(&hangar, &homes), 1);
    assert_eq!(table_names(&hangar), vec!["sess-kept"]);
}

/// The file and degraded sources refuse a corrupt `sessions.json` too, rather
/// than save the empty store `SessionStore::load` reads it as.
#[test]
fn a_corrupt_file_is_refused_on_the_file_and_degraded_paths() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let homes = Homes::new();
    fs::write(homes.sessions_json(), b"{ \"sessions\": { not json").unwrap();
    let before = fs::read(homes.sessions_json()).unwrap();
    let rt = rt();

    for source in [SessionSource::File, SessionSource::Degraded(None)] {
        let err = rt
            .block_on(source.mutate(|s| s.upsert(make_session("sess-new"))))
            .expect_err("a corrupt file must refuse the write");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::InvalidData,
            "{source:?}: {err}"
        );
        assert_eq!(
            fs::read(homes.sessions_json()).unwrap(),
            before,
            "{source:?}"
        );
    }
}
