//! The CLI's session store against a real daemon, a misbehaving one, and none
//! (P6d).
//!
//! ```text
//! resolve once ──▶ capability + import marker ──▶ Daemon ──▶ table only
//!              └─▶ anything else ────────────────▶ File   ──▶ sessions.json
//! ```
//!
//! P6d lands dark: no production daemon advertises the capability, so the
//! first two tests pin that every CLI process stays on the file. The rest
//! switch the capability on through the daemon's test-only switch.
//!
//! Every daemon test here drives the real RPC handlers through a real socket
//! (`FleetHangar`), so deleting a handler fails them. The file is checked byte
//! for byte after every daemon-mode write: the table is authoritative and the
//! file is never rewritten from it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use uuid::Uuid;

use ainb::cli::util::SessionSource;
use ainb::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use ainb::models::session::SessionAgentType;
use ainb_hangar_proto::protocol::CAP_WORKSPACE_SESSIONS;
use ainb_hangar_store::repo::sessions::{SessionRow, SessionsRepo};

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;

use fleet_hangar::{EnvGuard, FleetHangar};

/// `SessionStore`'s file path comes from `AINB_HOME`, which is process-wide,
/// so the tests in this binary take turns.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn make_session(name: &str, ws: &str) -> SessionMetadata {
    SessionMetadata {
        session_id: Uuid::new_v4(),
        tmux_session_name: name.to_string(),
        worktree_path: PathBuf::from(format!("/tmp/work/{ws}")),
        workspace_name: ws.to_string(),
        created_at: Utc::now(),
        agent_type: SessionAgentType::Codex,
        headroom_enabled: false,
        rtk_enabled: true,
        skip_permissions: Some(true),
        model: Some("gpt-5-codex".to_string()),
        model_source: ModelSource::Raw,
        codex_model: None,
        codex_thread_id: None,
    }
}

/// An isolated `AINB_HOME` (holding `sessions.json`) and hangar home.
struct Homes {
    _root: tempfile::TempDir,
    ainb: PathBuf,
    hangar: PathBuf,
    _ainb_home: EnvGuard,
}

impl Homes {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("create root");
        let ainb = root.path().join("home");
        let hangar = root.path().join("hangar");
        fs::create_dir_all(ainb.join(".agents-in-a-box")).expect("create ainb home");
        fs::create_dir_all(&hangar).expect("create hangar home");
        let guard = EnvGuard::set("AINB_HOME", &ainb);
        Self {
            _root: root,
            ainb,
            hangar,
            _ainb_home: guard,
        }
    }

    fn sessions_json(&self) -> PathBuf {
        self.ainb.join(".agents-in-a-box").join("sessions.json")
    }

    fn write_file_store(&self, sessions: &[&SessionMetadata]) -> Vec<u8> {
        let mut store = SessionStore::default();
        for s in sessions {
            store.upsert((*s).clone());
        }
        let body = serde_json::to_vec_pretty(&store).expect("serialize store");
        fs::write(self.sessions_json(), &body).expect("write sessions.json");
        body
    }

    fn file_bytes(&self) -> Option<Vec<u8>> {
        fs::read(self.sessions_json()).ok()
    }

    /// Start a real daemon socket on the hangar home, with the dark sessions
    /// capability switched on so the daemon path can be driven.
    fn start_daemon(&self) -> FleetHangar {
        ainb_hangar_daemon::rpc::auth::advertise_workspace_sessions_for_tests(true);
        FleetHangar::start(&self.hangar)
    }

    /// The token the daemon wrote and the socket it listens on.
    fn daemon_parts(&self) -> (PathBuf, String) {
        let token = fs::read_to_string(ainb_hangar_proto::auth::token_file_in(&self.hangar))
            .expect("read daemon token");
        (
            ainb_hangar_daemon::rpc::socket_path_in(&self.hangar),
            token.trim().to_string(),
        )
    }
}

/// Bring the table up to date as a daemon boot does: the one-time import,
/// then a reconcile pass. P6e: `import_complete` needs both.
fn complete_import(hangar: &FleetHangar, homes: &Homes) {
    let path = homes.sessions_json();
    hangar.block_on(async {
        ainb_hangar_daemon::session_import::import_sessions_if_needed(hangar.pool(), &path)
            .await
            .expect("boot import");
        ainb_hangar_daemon::session_import::reconcile_sessions(hangar.pool(), &path)
            .await
            .expect("boot reconcile");
    });
}

