// ABOUTME: Token-savings aggregation from three sources: Headroom proxy,
// RTK tool, and a caveman estimate from local output-token totals.
//
// Fetched asynchronously; stored in BurndownPlugin state and handed to
// the renderer via UsageViewState. All sources degrade gracefully —
// unavailable sources contribute 0 and are flagged as inactive.

use serde::Deserialize;
use std::time::Duration;

/// Caveman multiplier: fraction of output tokens estimated as savings.
/// 74 % is a modelled potential, NOT measured throughput. Rendered
/// explicitly as `(est)` so users understand it is an approximation.
pub const CAVEMAN_OUTPUT_RATIO: f64 = 0.74;

/// Aggregated savings figures fetched by the plugin.
#[derive(Debug, Clone, Default)]
pub struct SavingsData {
    /// Whether the Headroom proxy responded successfully.
    pub headroom_running: bool,
    /// `summary.tokens_saved_total` from Headroom /stats.
    pub headroom_tokens_saved: u64,
    /// Whether `rtk` was found on PATH and exited successfully.
    pub rtk_installed: bool,
    /// `summary.total_saved` from `rtk gain --all --format json`.
    pub rtk_total_saved: u64,
    /// Modelled estimate: `grand_total.output_tokens * CAVEMAN_OUTPUT_RATIO`.
    /// Always present; always marked as an estimate in the renderer.
    pub caveman_est: u64,
}

impl SavingsData {
    /// Net tokens saved across all real (non-estimated) sources.
    pub fn net_real(&self) -> u64 {
        self.headroom_tokens_saved + self.rtk_total_saved
    }
}

// ── Headroom /stats response shape ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct HeadroomStats {
    pub summary: HeadroomSummary,
}

#[derive(Debug, Deserialize)]
pub struct HeadroomSummary {
    pub tokens_saved_total: u64,
    #[allow(dead_code)]
    pub requests_total: u64,
}

// ── RTK gain response shape ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RtkGain {
    pub summary: RtkSummary,
}

#[derive(Debug, Deserialize)]
pub struct RtkSummary {
    pub total_saved: u64,
    #[allow(dead_code)]
    pub avg_savings_pct: f64,
}

// ── Fetch helpers ─────────────────────────────────────────────────────────────

/// Fetch token-savings figures from all three sources and return a
/// [`SavingsData`] snapshot. Safe to call from an async context —
/// both I/O operations are awaited, not blocked.
///
/// `output_tokens` is the `grand_total.output_tokens` from the latest
/// [`UsageData`] snapshot; used to compute `caveman_est`.
pub async fn fetch_savings(output_tokens: u64) -> SavingsData {
    let caveman_est = (output_tokens as f64 * CAVEMAN_OUTPUT_RATIO).round() as u64;

    let (headroom_running, headroom_tokens_saved) = fetch_headroom().await;
    let (rtk_installed, rtk_total_saved) = fetch_rtk().await;

    SavingsData {
        headroom_running,
        headroom_tokens_saved,
        rtk_installed,
        rtk_total_saved,
        caveman_est,
    }
}

/// GET `http://127.0.0.1:<port>/stats` where port is `AINB_HEADROOM_PORT`
/// or 8787. Returns `(true, tokens_saved_total)` on success, `(false, 0)`
/// on any error (proxy down, parse failure, network issue).
async fn fetch_headroom() -> (bool, u64) {
    let port = std::env::var("AINB_HEADROOM_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(8787);

    let url = format!("http://127.0.0.1:{port}/stats");
    let client = match reqwest::Client::builder().timeout(Duration::from_millis(500)).build() {
        Ok(c) => c,
        Err(_) => return (false, 0),
    };

    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<HeadroomStats>().await {
            Ok(stats) => (true, stats.summary.tokens_saved_total),
            Err(_) => (false, 0),
        },
        _ => (false, 0),
    }
}

/// Run `rtk gain --all --format json`. Returns `(true, total_saved)` when
/// `rtk` is on PATH and exits successfully, `(false, 0)` otherwise.
async fn fetch_rtk() -> (bool, u64) {
    use tokio::process::Command;

    let output =
        match Command::new("rtk").args(["gain", "--all", "--format", "json"]).output().await {
            Ok(o) => o,
            Err(_) => return (false, 0), // rtk not on PATH
        };

    if !output.status.success() {
        return (false, 0);
    }

    match serde_json::from_slice::<RtkGain>(&output.stdout) {
        Ok(gain) => (true, gain.summary.total_saved),
        Err(_) => (false, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headroom_stats_deserialises_from_sample_json() {
        let json = r#"{"summary":{"tokens_saved_total":42000,"requests_total":120}}"#;
        let stats: HeadroomStats = serde_json::from_str(json).expect("parse HeadroomStats");
        assert_eq!(stats.summary.tokens_saved_total, 42000);
        assert_eq!(stats.summary.requests_total, 120);
    }

    #[test]
    fn rtk_gain_deserialises_from_sample_json() {
        let json = r#"{"summary":{"total_saved":8000,"avg_savings_pct":18.5}}"#;
        let gain: RtkGain = serde_json::from_str(json).expect("parse RtkGain");
        assert_eq!(gain.summary.total_saved, 8000);
    }

    #[test]
    fn caveman_estimate_rounds_correctly() {
        // 1000 output tokens * 0.74 = 740
        let est = (1000_f64 * CAVEMAN_OUTPUT_RATIO).round() as u64;
        assert_eq!(est, 740);
    }

    #[test]
    fn caveman_estimate_zero_for_zero_output_tokens() {
        let est = (0_f64 * CAVEMAN_OUTPUT_RATIO).round() as u64;
        assert_eq!(est, 0);
    }

    #[test]
    fn savings_data_net_real_sums_headroom_and_rtk() {
        let sd = SavingsData {
            headroom_running: true,
            headroom_tokens_saved: 10_000,
            rtk_installed: true,
            rtk_total_saved: 5_000,
            caveman_est: 7_000,
        };
        assert_eq!(sd.net_real(), 15_000);
    }

    #[test]
    fn savings_data_net_real_excludes_caveman() {
        // caveman is an estimate; it must not inflate net_real
        let sd = SavingsData {
            headroom_running: false,
            headroom_tokens_saved: 0,
            rtk_installed: false,
            rtk_total_saved: 0,
            caveman_est: 999_999,
        };
        assert_eq!(sd.net_real(), 0);
    }
}
