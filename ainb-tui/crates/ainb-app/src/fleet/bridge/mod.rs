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
use heartbeat::BridgeHeartbeat;
use transport::AinbTransport;

/// How many INBOUND chat channels this config will start.
///
/// The number the daemon is then held to on the heartbeat: a channel task that
/// exits walks the live count below this, and the health probe degrades the row
/// naming the inbound half. Pure so the accounting is testable without spawning
/// a channel or touching the network.
#[must_use]
fn configured_inbound_channels(cfg: &BridgeConfig) -> u32 {
    u32::from(cfg.telegram.is_some())
        + u32::from(cfg.slack.is_some())
        + u32::from(cfg.discord.is_some())
}

/// Record that one inbound chat channel has STOPPED.
///
/// Every channel's `run` loops forever, so ANY return is terminal: nothing
/// restarts the task, and from this moment nothing the human sends from the
/// phone can reach the fleet through that channel. Both arms are therefore a
/// failure to report, including the `Ok(())` one, which is a loop that ended
/// without saying why.
fn record_channel_exit(heartbeat: &BridgeHeartbeat, channel: &str, outcome: Result<()>) {
    let detail = match outcome {
        Ok(()) => "its loop ended without an error".to_string(),
        Err(e) => format!("{e}"),
    };
    tracing::error!(
        channel,
        detail,
        "bridge inbound channel STOPPED; nothing from the phone can reach the fleet on it"
    );
    heartbeat.record_inbound_exit(format!("{channel} channel stopped: {detail}"));
}

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
    // Declare the inbound claim BEFORE anything is spawned, so the window in
    // which a channel could die unaccounted for is empty.
    heartbeat.set_inbound_expected(configured_inbound_channels(&cfg));

    let mut tasks = Vec::new();

    // The OUTBOUND half. Built before the channels take ownership of their
    // configs: one notifier per configured channel, fanned out from a single
    // attention-inbox poll loop.
    let outbound_notifier = build_outbound_notifier(&cfg);

    // Each channel task reports its own DEATH on the way out. Logging the error
    // was never enough: the process outlives every channel (the outbound worker
    // below loops forever), the idle ticker keeps the liveness clock fresh, and
    // `connected` still reads true from the handshake, so an exited channel was
    // invisible on every surface.
    if let Some(tg) = cfg.telegram {
        let hb = heartbeat.clone();
        tasks.push(tokio::spawn(async move {
            let outcome = telegram::run(tg, transport, hb.clone()).await;
            record_channel_exit(&hb, "Telegram", outcome);
        }));
    }
    if let Some(sl) = cfg.slack {
        let hb = heartbeat.clone();
        tasks.push(tokio::spawn(async move {
            let outcome = slack::run(sl, transport, hb.clone()).await;
            record_channel_exit(&hb, "Slack", outcome);
        }));
    }
    if let Some(dc) = cfg.discord {
        let hb = heartbeat.clone();
        tasks.push(tokio::spawn(async move {
            let outcome = discord::run(dc, transport, hb.clone()).await;
            record_channel_exit(&hb, "Discord", outcome);
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

    // Wait for every task. Each channel loops forever internally, so a channel
    // task only ends if its loop terminates or it panics.
    //
    // PROCESS EXIT IS NOT THE HEALTH SIGNAL, and must not be mistaken for one.
    // The outbound worker below also loops forever, so whenever it is spawned
    // this join never completes and the `bail!` is unreachable: a bridge whose
    // every chat channel has died stays up, heartbeating, serving the outbound
    // half. That is the right behaviour (the phone should still be TOLD things
    // even when it can no longer answer), which is exactly why the inbound half
    // reports its own death on the heartbeat above instead of relying on the
    // process falling over. The `bail!` still fires in the one configuration
    // where nothing else is running: outbound disabled (or unavailable) and
    // every channel gone.
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
///
/// The unit is always written, but it is only STARTED when the daemon could
/// actually run: an unresolvable program OR an unusable config both mean
/// `ainb fleet bridge run` exits immediately, and `KeepAlive` /
/// `Restart=always` then respawn that exit forever into an unrotated log. The
/// config half is why this host ran a service that had never once started.
pub fn install() -> Result<std::path::PathBuf> {
    service::install(config_problem().is_none())
}

/// `Some(warning)` when the service `install` writes could not start: either
/// the unit names a program that does not resolve, or the config the daemon
/// reads is missing/unusable. The CLI prints this rather than reporting a
/// success that can never run.
#[must_use]
pub fn install_would_be_unrunnable() -> Option<String> {
    service::install_would_be_unrunnable().or_else(config_problem)
}

/// `Some(explanation)` when the config `ainb fleet bridge run` would load
/// cannot start a bridge. The explanation is the loader's own error, which
/// already names the file it looked in and the keys it needs.
///
/// Secrets never reach this string: the loader reports missing/empty KEYS, and
/// resolved values are not echoed.
#[must_use]
pub fn config_problem() -> Option<String> {
    load_config(None).err().map(|e| format!("{e:#}"))
}

/// [`config_problem`], memoised on the config file's identity.
///
/// The Daemons collector asks this every couple of seconds for as long as a
/// bridge row is down, and the answer is not cheap: parsing resolves the
/// configured secrets, and a `keychain:<service>` ref shells out to
/// `/usr/bin/security`. Uncached, a stopped bridge with a keychain token means
/// a `security` subprocess every 2s — repeated unlock prompts, and a 5s stall
/// each time the keychain is locked.
///
/// The verdict only changes when the file does, so key the cache on its path,
/// mtime and length. A config that cannot be stat'd re-reads every time, which
/// is the safe direction: it is the case where the answer is "missing", and
/// that is the cheap branch anyway.
pub fn config_problem_cached() -> Option<String> {
    use std::sync::Mutex;
    type Stamp = (std::path::PathBuf, Option<std::time::SystemTime>, u64);
    static CACHE: Mutex<Option<(Stamp, Option<String>)>> = Mutex::new(None);

    let path = crate::fleet::bridge::config::default_config_path();
    let meta = std::fs::metadata(&path).ok();
    let stamp: Stamp = (
        path,
        meta.as_ref().and_then(|m| m.modified().ok()),
        meta.as_ref().map_or(0, std::fs::Metadata::len),
    );

    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((cached_stamp, verdict)) = cache.as_ref() {
        if *cached_stamp == stamp {
            return verdict.clone();
        }
    }
    let verdict = config_problem();
    *cache = Some((stamp, verdict.clone()));
    verdict
}

/// Uninstall the bridge service. Returns the removed unit path, if any.
pub fn uninstall() -> Result<Option<std::path::PathBuf>> {
    service::uninstall()
}

/// Human-readable install status.
///
/// Install state alone was a half-truth: a correctly-installed unit whose
/// config has no `[fleet.bridge]` table reported clean while the bridge exited
/// 1 on every launch. The config verdict is part of the status.
pub fn status() -> Result<String> {
    let unit = service::status()?;
    Ok(match config_problem() {
        Some(problem) => format!("{unit}\n  config: UNUSABLE — {problem}"),
        None => format!("{unit}\n  config: ok"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::daemons::heartbeat::{DaemonHeartbeat, PidCheck, process_start_ms};
    use crate::fleet::daemons::probe::{DaemonKind, DaemonState, classify_heartbeat};
    use config::{DiscordConfig, TelegramConfig};

    fn telegram() -> TelegramConfig {
        TelegramConfig {
            token: "t".into(),
            authorized_user_id: 1,
            default_target: None,
            require_mention_in_groups: false,
            response_timeout: 300,
        }
    }

    fn discord() -> DiscordConfig {
        DiscordConfig {
            token: "d".into(),
            authorized_user_id: "1".into(),
            default_target: None,
            channel_id: None,
            response_timeout: 300,
        }
    }

    fn cfg(telegram: Option<TelegramConfig>, discord: Option<DiscordConfig>) -> BridgeConfig {
        BridgeConfig {
            telegram,
            slack: None,
            discord,
            outbound_enabled: true,
            outbound_poll_secs: 15,
        }
    }

    #[test]
    fn inbound_claim_counts_every_configured_channel() {
        assert_eq!(configured_inbound_channels(&cfg(None, None)), 0);
        assert_eq!(configured_inbound_channels(&cfg(Some(telegram()), None)), 1);
        assert_eq!(
            configured_inbound_channels(&cfg(Some(telegram()), Some(discord()))),
            2
        );
    }

    /// THE regression test for the surviving half of the blind spot: a bridge
    /// whose ONLY chat channel task has exited kept reporting "● running".
    ///
    /// The process cannot be the signal: the outbound worker keeps it alive and
    /// keeps its liveness clock fresh forever, and `connected` still reads true
    /// from the handshake that happened before the channel died. So the death is
    /// asserted where it is now recorded: on the heartbeat, and through the
    /// health verdict the operator actually reads.
    #[test]
    fn a_channel_that_exits_degrades_the_health_row_and_names_the_inbound_half() {
        let home = tempfile::tempdir().expect("temp home");
        let heartbeat = BridgeHeartbeat::start_in(home.path());
        heartbeat.set_inbound_expected(configured_inbound_channels(&cfg(Some(telegram()), None)));
        // The gateway handshake succeeded, and the outbound worker is polling
        // happily: this is the row that used to read fully healthy.
        heartbeat.set_connected(true, Some("Telegram (@bot)".into()));
        heartbeat.record_attention_poll();

        let started = process_start_ms(std::process::id()).expect("self start readable");
        let live = DaemonHeartbeat::read_in(home.path(), "bridge").expect("heartbeat written");
        let now = live.last_heartbeat_at + 1_000;
        let healthy = classify_heartbeat(
            DaemonKind::Bridge,
            Some(DaemonHeartbeat {
                started_at: started,
                ..live
            }),
            PidCheck::Matched,
            now,
        );
        assert_eq!(
            healthy.state,
            DaemonState::Running,
            "sanity: with its channel alive the bridge is healthy: {}",
            healthy.reason
        );

        // The channel's `run` returns (terminal, because nothing restarts it).
        record_channel_exit(
            &heartbeat,
            "Telegram",
            Err(anyhow::anyhow!("building HTTP client failed")),
        );

        let dead = DaemonHeartbeat::read_in(home.path(), "bridge").expect("heartbeat written");
        assert_eq!(dead.inbound_expected, 1);
        assert_eq!(dead.inbound_live, 0);
        let status = classify_heartbeat(
            DaemonKind::Bridge,
            Some(DaemonHeartbeat {
                started_at: started,
                ..dead
            }),
            PidCheck::Matched,
            now,
        );
        assert_eq!(
            status.state,
            DaemonState::Degraded,
            "a bridge that can no longer hear the phone must not read healthy: {}",
            status.reason
        );
        assert!(
            status.reason.contains("INBOUND"),
            "the reason must name WHICH half died: {}",
            status.reason
        );
        assert!(
            status.reason.contains("building HTTP client failed"),
            "the reason must carry the exit cause: {}",
            status.reason
        );
        // The outbound half is untouched, and still reported separately.
        assert!(status.connected);
        assert!(status.last_attention_poll_at.is_some());
        assert!(status.last_attention_error.is_none());
    }

    #[test]
    fn a_channel_loop_that_just_ends_is_recorded_too() {
        // `Ok(())` from a channel `run` is not a clean shutdown, it is a loop
        // that stopped without saying why. Swallowing it would leave exactly the
        // silent-death hole this closes.
        let home = tempfile::tempdir().expect("temp home");
        let heartbeat = BridgeHeartbeat::start_in(home.path());
        heartbeat.set_inbound_expected(1);
        record_channel_exit(&heartbeat, "Slack", Ok(()));
        let hb = DaemonHeartbeat::read_in(home.path(), "bridge").expect("heartbeat written");
        assert_eq!(hb.inbound_live, 0);
        assert!(
            hb.last_inbound_error.as_deref().unwrap().contains("Slack"),
            "{:?}",
            hb.last_inbound_error
        );
    }
}