fn table(hangar: &FleetHangar) -> Vec<SessionRow> {
    hangar.block_on(async {
        SessionsRepo::list(hangar.pool(), None, 1000).await.expect("list table")
    })
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("client runtime")
}

/// Dark: a daemon with a completed import that does not advertise the
/// capability (every production daemon in P6d) leaves the process on the file.
#[test]
fn a_dark_daemon_leaves_the_cli_on_the_file() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let homes = Homes::new();
    let meta = make_session("sess-file-1", "file-ws");
    homes.write_file_store(&[&meta]);
    let hangar = homes.start_daemon();
    complete_import(&hangar, &homes);
    ainb_hangar_daemon::rpc::auth::advertise_workspace_sessions_for_tests(false);
    let (socket, token) = homes.daemon_parts();

    let rt = rt();
    let source = rt.block_on(SessionSource::resolve_at(socket, token));
    assert!(matches!(source, SessionSource::File), "{source:?}");
    let run_meta = make_session("sess-run-1", "run-ws");
    rt.block_on(source.mutate(|s| s.upsert(run_meta.clone()))).expect("file write");
    let store = SessionStore::load();
    assert!(
        store.sessions.contains_key("sess-run-1"),
        "the run landed in the file"
    );
    assert_eq!(
        table(&hangar).len(),
        1,
        "the table holds only the imported row"
    );
}

/// Dark: the production resolver answers the file even with a live daemon on
/// the hangar home, because this build does not advertise the capability.
#[test]
fn the_production_resolver_is_the_file_while_dark() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let homes = Homes::new();
    let _hangar = homes.start_daemon();
    let _hangar_home = EnvGuard::set("AINB_HANGAR_HOME", &homes.hangar);
    let source = rt().block_on(SessionSource::resolve());
    assert!(matches!(source, SessionSource::File), "{source:?}");
}

/// On the file path a mutation that changes nothing leaves `sessions.json`
/// untouched, as v2's orphan cleanup did (it saved only on change); one that
/// changes something rewrites it.
#[test]
fn a_file_mutation_that_changes_nothing_does_not_rewrite_the_file() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let homes = Homes::new();
    let meta = make_session("sess-file-1", "file-ws");
    let mut store = SessionStore::default();
    store.upsert(meta.clone());
    // Compact, unlike SessionStore::save, so a rewrite shows in the bytes.
    let compact = serde_json::to_vec(&store).expect("serialize store");
    fs::write(homes.sessions_json(), &compact).expect("write sessions.json");

    let rt = rt();
    let source = SessionSource::File;
    rt.block_on(source.mutate(|s| s.remove_by_session_id(Uuid::new_v4())))
        .expect("no-op mutate");
    assert_eq!(
        homes.file_bytes(),
        Some(compact.clone()),
        "a no-op rewrote the file"
    );

    rt.block_on(source.mutate(|s| s.remove_by_session_id(meta.session_id)))
        .expect("real mutate");
    assert_ne!(homes.file_bytes(), Some(compact));
    assert!(SessionStore::load().sessions.is_empty());
}

/// No daemon at all: the process is degraded, and the file is read as-is.
/// P6e: a build that speaks the capability but reaches no daemon is
/// `Degraded`, not `File`, so a long-lived process knows to keep trying.
#[test]
fn with_no_daemon_the_file_is_the_source() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let homes = Homes::new();
    let meta = make_session("sess-stopped-1", "stopped-ws");
    homes.write_file_store(&[&meta]);

    let rt = rt();
    let missing = homes.hangar.join("hangar.sock");
    let source = rt.block_on(SessionSource::resolve_at(missing, "no-token".to_string()));
    assert!(source.is_degraded(), "{source:?}");

    let store = rt.block_on(source.load()).expect("file load");
    assert_eq!(store.sessions.len(), 1);
    assert_eq!(store.sessions["sess-stopped-1"].session_id, meta.session_id);
}

/// A daemon that advertises the capability but has no import marker must not
/// hide a populated file behind its empty table: the process is degraded and
/// reads the file.
#[test]
fn an_unimported_daemon_table_does_not_mask_the_file() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let homes = Homes::new();
    let meta = make_session("sess-file-1", "file-ws");
    homes.write_file_store(&[&meta]);
    let hangar = homes.start_daemon();
    let (socket, token) = homes.daemon_parts();

    let rt = rt();
    let source = rt.block_on(SessionSource::resolve_at(socket, token));
    assert!(source.is_degraded(), "{source:?}");
    let store = rt.block_on(source.load()).expect("file load");
    assert!(store.sessions.contains_key("sess-file-1"));
    assert!(table(&hangar).is_empty());
}

