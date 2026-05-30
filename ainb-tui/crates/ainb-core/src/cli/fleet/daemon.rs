// ABOUTME: `ainb fleet daemon` — long-running watcher with auto-continue on API errors.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Result;

use crate::cli::OutputFormat;
use crate::fleet::discover::{discover_from_ainb, discover_from_peers, merge_sessions};
use crate::fleet::read::{capture_pane, detect_error_signals};
use crate::fleet::send::{broker_health, send};
use crate::fleet::types::Signal;

const SCAN_INTERVAL_SECS: u64 = 5;

pub async fn execute(matches: &clap::ArgMatches, _format: OutputFormat) -> Result<()> {
    let verbose = matches.get_flag("verbose");

    if broker_health().await {
        // Phase 5 will wire actual broker_register/heartbeat for `ainb-fleet-cp`.
        eprintln!("[fleet/daemon] broker healthy at 127.0.0.1:7899");
    } else {
        eprintln!("[fleet/daemon] broker not reachable — operating in tmux-only mode");
    }

    let mut seen = HashSet::new();
    loop {
        if let Err(e) = tick(&mut seen, verbose).await {
            eprintln!("[fleet/daemon] tick failed: {e}");
        }
        tokio::time::sleep(Duration::from_secs(SCAN_INTERVAL_SECS)).await;
    }
}

async fn tick(seen: &mut HashSet<String>, verbose: bool) -> Result<()> {
    let (ainb, peers) = tokio::join!(discover_from_ainb(), async { discover_from_peers() });
    let merged = merge_sessions(vec![
        ainb.unwrap_or_default(),
        peers.unwrap_or_default(),
    ]);

    let now_ms = chrono::Utc::now().timestamp_millis();
    for s in merged {
        let Some(name) = s.tmux_session.as_deref() else { continue };
        let pane = capture_pane(name, 80).await.unwrap_or_default();
        let errors = detect_error_signals(&pane, now_ms);

        let Some(Signal::ApiError { pattern, raw, .. }) = errors.into_iter().next() else {
            continue;
        };
        let raw_tail: String = raw.chars().rev().take(40).collect::<String>().chars().rev().collect();
        let key = format!("{}::{pattern}::{raw_tail}", s.id);
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);

        if verbose {
            eprintln!("[fleet/daemon] auto-continue -> {name} ({pattern})");
        }
        let _ = send(&s, "continue").await;
    }
    Ok(())
}
