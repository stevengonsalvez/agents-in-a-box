//! The real daemon binary opens its socket at once, but serves no session
//! read from the table until this boot's first reconcile pass has committed
//! (P6e, amended: "socket opens at once, first read waits for the first
//! pass").
//!
//! The home is seeded as a previous boot left it: import and reconcile
//! markers present, and a table that no longer matches `sessions.json`. Then
//! `sessions.json.lock` is held so the boot pass cannot run. No session RPC in
//! that window may act on that stale table: the list answers not-ready, and a
//! mutation is refused with `STORE_UNAVAILABLE`.

use ainb_hangar_client::{
    DaemonClient, DaemonError, WorkspaceSessionDeleteParams, WorkspaceSessionListParams,
    WorkspaceSessionUpsertParams,
};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::sessions::{SessionRow, SessionsRepo};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const KEPT: &str = "00000000-0000-0000-0000-00000000f001";
const NEW: &str = "00000000-0000-0000-0000-00000000f002";

/// The socket and `auth/hello` must answer inside the flock bound the boot
/// pass is stuck on: a boot that waited on the pass costs at least the whole
/// bound, so it fails here. Three quarters of the bound leaves a quarter as
/// the margin that tells the two apart. A debug build answers in about 0.35 s
/// from spawn on the dev box, so a false red needs the runner to be about
/// four times slower than that.
fn opens_within() -> Duration {
    ainb_hangar_daemon::session_import::SESSIONS_FLOCK_BOUND * 3 / 4
}

/// The daemon child, killed by its own pid when the test ends.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn row(id: &str, tmux: &str) -> SessionRow {
    SessionRow {
        session_id: id.to_string(),
        tmux_session_name: tmux.to_string(),
        worktree_path: format!("/home/user/work/{tmux}"),
        workspace_name: "ws".to_string(),
        created_at: 1_757_937_600_000,
        agent_type: "Claude".to_string(),
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        model_source: "LegacyTyped".to_string(),
        codex_model: None,
        codex_thread_id: None,
    }
}