/// Writes go through the real handlers into the table, reads come back from
/// it with every field. P6e: each write also changes the session's row in
/// `sessions.json` first, so the file tracks the table row by row.
#[test]
fn daemon_source_round_trips_through_the_real_handlers() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let homes = Homes::new();
    let hangar = homes.start_daemon();
    complete_import(&hangar, &homes);
    let (socket, token) = homes.daemon_parts();

    let rt = rt();
    let source = rt.block_on(SessionSource::resolve_at(socket, token));
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");

    let meta = make_session("sess-run-1", "run-ws");
    let id = meta.session_id;
    rt.block_on(source.mutate(|s| s.upsert(meta.clone()))).expect("daemon upsert");
    assert_eq!(
        SessionStore::load().sessions["sess-run-1"].session_id,
        id,
        "the write put its row in sessions.json too"
    );

    let rows = table(&hangar);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id, id.to_string());
    assert_eq!(rows[0].agent_type, "Codex");
    assert_eq!(rows[0].model.as_deref(), Some("gpt-5-codex"));
    assert!(rows[0].rtk_enabled);

    let store = rt.block_on(source.load()).expect("daemon load");
    let back = &store.sessions["sess-run-1"];
    assert_eq!(back.session_id, id);
    assert_eq!(back.agent_type, SessionAgentType::Codex);
    assert_eq!(back.model_source, ModelSource::Raw);
    assert_eq!(back.skip_permissions, Some(true));

    rt.block_on(source.mutate(|s| s.remove_by_session_id(id)))
        .expect("daemon delete");
    assert!(table(&hangar).is_empty());
    assert!(
        SessionStore::load().sessions.is_empty(),
        "the delete removed the file row too"
    );
}

/// A session the daemon registered and one `ainb run` wrote both appear once.
#[test]
fn a_daemon_session_and_a_run_session_both_appear_once() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let homes = Homes::new();
    let hangar = homes.start_daemon();
    complete_import(&hangar, &homes);
    let (socket, token) = homes.daemon_parts();

    let daemon_sid = Uuid::new_v4().to_string();
    let daemon_row = SessionRow {
        session_id: daemon_sid.clone(),
        tmux_session_name: "tmux_hangar-01J".to_string(),
        worktree_path: "/tmp/work/daemon-ws".to_string(),
        workspace_name: "daemon-ws".to_string(),
        created_at: Utc::now().timestamp_millis(),
        agent_type: "Codex".to_string(),
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: Some(true),
        model: None,
        model_source: "LegacyTyped".to_string(),
        codex_model: None,
        codex_thread_id: None,
    };
    hangar.block_on(async {
        SessionsRepo::upsert(hangar.pool(), &daemon_row)
            .await
            .expect("daemon registration");
    });

    let rt = rt();
    let source = rt.block_on(SessionSource::resolve_at(socket, token));
    let run_meta = make_session("sess-run-1", "run-ws");
    rt.block_on(source.mutate(|s| s.upsert(run_meta.clone()))).expect("run write");

    let store = rt.block_on(source.load()).expect("load");
    assert_eq!(store.sessions.len(), 2);
    assert_eq!(
        store.sessions["tmux_hangar-01J"].session_id.to_string(),
        daemon_sid
    );
    assert_eq!(
        store.sessions["tmux_hangar-01J"].agent_type,
        SessionAgentType::Codex
    );
    assert_eq!(store.sessions["sess-run-1"].session_id, run_meta.session_id);
}

/// The daemon dies after the process chose it. The write fails loudly and
/// nothing lands in the file: one command never straddles two sources.
#[test]
fn daemon_death_mid_command_is_an_error_not_a_fallback() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let homes = Homes::new();
    let file_meta = make_session("sess-file-1", "file-ws");
    let file_before = homes.write_file_store(&[&file_meta]);
    let hangar = homes.start_daemon();
    complete_import(&hangar, &homes);
    let (socket, token) = homes.daemon_parts();

    let rt = rt();
    let source = rt.block_on(SessionSource::resolve_at(socket, token));
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");

    drop(hangar);

    let err = rt
        .block_on(source.mutate(|s| s.upsert(make_session("sess-late", "late-ws"))))
        .expect_err("a write to a dead daemon must fail");
    assert!(err.to_string().contains("daemon"), "{err}");
    assert!(rt.block_on(source.load()).is_err(), "a read must fail too");
    assert_eq!(homes.file_bytes(), Some(file_before));
}

