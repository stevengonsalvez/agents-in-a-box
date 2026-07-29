//! Real Fleet daemon fixture for the macOS RPC contract tests.
//!
//! The process owns an isolated `AINB_HANGAR_HOME` supplied by its test parent.
//! It creates the real SQLite store, daemon token, Unix socket server, durable
//! Fleet revision log, and live event broker. JSON commands on stdin only inject
//! provider hook observations; they never emulate an RPC response or Fleet state.

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::fleet::{self, HookObservation};
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_store::Store;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Command {
    Seed {
        event_id: String,
        #[serde(default = "default_provider")]
        provider: String,
        #[serde(default = "default_session_id")]
        session_id: String,
        #[serde(default = "default_event_type")]
        event_type: String,
        #[serde(default = "default_cwd")]
        cwd: String,
        #[serde(default)]
        payload: Value,
        #[serde(default)]
        observed_at: Option<i64>,
    },
    Shutdown,
}

fn default_provider() -> String {
    "claude".to_string()
}
fn default_session_id() -> String {
    "fixture-session".to_string()
}
fn default_event_type() -> String {
    "SessionStart".to_string()
}
fn default_cwd() -> String {
    "/fixture".to_string()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let home = fixture_home()?;
    let store = Store::open_in(&home).await?;
    rpc::auth::ensure_socket_token(store.pool(), &home).await?;
    let socket = rpc::socket_path_in(&home);
    let listener = rpc::bind(&socket)?;
    let broker = EventBroker::new();
    let sink = broker.sink();
    let health = DaemonHealth {
        socket_path: socket.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        stats: Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(listener, store.pool().clone(), health, broker));

    let mut next_observed_at = 1_700_000_000_000_i64;
    'commands: loop {
        for line in std::io::stdin().lock().lines() {
            let line = line?;
            let command: Command = match serde_json::from_str(&line) {
                Ok(command) => command,
                Err(error) => {
                    println!(
                        "{}",
                        serde_json::json!({ "ok": false, "error": error.to_string() })
                    );
                    std::io::stdout().flush()?;
                    continue;
                }
            };
            match command {
                Command::Seed {
                    event_id,
                    provider,
                    session_id,
                    event_type,
                    cwd,
                    payload,
                    observed_at,
                } => {
                    let observed_at = observed_at.unwrap_or_else(|| {
                        next_observed_at += 1;
                        next_observed_at
                    });
                    match fleet::apply_hook(
                        store.pool(),
                        &sink,
                        HookObservation {
                            event_id: event_id.clone(),
                            provider: &provider,
                            provider_session_id: &session_id,
                            cwd: &cwd,
                            event_type: &event_type,
                            payload: &payload,
                            observed_at,
                        },
                    )
                    .await
                    {
                        Ok(result) => println!(
                            "{}",
                            serde_json::json!({
                                "ok": true,
                                "event_id": event_id,
                                "revision": result.revision,
                                "duplicate": result.duplicate,
                            })
                        ),
                        Err(error) => println!(
                            "{}",
                            serde_json::json!({ "ok": false, "error": error.to_string() })
                        ),
                    }
                    std::io::stdout().flush()?;
                }
                Command::Shutdown => break 'commands,
            }
        }
        // XCTest may momentarily close its inherited stdin during test-host launch.
        // Keep the real daemon available for its readiness and socket proof instead
        // of turning that transport startup race into a clean fixture exit.
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}

fn fixture_home() -> anyhow::Result<PathBuf> {
    let raw = std::env::var_os("AINB_HANGAR_HOME")
        .ok_or_else(|| anyhow::anyhow!("AINB_HANGAR_HOME is required for fixture isolation"))?;
    let home = PathBuf::from(raw);
    if !home.is_absolute() {
        anyhow::bail!("AINB_HANGAR_HOME must be an absolute isolated directory");
    }
    std::fs::create_dir_all(&home)?;
    Ok(home)
}