fn write_sessions(path: &Path) {
    let entry = |id: &str, tmux: &str| {
        serde_json::json!({
            "session_id": id,
            "tmux_session_name": tmux,
            "worktree_path": format!("/home/user/work/{tmux}"),
            "workspace_name": "ws",
            "created_at": 1_757_937_600_000_i64,
            "agent_type": "Claude"
        })
    };
    let json = serde_json::json!({
        "sessions": { "ainb-kept": entry(KEPT, "ainb-kept"), "ainb-new": entry(NEW, "ainb-new") }
    });
    std::fs::write(path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
}

fn ids(sessions: &[ainb_hangar_client::WorkspaceSessionEntry]) -> Vec<&str> {
    let mut ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
    ids.sort_unstable();
    ids
}

fn entry(id: &str, tmux: &str) -> ainb_hangar_client::WorkspaceSessionEntry {
    let r = row(id, tmux);
    ainb_hangar_client::WorkspaceSessionEntry {
        session_id: r.session_id,
        tmux_session_name: r.tmux_session_name,
        worktree_path: r.worktree_path,
        workspace_name: r.workspace_name,
        created_at: r.created_at,
        agent_type: r.agent_type,
        headroom_enabled: r.headroom_enabled,
        rtk_enabled: r.rtk_enabled,
        skip_permissions: r.skip_permissions,
        model: r.model,
        model_source: r.model_source,
        codex_model: r.codex_model,
        codex_thread_id: r.codex_thread_id,
    }
}

/// What one session RPC answered while the first pass was held.
#[derive(Debug, PartialEq, Eq)]
enum Held {
    /// The list's not-ready shape: no rows, `import_complete: false`.
    ListNotReady,
    /// A mutation refused with `STORE_UNAVAILABLE`.
    StoreUnavailable,
    /// The reconcile ran a pass of its own, which hit the held lock.
    PassRanIntoTheLock,
    /// Anything else, which means the RPC acted on the stale table.
    Served(String),
}

fn refused<T: std::fmt::Debug>(answer: Result<T, DaemonError>) -> Held {
    match answer {
        Err(DaemonError::Rpc { code, .. }) if code == ainb_hangar_proto::STORE_UNAVAILABLE => {
            Held::StoreUnavailable
        }
        other => Held::Served(format!("{other:?}")),
    }
}

/// Dial the daemon and complete `auth/hello` while the test holds the lock,
/// failing if that takes longer than [`opens_within`] from `spawned`.
///
/// `connections_list` is not a session RPC, so it is not gated, and every
/// call dials and says hello first.
async fn connect_while_locked(hangar: &Path, spawned: Instant) -> DaemonClient {
    let socket = ainb_hangar_client::socket_path_in(hangar);
    let token_file = ainb_hangar_proto::auth::token_file_in(hangar);
    loop {
        if socket.exists() && token_file.exists() {
            let token = std::fs::read_to_string(&token_file).unwrap().trim().to_string();
            let client = DaemonClient::with_parts(socket.clone(), token);
            if client.connections_list().await.is_ok() {
                return client;
            }
        }
        assert!(
            spawned.elapsed() < opens_within(),
            "connect and hello waited on the held lock: not done after {:?}",
            spawned.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_session_rpc_touches_the_table_before_the_first_pass() {
    let home = tempfile::tempdir().unwrap();
    let hangar = home.path().join(".agents-in-a-box");
    std::fs::create_dir_all(hangar.join("config")).unwrap();
    let sessions_path = hangar.join("sessions.json");
    let source = sessions_path.to_string_lossy().into_owned();

    // A previous boot's leftovers: both markers, and a table holding only
    // KEPT, while the file now also holds NEW.
    {
        let store = Store::open_in(&hangar).await.unwrap();
        let pool = store.pool();
        SessionsRepo::upsert(pool, &row(KEPT, "ainb-kept")).await.unwrap();
        SessionsRepo::complete_import(pool, &source, &[], 0, 1).await.unwrap();
        SessionsRepo::complete_reconcile(pool, &source, &[], 0, 2).await.unwrap();
        pool.close().await;
    }
    write_sessions(&sessions_path);

    let lock = ainb_fleet_core::session_registry::lock_sessions_store_at(&hangar).unwrap();
    let spawned = Instant::now();
    let _daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_ainb-hangar-daemon"))
            .env("HOME", home.path())
            .env_remove("AINB_HANGAR_HOME")
            .env_remove("AINB_HOME")
            .env("HANGAR_TEST_PARENT_PID", std::process::id().to_string())
            .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ainb-hangar-daemon"),
    );

    let client = connect_while_locked(&hangar, spawned).await;

    // The boot pass is stuck on the lock this test holds. Every session RPC
    // waits for it, bounded, then refuses rather than act on the stale
    // table. One row per method.
    let asked = Instant::now();
    let (list, upsert, delete, reconcile) = tokio::join!(
        client.workspace_session_list(WorkspaceSessionListParams::default()),
        client.workspace_session_upsert(WorkspaceSessionUpsertParams {
            session: entry(NEW, "ainb-new"),
        }),
        client.workspace_session_delete(WorkspaceSessionDeleteParams {
            session_id: Some(KEPT.to_string()),
            tmux_session_name: None,
        }),
        client.workspace_session_reconcile(),
    );
    let waited = asked.elapsed();
    let list = match list {
        Ok(answer) if !answer.import_complete && answer.sessions.is_empty() => Held::ListNotReady,
        other => Held::Served(format!("{other:?}")),
    };
    // The reconcile RPC is exempt from the gate (a committed pass is what
    // opens it). It runs its own pass, which fails on the held lock: an
    // internal error naming the lock, not the gate's not-ready refusal.
    let reconcile = match reconcile {
        Err(DaemonError::Rpc {
            code: -32603,
            message,
        }) if message.contains("lock") => Held::PassRanIntoTheLock,
        other => Held::Served(format!("{other:?}")),
    };
    let held = [
        ("workspace/session_list", list, Held::ListNotReady),
        (
            "workspace/session_upsert",
            refused(upsert),
            Held::StoreUnavailable,
        ),
        (
            "workspace/session_delete",
            refused(delete),
            Held::StoreUnavailable,
        ),
        (
            "workspace/session_reconcile",
            reconcile,
            Held::PassRanIntoTheLock,
        ),
    ];
    for (method, got, want) in held {
        assert_eq!(got, want, "{method} before the first pass");
    }
    assert!(
        waited < Duration::from_secs(5),
        "the wait was not bounded: {waited:?}"
    );

    // With the lock free, the watcher's retry (backoff from 1 s, not the
    // 30 s tick) commits the first pass on its own, and reads are served,
    // NEW included. No reconcile RPC here: the boot pass must recover alone.
    drop(lock);
    let freed = Instant::now();
    let ready = loop {
        let answer = client
            .workspace_session_list(WorkspaceSessionListParams::default())
            .await
            .unwrap();
        if answer.import_complete {
            break answer;
        }
        assert!(
            freed.elapsed() < Duration::from_secs(10),
            "the first pass was not retried within 10 s of the lock freeing"
        );
    };
    assert_eq!(
        ids(&ready.sessions),
        vec![KEPT, NEW],
        "the refused delete removed nothing and the pass inserted NEW"
    );
}