/// A fake daemon: every connection gets a hello advertising the sessions
/// capability, then `reply(method)` for its one request.
fn fake_daemon(
    rt: &tokio::runtime::Runtime,
    dir: &Path,
    reply: fn(&str) -> serde_json::Value,
) -> PathBuf {
    let socket = dir.join("fake.sock");
    let listener = rt.block_on(async { UnixListener::bind(&socket) }).expect("bind fake");
    rt.spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
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
                let mut resp = reply(req["method"].as_str().unwrap_or_default());
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
    let body = serde_json::to_vec(value).expect("frame serializes");
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    let _ = writer.write_all(&frame).await;
    let _ = writer.flush().await;
}

fn empty_imported_list() -> serde_json::Value {
    serde_json::json!({ "result": { "sessions": [], "truncated": false, "import_complete": true } })
}

/// The daemon answers hello and list, then fails the write. The error
/// reaches the caller (so `ainb run` rolls back) and the file is untouched.
#[test]
fn hello_then_a_failed_write_is_returned_to_the_caller() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let homes = Homes::new();
    let file_meta = make_session("sess-file-1", "file-ws");
    let file_before = homes.write_file_store(&[&file_meta]);

    let rt = rt();
    let socket = fake_daemon(&rt, &homes.hangar, |method| match method {
        "workspace/session_list" => empty_imported_list(),
        _ => {
            serde_json::json!({ "error": { "code": -32603, "message": "store error: disk I/O error" } })
        }
    });
    let source = rt.block_on(SessionSource::resolve_at(socket, "t".to_string()));
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");

    let err = rt
        .block_on(source.mutate(|s| s.upsert(make_session("sess-new", "new-ws"))))
        .expect_err("a refused write must not read as success");
    assert!(err.to_string().contains("disk I/O error"), "{err}");
    assert_eq!(homes.file_bytes(), Some(file_before));
}

/// Lists the restart test's fake daemon has answered.
static RESTART_LISTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Whether the restart test's fake daemon was sent any upsert or delete.
static RESTART_WROTE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// P6e: a daemon that restarts answers not-ready (no rows,
/// `import_complete: false`) until its first reconcile pass commits. A
/// process already on the daemon must read that as an error, not as zero
/// sessions: `mutate` would otherwise diff against an empty view. Here the
/// source was resolved while ready, then every later list is not-ready; the
/// mutation fails, sends no write, and leaves the file alone.
#[test]
fn a_not_ready_list_after_resolve_is_an_error_not_an_empty_store() {
    use std::sync::atomic::Ordering;

    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    RESTART_LISTS.store(0, Ordering::SeqCst);
    RESTART_WROTE.store(false, Ordering::SeqCst);
    let homes = Homes::new();
    let kept = make_session("sess-kept", "kept-ws");
    let file_before = homes.write_file_store(&[&kept]);

    let rt = rt();
    let socket = fake_daemon(&rt, &homes.hangar, |method| match method {
        "workspace/session_list" if RESTART_LISTS.fetch_add(1, Ordering::SeqCst) == 0 => {
            empty_imported_list()
        }
        "workspace/session_list" => serde_json::json!({
            "result": { "sessions": [], "truncated": false, "import_complete": false }
        }),
        _ => {
            RESTART_WROTE.store(true, Ordering::SeqCst);
            serde_json::json!({ "result": { "ok": true, "deleted": true } })
        }
    });
    let source = rt.block_on(SessionSource::resolve_at(socket, "t".to_string()));
    assert!(matches!(source, SessionSource::Daemon(_)), "{source:?}");

    let err = rt.block_on(source.load()).expect_err("a not-ready list is not zero sessions");
    assert!(err.to_string().contains("not ready"), "{err}");
    let err = rt
        .block_on(source.mutate(|s| s.upsert(make_session("sess-new", "new-ws"))))
        .expect_err("a mutation over a not-ready list must fail");
    assert!(err.to_string().contains("still reconciling"), "{err}");
    assert!(
        !RESTART_WROTE.load(Ordering::SeqCst),
        "a write was sent from an empty view"
    );
    assert_eq!(homes.file_bytes(), Some(file_before));
}

