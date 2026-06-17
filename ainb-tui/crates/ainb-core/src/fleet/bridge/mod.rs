// ABOUTME: Native single-binary phone bridge — `ainb fleet bridge`.
//
// A Rust port of the Python `ainb_phone_bridge`, folding both channels into the
// ainb binary so there is no separate Python runtime to install or manage:
//
//   * Telegram (long-polling getUpdates over reqwest) — `telegram.rs`
//   * Slack    (socket-mode WebSocket via tokio-tungstenite) — `slack.rs`
//
// Both channels share ONE relay/routing core (`relay.rs`) over one transport
// (`transport.rs`: discover via `ainb list`, deliver via tmux send-keys with the
// `--` terminator, capture the reply from the JSONL transcript tail with the
// complete-line / rotation-follow / send-time-guard fixes verified in Python).
//
// Config (`config.rs`) lives under `[fleet.bridge.telegram]` / `[fleet.bridge.slack]`
// in ainb's config.toml; tokens resolve via the secret resolver (`secrets.rs`)
// and are NEVER passed on argv or written into the launchd/systemd unit
// (`service.rs`). The pure logic (routing, markdown/split, secrets, relay) is
// unit-tested without a live fleet or network.

pub mod config;
pub mod format;
pub mod heartbeat;
pub mod redact;
pub mod relay;
pub mod routing;
pub mod secrets;
pub mod service;
pub mod slack;
pub mod telegram;
pub mod transport;

use anyhow::{Result, bail};

use config::{BridgeConfig, load_config};
use transport::AinbTransport;

/// Run the bridge daemon: load config, then drive every configured channel
/// concurrently over the shared transport. Runs until the process is stopped
/// (or every channel exits, which only happens on a fatal error).
pub async fn run() -> Result<()> {
    let cfg: BridgeConfig = load_config(None)?;
    run_with_config(cfg).await
}

/// Run with an already-loaded config (the testable seam — no disk access).
pub async fn run_with_config(cfg: BridgeConfig) -> Result<()> {
    if !cfg.any_channel() {
        bail!("no bridge channel configured — add [fleet.bridge.telegram] or [fleet.bridge.slack]");
    }

    // The shared transport is cheap (a zero-sized marker); each channel borrows
    // it. We leak a single instance so both channel tasks can hold a `'static`
    // reference for the lifetime of the daemon (it lives until process exit).
    let transport: &'static AinbTransport = Box::leak(Box::new(AinbTransport));

    // Heartbeat: write the initial "starting" record and keep the liveness clock
    // fresh via the idle ticker. Each channel reports its own connection +
    // relay activity through a clone of this handle. This is what closes the
    // "running bridge is indistinguishable from a dead one" blind spot.
    let heartbeat = heartbeat::BridgeHeartbeat::start();
    heartbeat.spawn_ticker();

    let mut tasks = Vec::new();

    if let Some(tg) = cfg.telegram {
        let hb = heartbeat.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = telegram::run(tg, transport, hb).await {
                tracing::error!(error = %e, "Telegram channel exited with error");
            }
        }));
    }
    if let Some(sl) = cfg.slack {
        let hb = heartbeat.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = slack::run(sl, transport, hb).await {
                tracing::error!(error = %e, "Slack channel exited with error");
            }
        }));
    }

    // Wait for all channel tasks. Each channel loops forever internally, so this
    // only returns if a channel panics or its loop terminates.
    for task in tasks {
        let _ = task.await;
    }
    bail!("all bridge channels stopped");
}

/// Install the bridge as a launchd/systemd service (idempotent).
pub fn install() -> Result<std::path::PathBuf> {
    service::install()
}

/// Uninstall the bridge service. Returns the removed unit path, if any.
pub fn uninstall() -> Result<Option<std::path::PathBuf>> {
    service::uninstall()
}

/// Human-readable install status.
pub fn status() -> Result<String> {
    service::status()
}
