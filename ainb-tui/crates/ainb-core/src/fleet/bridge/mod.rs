// ABOUTME: Native single-binary phone bridge — `ainb fleet bridge`.
//
// A Rust port of the Python `ainb_phone_bridge`, folding both channels into the
// ainb binary so there is no separate Python runtime to install or manage:
//
//   * Telegram (long-polling getUpdates over reqwest) — `telegram.rs`
//   * Slack    (socket-mode WebSocket via tokio-tungstenite) — `slack.rs`
//   * Discord  (raw Gateway WebSocket via tokio-tungstenite) — `discord.rs`
//
// The bridge is TWO-WAY. Inbound is the three channel runners above. Outbound is
// `outbound.rs`: one worker that polls the hangar daemon's open attention inbox
// and pushes each newly-open phone-routed ASK/escalation to every configured
// channel. `run_with_config` spawns it alongside the channels. Without that
// spawn the module is unreachable and the phone never hears from the fleet,
// which is exactly how it shipped: fully implemented, fully unit-tested, never
// called. The worker stamps its liveness onto the shared heartbeat so
// `ainb fleet daemons` degrades when it cannot reach the daemon, instead of
// reporting a green "running + connected" that only ever described the INBOUND
// chat gateway.
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

pub mod answer;
pub mod config;
pub mod daemon;
pub mod discord;
pub mod format;
pub mod heartbeat;
pub mod outbound;
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
        bail!(
            "no bridge channel configured — add [fleet.bridge.telegram], \
             [fleet.bridge.slack], or [fleet.bridge.discord]"
        );
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

    // The OUTBOUND half. Built before the channels take ownership of their
    // configs: one notifier per configured channel, fanned out from a single
    // attention-inbox poll loop.
    let outbound_notifier = build_outbound_notifier(&cfg);

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
    if let Some(dc) = cfg.discord {
        let hb = heartbeat.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = discord::run(dc, transport, hb).await {
                tracing::error!(error = %e, "Discord channel exited with error");
            }
        }));
    }

    // Spawn the outbound attention-push worker. Everything about this block is
    // non-fatal by design: a bridge that cannot reach the daemon must still
    // relay INBOUND messages. What it must NOT do is stay silent about it, so
    // every failure path lands on the heartbeat and degrades
    // `ainb fleet daemons` instead of leaving a green row.
    if !cfg.outbound_enabled {
        tracing::warn!(
            "outbound attention push disabled by config ([fleet.bridge] outbound_enabled = false); \
             nothing will be pushed to the phone"
        );
        heartbeat.record_attention_error(
            "outbound push disabled by config ([fleet.bridge] outbound_enabled = false)",
        );
    } else if let Some(notifier) = outbound_notifier {
        match daemon::DaemonClient::from_env() {
            Ok(client) => {
                let hb = heartbeat.clone();
                let interval = std::time::Duration::from_secs(cfg.outbound_poll_secs);
                tasks.push(tokio::spawn(async move {
                    outbound::run(client, notifier, interval, hb).await;
                }));
            }
            Err(e) => {
                // No client means no push, ever. Record it so health degrades
                // rather than reporting a bridge that is connected to chat and
                // deaf to the fleet.
                tracing::warn!(
                    error = %e,
                    "outbound attention push unavailable: could not build a hangar daemon client"
                );
                heartbeat.record_attention_error(format!("hangar daemon client: {e}"));
            }
        }
    }

    // Wait for all channel tasks. Each channel loops forever internally, so this
    // only returns if a channel panics or its loop terminates.
    for task in tasks {
        let _ = task.await;
    }
    bail!("all bridge channels stopped");
}

/// Build the fan-out notifier the outbound worker pushes through: one entry per
/// configured channel. Returns `None` when no channel could build a notifier, in
/// which case there is nothing to spawn.
///
/// A channel whose notifier fails to build (e.g. its HTTP client) is logged and
/// skipped rather than taking the bridge down: the other channels, and the
/// entire inbound path, still work.
/// A notifier takes no heartbeat handle: it REPORTS each delivery outcome to the
/// outbound worker, which owns the single place that records what reached the
/// human and what did not.
fn build_outbound_notifier(cfg: &BridgeConfig) -> Option<outbound::Fanout> {
    let mut notifiers: Vec<Box<dyn outbound::Notifier>> = Vec::new();
    if let Some(tg) = cfg.telegram.as_ref() {
        match telegram::TelegramNotifier::new(tg) {
            Ok(n) => notifiers.push(Box::new(n)),
            Err(e) => tracing::warn!(error = %e, "Telegram outbound notifier unavailable"),
        }
    }
    if let Some(sl) = cfg.slack.as_ref() {
        match slack::SlackNotifier::new(sl) {
            Ok(n) => notifiers.push(Box::new(n)),
            Err(e) => tracing::warn!(error = %e, "Slack outbound notifier unavailable"),
        }
    }
    if let Some(dc) = cfg.discord.as_ref() {
        match discord::DiscordNotifier::new(dc.clone()) {
            Ok(n) => notifiers.push(Box::new(n)),
            Err(e) => tracing::warn!(error = %e, "Discord outbound notifier unavailable"),
        }
    }
    let fanout = outbound::Fanout::new(notifiers);
    (!fanout.is_empty()).then_some(fanout)
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
