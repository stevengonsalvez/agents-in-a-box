//! The P6e resolver's guarantees, each against a daemon that misbehaves in
//! one specific way: the kill switch (criterion 9), bounded blocking and no
//! nested lock (criterion 10), and a delete through the daemon that the next
//! reconcile pass does not bring back.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use uuid::Uuid;

use ainb::cli::util::{self, SESSION_RPC_DEADLINE, SessionSource};
use ainb::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use ainb::models::session::SessionAgentType;
use ainb_hangar_proto::protocol::CAP_WORKSPACE_SESSIONS;
use ainb_hangar_store::repo::sessions::SessionsRepo;

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;
use fleet_hangar::{EnvGuard, FleetHangar};

/// `AINB_HOME`, `AINB_HANGAR_HOME` and `AINB_SESSION_SOURCE` are process-wide.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Connections the fake daemon has accepted.
static ACCEPTED: AtomicUsize = AtomicUsize::new(0);
/// Session requests (past hello) the fake daemon has read.
static REQUESTS: AtomicUsize = AtomicUsize::new(0);
/// Upserts the fake daemon has been sent.
static UPSERTS: AtomicUsize = AtomicUsize::new(0);
/// How long the fake daemon takes over each upsert it answers, in ms.
static UPSERT_DELAY_MS: AtomicUsize = AtomicUsize::new(0);

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

/// A fake daemon on `home`'s plain socket, with a token file, as
/// `DaemonClient::from_env` finds it. It answers hello with the sessions
/// capability, then each session request with `reply(method, n)`, where `n`
/// counts session requests across connections; `None` never answers.
fn fake_daemon(
    rt: &tokio::runtime::Runtime,
    home: &Path,
    reply: fn(&str, usize) -> Option<serde_json::Value>,
) -> PathBuf {
    ACCEPTED.store(0, Ordering::SeqCst);
    REQUESTS.store(0, Ordering::SeqCst);
    UPSERTS.store(0, Ordering::SeqCst);
    fs::write(ainb_hangar_proto::auth::token_file_in(home), "t\n").unwrap();
    let socket = home.join("hangar.sock");
    let listener = rt.block_on(async { UnixListener::bind(&socket) }).unwrap();
    rt.spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            ACCEPTED.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let (read_half, mut writer) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let Some(hello) = read_frame(&mut reader).await else {
                    return;
                };
                let caps = serde_json::json!({ "capabilities": [CAP_WORKSPACE_SESSIONS] });
                write_frame(
                    &mut writer,
                    &serde_json::json!({"jsonrpc": "2.0", "id": hello["id"], "result": caps}),
                )
                .await;
                let Some(req) = read_frame(&mut reader).await else {
                    return;
                };
                let n = REQUESTS.fetch_add(1, Ordering::SeqCst);
                let method = req["method"].as_str().unwrap_or_default();
                if method == "workspace/session_upsert" {
                    UPSERTS.fetch_add(1, Ordering::SeqCst);
                    let delay = UPSERT_DELAY_MS.load(Ordering::SeqCst) as u64;
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                let Some(mut resp) = reply(method, n) else {
                    // Never answer: hold the connection open.
                    let _ = read_frame(&mut reader).await;
                    std::future::pending::<()>().await;
                    return;
                };
                resp["jsonrpc"] = "2.0".into();
                resp["id"] = req["id"].clone();
                write_frame(&mut writer, &resp).await;
            });
        }
    });
    socket
}

async fn read_frame(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> Option<serde_json::Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(n) = line.strip_prefix("Content-Length: ") {
            len = n.parse::<usize>().ok();
        }
    }
    let mut body = vec![0; len?];
    reader.read_exact(&mut body).await.ok()?;
    serde_json::from_slice(&body).ok()
}

async fn write_frame(writer: &mut tokio::net::unix::OwnedWriteHalf, value: &serde_json::Value) {
    let body = serde_json::to_vec(value).unwrap();
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    let _ = writer.write_all(&frame).await;
    let _ = writer.flush().await;
}

fn ready_list() -> serde_json::Value {
    serde_json::json!({ "result": { "sessions": [], "truncated": false, "import_complete": true } })
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

fn upsert_ok() -> serde_json::Value {
    serde_json::json!({ "result": { "ok": true } })
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
