// ABOUTME: `ainb fleet daemon` — long-running watcher with auto-continue on API errors.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Result;

use crate::cli::OutputFormat;
use crate::fleet::daemons::DaemonHeartbeat;
use crate::fleet::daemons::probe::DaemonKind;
use crate::fleet::discover::{discover_from_ainb, discover_from_peers, merge_sessions};
use crate::fleet::read::{capture_pane, detect_error_signals};
use crate::fleet::send::{broker_health, send};
use crate::fleet::types::Signal;

const SCAN_INTERVAL_SECS: u64 = 5;

pub async fn execute(matches: &clap::ArgMatches, _format: OutputFormat) -> Result<()> {
    let verbose = matches.get_flag("verbose");

    // Heartbeat: the fleet daemon had no observability either. Record a startup
    // record and refresh it every scan so the Daemons surface can show it
    // running + (when the broker is reachable) connected. The peer-registration
    // ("connected") signal is the broker health probe — the closest signal we
    // have until real `ainb-fleet-cp` registration lands.
    let mut heartbeat = DaemonHeartbeat::starting();
    let broker_up = broker_health().await;
    heartbeat.set_connected(
        broker_up,
        Some(if broker_up {
            "broker 127.0.0.1:7899".to_string()
        } else {
            "tmux-only (broker down)".to_string()
        }),
    );
    write_heartbeat(&heartbeat);

    if broker_up {
        // Phase 5 will wire actual broker_register/heartbeat for `ainb-fleet-cp`.
        eprintln!("[fleet/daemon] broker healthy at 127.0.0.1:7899");
    } else {
        eprintln!("[fleet/daemon] broker not reachable — operating in tmux-only mode");
    }

    let mut seen = HashSet::new();
    loop {
        match tick(&mut seen, verbose).await {
            // `acted` is true when the scan auto-continued at least one session
            // this tick — a real unit of work worth recording as activity.
            Ok(acted) => {
                if acted {
                    heartbeat.record_activity();
                } else {
                    heartbeat.touch();
                }
            }
            Err(e) => {
                eprintln!("[fleet/daemon] tick failed: {e}");
                heartbeat.record_error(e.to_string());
            }
        }
        write_heartbeat(&heartbeat);
        tokio::time::sleep(Duration::from_secs(SCAN_INTERVAL_SECS)).await;
    }
}

/// Best-effort heartbeat write — a failure is logged and swallowed so a transient
/// FS error never takes the watcher down.
fn write_heartbeat(heartbeat: &DaemonHeartbeat) {
    if let Err(e) = heartbeat.write(DaemonKind::FleetDaemon.id()) {
        eprintln!("[fleet/daemon] heartbeat write failed (continuing): {e}");
    }
}

/// The de-dup key for an auto-continue: `<session-id>::<pattern>::<raw-tail>`.
/// `seen` holds these so the same error on the same session is continued once.
/// Built here (not inline) so [`prune_seen`] can recover the session id from a
/// key without re-deriving the format in two places.
fn dedup_key(session_id: &str, pattern: &str, raw_tail: &str) -> String {
    format!("{session_id}::{pattern}::{raw_tail}")
}

/// Evict `seen` entries whose session is no longer present in the fleet, bounding
/// the set to the live sessions (LOW-6). Without this, `seen` grows unbounded for
/// the daemon's whole lifetime: every session that ever errored leaves its keys
/// behind forever. A key belongs to a live session iff its `<session-id>::`
/// prefix matches a currently-discovered session id.
fn prune_seen(seen: &mut HashSet<String>, live_ids: &HashSet<String>) {
    seen.retain(|key| key.split_once("::").is_some_and(|(id, _)| live_ids.contains(id)));
}

/// Run one scan. Returns `true` when at least one session was auto-continued
/// this tick (a real unit of work), `false` when the scan found nothing to do.
async fn tick(seen: &mut HashSet<String>, verbose: bool) -> Result<bool> {
    let (ainb, peers) = tokio::join!(discover_from_ainb(), async { discover_from_peers() });
    let merged = merge_sessions(vec![ainb.unwrap_or_default(), peers.unwrap_or_default()]);

    // Bound `seen`: drop keys for sessions that have vanished from the fleet so
    // the set tracks only live sessions, never the whole-lifetime history (LOW-6).
    let live_ids: HashSet<String> = merged.iter().map(|s| s.id.clone()).collect();
    prune_seen(seen, &live_ids);

    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut acted = false;
    for s in merged {
        let Some(name) = s.tmux_session.as_deref() else {
            continue;
        };
        let pane = capture_pane(name, 80).await.unwrap_or_default();
        let errors = detect_error_signals(&pane, now_ms);

        let Some(Signal::ApiError { pattern, raw, .. }) = errors.into_iter().next() else {
            continue;
        };
        let raw_tail: String =
            raw.chars().rev().take(40).collect::<String>().chars().rev().collect();
        let key = dedup_key(&s.id, &pattern, &raw_tail);
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);

        if verbose {
            eprintln!("[fleet/daemon] auto-continue -> {name} ({pattern})");
        }
        let _ = send(&s, "continue").await;
        acted = true;
    }
    Ok(acted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_seen_evicts_keys_for_vanished_sessions() {
        // LOW-6: a key whose session id is no longer live must be evicted, while
        // keys for still-present sessions survive — bounding the set.
        let mut seen: HashSet<String> = [
            dedup_key("alive", "api-error", "tail-a"),
            dedup_key("alive", "rate-limit", "tail-b"),
            dedup_key("gone", "api-error", "tail-c"),
        ]
        .into_iter()
        .collect();
        let live: HashSet<String> = ["alive".to_string()].into_iter().collect();
        prune_seen(&mut seen, &live);
        assert_eq!(seen.len(), 2, "only the two live-session keys survive");
        assert!(seen.contains(&dedup_key("alive", "api-error", "tail-a")));
        assert!(seen.contains(&dedup_key("alive", "rate-limit", "tail-b")));
        assert!(!seen.iter().any(|k| k.starts_with("gone::")));
    }

    #[test]
    fn prune_seen_empties_when_no_sessions_are_live() {
        let mut seen: HashSet<String> =
            [dedup_key("a", "p", "t"), dedup_key("b", "p", "t")].into_iter().collect();
        prune_seen(&mut seen, &HashSet::new());
        assert!(seen.is_empty(), "no live sessions → seen fully drained");
    }
}