/// The daemon answers hello but fails the list used to decide the source.
/// The process settles, degraded, on the file before doing anything, once.
#[test]
fn hello_then_a_failed_list_settles_on_the_file() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let homes = Homes::new();
    let meta = make_session("sess-file-1", "file-ws");
    homes.write_file_store(&[&meta]);

    let rt = rt();
    let socket = fake_daemon(
        &rt,
        &homes.hangar,
        |_| serde_json::json!({ "error": { "code": -32603, "message": "boom" } }),
    );
    let source = rt.block_on(SessionSource::resolve_at(socket, "t".to_string()));
    assert!(source.is_degraded(), "{source:?}");
    assert!(
        rt.block_on(source.load())
            .expect("file load")
            .sessions
            .contains_key("sess-file-1")
    );
}

/// A table row whose id is not a UUID (written before the boundary checked)
/// is skipped on read, never given an invented id, and a write elsewhere in
/// the store leaves it alone.
#[test]
fn a_non_uuid_row_is_skipped_not_reinvented() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let homes = Homes::new();
    let hangar = homes.start_daemon();
    complete_import(&hangar, &homes);
    let legacy = SessionRow {
        session_id: "01J8Z3K6Q2N4T5V7W9X0Y1Z2A3".to_string(),
        tmux_session_name: "tmux_legacy".to_string(),
        worktree_path: "/tmp/work/legacy".to_string(),
        workspace_name: "legacy".to_string(),
        created_at: 1,
        agent_type: "Claude".to_string(),
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        model_source: "LegacyTyped".to_string(),
        codex_model: None,
        codex_thread_id: None,
    };
    hangar.block_on(async {
        SessionsRepo::upsert(hangar.pool(), &legacy).await.expect("seed legacy row");
    });
    let (socket, token) = homes.daemon_parts();

    let rt = rt();
    let source = rt.block_on(SessionSource::resolve_at(socket, token));
    let first = rt.block_on(source.load()).expect("load");
    let second = rt.block_on(source.load()).expect("load");
    assert!(first.sessions.is_empty(), "{:?}", first.sessions.keys());
    assert!(second.sessions.is_empty());

    rt.block_on(source.mutate(|s| s.upsert(make_session("sess-new", "new-ws"))))
        .expect("write");
    let rows = table(&hangar);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r == &legacy), "legacy row changed");
}

/// Downgrade (P6e criterion 7a, new writer, old reader): a file imported by
/// the daemon, then edited through the daemon, carries the edit row by row.
/// A binary without the daemon path (here: the real `ainb list` with no
/// daemon reachable) lists the session the edit created and not the one it
/// killed, and still finds the session it left alone.
#[test]
fn a_downgrade_still_finds_its_sessions_in_the_file() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let homes = Homes::new();
    let a = make_session("sess-a", "ws-a");
    let b = make_session("sess-b", "ws-b");
    let file_before = homes.write_file_store(&[&a, &b]);
    let hangar = homes.start_daemon();
    complete_import(&hangar, &homes);
    assert_eq!(table(&hangar).len(), 2, "the boot import copies both");
    let (socket, token) = homes.daemon_parts();

    let rt = rt();
    let source = rt.block_on(SessionSource::resolve_at(socket, token));
    rt.block_on(source.mutate(|s| {
        s.remove_by_session_id(a.session_id);
        s.upsert(make_session("sess-c", "ws-c"));
    }))
    .expect("daemon-mode edit");
    drop(hangar);

    assert_ne!(homes.file_bytes(), Some(file_before));

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ainb"))
        .args(["list", "--format", "json"])
        .env("AINB_HOME", &homes.ainb)
        .env("HOME", &homes.ainb)
        .env("AINB_HANGAR_HOME", homes.hangar.join("gone"))
        .output()
        .expect("run ainb list");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(
        listed.contains(&b.session_id.to_string()),
        "the untouched session is missing from:\n{listed}"
    );
    assert!(
        listed.contains("sess-c"),
        "the session the daemon-mode edit created is missing from:\n{listed}"
    );
    assert!(
        !listed.contains(&a.session_id.to_string()),
        "the session the daemon-mode edit killed is still listed:\n{listed}"
    );
}
