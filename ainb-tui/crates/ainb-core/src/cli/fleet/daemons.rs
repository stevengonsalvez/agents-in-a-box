// ABOUTME: `ainb fleet daemons [--format text|json]` — the unified daemon health view.
//
// Renders the typed `Vec<DaemonStatus>` from `fleet::daemons::collect` — the
// SAME aggregator the TUI Daemons screen uses, so the two surfaces can never
// drift. Text output is a fixed-width table (one row per daemon); `--format
// json` emits the typed rows verbatim for scripting.
//
// This is the runtime-health answer the bridge's install-only status could
// never give: a running, Telegram-connected bridge shows "running + connected"
// while a crashed one shows "stopped" with a stale-heartbeat reason.

use anyhow::Result;

use crate::cli::OutputFormat;
use crate::fleet::daemons::{DaemonState, DaemonStatus, collect};

pub async fn execute(_matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let rows = collect()?;
    let now_ms = crate::fleet::daemons::heartbeat::now_ms();
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        _ => {
            print!("{}", render_text(&rows, now_ms));
        }
    }
    Ok(())
}

/// Render the daemon rows as a fixed-width text table. `now_ms` is the clock the
/// relative-time columns (uptime / last-activity) are measured against — passed
/// in so the rendering is deterministic under test.
#[must_use]
pub fn render_text(rows: &[DaemonStatus], now_ms: i64) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<14} {:<9} {:<8} {:<10} {:<12} {:<7} {}\n",
        "DAEMON", "STATE", "PID", "UPTIME", "LAST ACTIVITY", "ERRORS", "HEALTH"
    ));
    out.push_str(&format!("{}\n", "-".repeat(86)));
    for r in rows {
        let state = state_glyph(r.state);
        let pid = r.pid.map_or_else(|| "-".to_string(), |p| p.to_string());
        let uptime = r.uptime_ms.map_or_else(|| "-".to_string(), fmt_duration_ms);
        let last_activity =
            r.last_activity_at.map_or_else(|| "-".to_string(), |ts| fmt_ago(now_ms, ts));
        let health = health_summary(r);
        out.push_str(&format!(
            "{:<14} {:<9} {:<8} {:<10} {:<12} {:<7} {}\n",
            r.kind.display_name(),
            state,
            pid,
            uptime,
            last_activity,
            r.error_count,
            health,
        ));
    }
    out
}

/// A glyphed state token for the text table.
fn state_glyph(state: DaemonState) -> String {
    match state {
        DaemonState::Running => "● running".to_string(),
        DaemonState::Stopped => "○ stopped".to_string(),
        DaemonState::Unknown => "? unknown".to_string(),
    }
}

/// The HEALTH column: connection label + the reason. For a running+connected
/// daemon this is the channel; otherwise it's the reason (e.g. the stale-
/// heartbeat explanation) so a crash is legible at a glance.
fn health_summary(r: &DaemonStatus) -> String {
    match (r.state, &r.channel) {
        (DaemonState::Running, Some(ch)) if r.connected => format!("{ch} — {}", r.reason),
        (DaemonState::Running, _) => r.reason.clone(),
        _ => r.reason.clone(),
    }
}

/// Format an elapsed-millis duration compactly: `5s`, `12m`, `3h`, `2d`.
#[must_use]
pub fn fmt_duration_ms(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Format `ts` (epoch ms) relative to `now_ms`: `5s ago`, `12m ago`, `just now`.
#[must_use]
pub fn fmt_ago(now_ms: i64, ts_ms: i64) -> String {
    let delta = now_ms.saturating_sub(ts_ms);
    if delta < 1000 {
        return "just now".to_string();
    }
    format!("{} ago", fmt_duration_ms(delta))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::daemons::probe::DaemonKind;

    fn row(
        kind: DaemonKind,
        state: DaemonState,
        connected: bool,
        channel: Option<&str>,
    ) -> DaemonStatus {
        DaemonStatus {
            kind,
            state,
            pid: Some(4242),
            uptime_ms: Some(125_000),
            connected,
            channel: channel.map(str::to_string),
            last_activity_at: Some(0),
            error_count: 0,
            last_error: None,
            reason: "running + connected".to_string(),
        }
    }

    #[test]
    fn fmt_duration_buckets() {
        assert_eq!(fmt_duration_ms(5_000), "5s");
        assert_eq!(fmt_duration_ms(125_000), "2m");
        assert_eq!(fmt_duration_ms(7_200_000), "2h");
        assert_eq!(fmt_duration_ms(172_800_000), "2d");
        assert_eq!(fmt_duration_ms(-5), "0s");
    }

    #[test]
    fn fmt_ago_handles_just_now_and_past() {
        assert_eq!(fmt_ago(1_000_000, 1_000_000), "just now");
        assert_eq!(fmt_ago(1_000_000, 1_000_000 - 5_000), "5s ago");
        assert_eq!(fmt_ago(1_000_000, 1_000_000 - 120_000), "2m ago");
    }

    #[test]
    fn render_text_has_header_and_one_row_per_daemon() {
        let now = 1_000_000;
        let rows = vec![
            row(
                DaemonKind::Bridge,
                DaemonState::Running,
                true,
                Some("Telegram (@bot)"),
            ),
            row(DaemonKind::Notifyd, DaemonState::Stopped, false, None),
        ];
        let txt = render_text(&rows, now);
        assert!(txt.contains("DAEMON"));
        assert!(txt.contains("STATE"));
        assert!(txt.contains("phone bridge"));
        assert!(txt.contains("notifyd"));
        // Running shows the channel; the row carries the running glyph.
        assert!(txt.contains("Telegram (@bot)"));
        assert!(txt.contains("● running"));
        assert!(txt.contains("○ stopped"));
        // Exactly header(2 lines) + 2 data rows.
        assert_eq!(txt.lines().count(), 4);
    }

    #[test]
    fn render_text_shows_stale_reason_for_crashed_daemon() {
        let now = 1_000_000;
        let mut r = row(
            DaemonKind::Bridge,
            DaemonState::Stopped,
            false,
            Some("Telegram"),
        );
        r.reason = "stale heartbeat — pid 4242 not alive (crashed)".to_string();
        let txt = render_text(&[r], now);
        assert!(
            txt.contains("crashed"),
            "crash reason must be visible: {txt}"
        );
    }
}
