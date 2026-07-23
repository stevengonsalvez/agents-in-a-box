use std::path::Path;
use std::process::Command;
use std::time::Instant;

use ainb_hangar_daemon::events::{EventBroker, EventSink};
use ainb_hangar_daemon::fleet::{self, HookObservation};
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_store::Store;
use sqlx::Row;

pub struct Receipt {
    pub action_kind: String,
    pub status: String,
    pub detail: Option<String>,
}

/// Test-owned tmux session with exact-name cleanup on success or panic.
pub struct ExactTmuxSession {
    name: String,
}

impl ExactTmuxSession {
    pub fn create(name: String, width: &str, height: &str) -> Self {
        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", &name, "-x", width, "-y", height])
            .status()
            .expect("tmux new-session");
        assert!(status.success(), "tmux new-session failed");
        Self { name }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for ExactTmuxSession {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.name]).status();
    }
}

/// Restore one process environment variable after a test, including unwind.
pub struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    pub fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Real Hangar socket server plus reducer-backed isolated store.
///
/// Dropping the runtime aborts the exact server task. No process-wide daemon or
/// shared socket is touched.
pub struct FleetHangar {
    runtime: tokio::runtime::Runtime,
    store: Store,
    events: EventSink,
}

impl FleetHangar {
    pub fn start(hangar_home: &Path) -> Self {
        let runtime = tokio::runtime::Runtime::new().expect("create Hangar runtime");
        let (store, events) =
            runtime.block_on(async {
                let store = Store::open_in(hangar_home).await.expect("open isolated Hangar store");
                rpc::auth::ensure_socket_token(store.pool(), hangar_home)
                    .await
                    .expect("create isolated daemon token");
                let socket = rpc::socket_path_in(hangar_home);
                let listener = rpc::bind(&socket).expect("bind isolated Hangar socket");
                let broker = EventBroker::new();
                let events = broker.sink();
                let health = DaemonHealth {
                    socket_path: socket.to_string_lossy().into_owned(),
                    pid: std::process::id(),
                    started_at: Instant::now(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    stats: std::sync::Arc::new(
                        ainb_hangar_daemon::health_stats::HealthStats::default(),
                    ),
                };
                tokio::spawn(rpc::serve(listener, store.pool().clone(), health, broker));
                (store, events)
            });
        Self {
            runtime,
            store,
            events,
        }
    }

    pub fn apply_hook(
        &self,
        event_id: &str,
        provider_session_id: &str,
        cwd: &Path,
        event_type: &str,
        payload: serde_json::Value,
        observed_at: i64,
    ) {
        let cwd = cwd.display().to_string();
        self.runtime
            .block_on(fleet::apply_hook(
                self.store.pool(),
                &self.events,
                HookObservation {
                    event_id: event_id.to_string(),
                    provider: "claude",
                    provider_session_id,
                    cwd: &cwd,
                    event_type,
                    payload: &payload,
                    observed_at,
                },
            ))
            .expect("apply authoritative Fleet hook");
    }

    pub fn latest_receipt(&self, session_key: &str) -> Option<Receipt> {
        self.runtime.block_on(async {
            sqlx::query(
                "SELECT action_kind, status, detail FROM fleet_action_receipt \
                 WHERE session_key = ? ORDER BY updated_at DESC LIMIT 1",
            )
            .bind(session_key)
            .fetch_optional(self.store.pool())
            .await
            .expect("query Fleet action receipt")
            .map(|row| Receipt {
                action_kind: row.get("action_kind"),
                status: row.get("status"),
                detail: row.get("detail"),
            })
        })
    }

    pub fn session(&self, session_key: &str) -> Option<ainb_hangar_proto::fleet::FleetSession> {
        self.runtime
            .block_on(ainb_hangar_daemon::fleet::snapshot_wire(self.store.pool()))
            .expect("read authoritative Fleet snapshot")
            .sessions
            .into_iter()
            .find(|session| session.session_key == session_key)
    }
}
