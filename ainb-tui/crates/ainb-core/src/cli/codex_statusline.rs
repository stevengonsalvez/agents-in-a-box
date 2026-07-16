// ABOUTME: `ainb codex statusline` subcommand — pull Codex OAuth quota
// (5h + weekly) and cache it for the ainb TUI top bar.
//
// Why this exists (and why it is NOT symmetric with `claudecode
// statusline`):
//
//   Claude Code PUSHES its rate-limit windows to `ainb claudecode
//   statusline` on stdin every prompt render (zero network — see
//   `cli::statusline`). Codex exposes nothing to a render-time
//   subprocess: the local rollout JSONL has `rate_limits: null` in
//   exec/CLI mode (openai/codex#14880). The only source of Codex quota
//   is the network endpoint, so Codex must be PULLED:
//
//     GET https://chatgpt.com/backend-api/wham/usage
//       Authorization: Bearer <tokens.access_token from ~/.codex/auth.json>
//       chatgpt-account-id: <tokens.account_id>
//       originator: codex_cli_rs                 (else Cloudflare 403s)
//
//   → `rate_limit.{primary_window,secondary_window}.{used_percent,
//      reset_at,limit_window_seconds}` where `reset_at` is an absolute
//      Unix epoch. The 5h vs weekly window is identified by
//      `limit_window_seconds` (18000 vs 604800), NOT by primary/secondary
//      position: some tiers (e.g. prolite) return a weekly-only window in
//      `primary_window` with `secondary_window: null`. See `parse_usage`.
//
// Design constraints:
//   - This is the single pull command; both drivers (the Codex `stop`
//     hook and the TUI background poller) invoke it. The render path
//     never calls the network — it only reads the cache this writes.
//   - Hide-on-fail: no `~/.codex/auth.json` / 401 / network / parse
//     error → the cache's windows are cleared so the TUI renders no Codex
//     segment. Never panics; `execute` always returns `Ok`.
//   - Throttled: a `codex-live.json` written < TTL ago short-circuits the
//     network call so the hook and poller don't double-fetch and a
//     logged-out state doesn't hammer the endpoint every tick.
//
// The Codex cache is a SEPARATE file from the Claude `live.json`. Both
// writers do full-file rewrites; a shared file would mean each writer has
// to read-before-write to avoid clobbering the other's fields. Separate
// files keep the Claude render path completely untouched.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::cli::statusline::{RateWindow, parse_resets_at, round_pct};

/// Codex usage endpoint. The `/api/codex/usage` path from earlier notes
/// 403s behind Cloudflare; `/backend-api/wham/usage` is the path the
/// `codex` CLI actually hits.
const WHAM_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

/// Required by the backend — without it the request is served a Cloudflare
/// 403 challenge page instead of JSON.
const CODEX_ORIGINATOR: &str = "codex_cli_rs";

/// Network timeout for the usage pull. Short — this runs off the render
/// path but is still driven by interactive events (hook on `stop`, TUI
/// poller tick), so it must never hang.
const CODEX_HTTP_TIMEOUT_SECS: u64 = 4;

/// Default throttle: skip the network call if the cache was written less
/// than this many seconds ago. Override via `AINB_CODEX_USAGE_TTL_SECS`.
pub const CODEX_USAGE_TTL_SECS: u64 = 60;

/// Schema version for `codex-live.json`.
pub const CODEX_CACHE_SCHEMA_VERSION: u32 = 1;

/// On-disk cache for Codex usage, sibling of the Claude `live.json`.
///
/// Lives in the same OS cache dir. `updated_at` is stamped on every pull
/// attempt — success OR failure — so the throttle applies even when Codex
/// is logged out (otherwise a logged-out TUI would hammer the endpoint
/// every tick).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CodexLiveCache {
    pub version: u32,
    /// RFC3339 timestamp of the last pull attempt (success or failure).
    pub updated_at: String,
    /// Codex 5-hour (primary) window. `None` when logged out / the pull
    /// failed — the TUI hides the segment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<RateWindow>,
    /// Codex weekly (secondary) window. See [`Self::five_hour`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day: Option<RateWindow>,
    /// Codex plan tier (e.g. `prolite`), when reported. Display-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
}

/// Resolve `<cache-dir>/ainb/codex-live.json`. `None` only when there is
/// no cache or home dir (sandbox without HOME).
pub fn codex_cache_path() -> Option<PathBuf> {
    let dir = dirs::cache_dir().or_else(|| dirs::home_dir().map(|h| h.join(".cache")))?;
    Some(dir.join("ainb").join("codex-live.json"))
}

/// Effective throttle TTL (env override → default).
fn ttl_secs() -> u64 {
    std::env::var("AINB_CODEX_USAGE_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(CODEX_USAGE_TTL_SECS)
}

// ---- wham/usage response shape (only the fields we read) ----

#[derive(Debug, Deserialize)]
struct WhamUsage {
    #[serde(default)]
    plan_type: Option<String>,
    #[serde(default)]
    rate_limit: Option<WhamRateLimit>,
}

#[derive(Debug, Deserialize)]
struct WhamRateLimit {
    #[serde(default)]
    primary_window: Option<WhamWindow>,
    #[serde(default)]
    secondary_window: Option<WhamWindow>,
}

#[derive(Debug, Deserialize)]
struct WhamWindow {
    #[serde(default)]
    used_percent: Option<f64>,
    /// Absolute Unix epoch (seconds) the window resets. Absent on some
    /// payloads → the TUI just omits the `↻ <reset>` affordance.
    #[serde(default)]
    reset_at: Option<i64>,
    /// Length of the rate-limit window in seconds (`18000` = 5h,
    /// `604800` = weekly). This is what tells the 5h window apart from the
    /// weekly one — the position in the payload does NOT (see `parse_usage`).
    #[serde(default)]
    limit_window_seconds: Option<i64>,
}

impl WhamWindow {
    /// Convert one window into a `RateWindow` (the shared cache shape).
    /// `None` when the window carries no usage percentage.
    fn to_rate_window(&self) -> Option<RateWindow> {
        let pct = round_pct(self.used_percent?);
        let resets_at =
            self.reset_at.and_then(|epoch| parse_resets_at(&serde_json::Value::from(epoch)));
        Some(RateWindow { pct, resets_at })
    }
}

/// Windows at or above this length (seconds) are the weekly bucket;
/// shorter ones are the 5h bucket. Splits `18000` (5h) from `604800`
/// (weekly) with a full day of slack on either side.
const WEEKLY_WINDOW_MIN_SECONDS: i64 = 86_400;

/// Parse a `wham/usage` response body into a populated `CodexLiveCache`.
///
/// Windows are routed by `limit_window_seconds`, NOT by position: the
/// endpoint moves the sole window between `primary_window` and
/// `secondary_window` across plan tiers (e.g. `prolite` returns a
/// weekly-only window in `primary_window` with `secondary_window: null`),
/// so a positional map mislabels the weekly window as 5h. A window whose
/// length is `>= WEEKLY_WINDOW_MIN_SECONDS` fills the weekly slot, else the
/// 5h slot; when `limit_window_seconds` is absent we fall back to position
/// (primary → 5h, secondary → weekly).
///
/// Returns `Err` only when the body is not valid JSON; a well-formed body
/// that simply lacks `rate_limit` yields a cache with both windows `None`
/// (a valid "logged in but no quota data" state).
///
/// `now_rfc3339` is injected so tests are deterministic; production passes
/// `chrono::Utc::now().to_rfc3339()`.
pub fn parse_usage(body: &str, now_rfc3339: String) -> Result<CodexLiveCache> {
    let usage: WhamUsage = serde_json::from_str(body)?;
    let mut five_hour = None;
    let mut seven_day = None;
    if let Some(rl) = usage.rate_limit {
        for (window, is_primary) in [(rl.primary_window, true), (rl.secondary_window, false)] {
            let Some(w) = window else { continue };
            let is_weekly = match w.limit_window_seconds {
                Some(secs) => secs >= WEEKLY_WINDOW_MIN_SECONDS,
                None => !is_primary,
            };
            let Some(rw) = w.to_rate_window() else {
                continue;
            };
            let slot = if is_weekly {
                &mut seven_day
            } else {
                &mut five_hour
            };
            // First window to claim a slot keeps it: if the API ever put two
            // windows in the same bucket, the earlier (primary) one wins
            // rather than being overwritten by the later one.
            slot.get_or_insert(rw);
        }
    }
    Ok(CodexLiveCache {
        version: CODEX_CACHE_SCHEMA_VERSION,
        updated_at: now_rfc3339,
        five_hour,
        seven_day,
        plan_type: usage.plan_type,
    })
}

/// A "hidden" cache: a fresh stamp with no windows. Written on any failure
/// so the TUI hides the Codex segment AND the throttle applies (no hammer
/// loop when logged out).
fn empty_cache(now_rfc3339: String) -> CodexLiveCache {
    CodexLiveCache {
        version: CODEX_CACHE_SCHEMA_VERSION,
        updated_at: now_rfc3339,
        ..Default::default()
    }
}

/// Whether a cache stamped `updated_at` is older than `ttl` seconds (or
/// has an unparseable / future stamp — both treated as stale so we refresh).
pub fn is_stale(updated_at_rfc3339: &str, ttl: u64, now: chrono::DateTime<chrono::Utc>) -> bool {
    // Unparseable stamp → stale (refresh). For a valid stamp, a negative
    // age (future stamp / clock skew) is also treated as stale.
    chrono::DateTime::parse_from_rfc3339(updated_at_rfc3339).map_or(true, |t| {
        let secs = now.signed_duration_since(t.with_timezone(&chrono::Utc)).num_seconds();
        secs < 0 || secs.unsigned_abs() >= ttl
    })
}

// ---- IO ----

/// Read the Codex cache if present and schema-current.
pub fn read_codex_cache(path: &std::path::Path) -> Option<CodexLiveCache> {
    let bytes = std::fs::read(path).ok()?;
    let cache: CodexLiveCache = serde_json::from_slice(&bytes).ok()?;
    if cache.version != CODEX_CACHE_SCHEMA_VERSION {
        return None;
    }
    Some(cache)
}

/// Atomically write the Codex cache (write-tmp + rename). Creates the
/// parent dir if needed.
pub fn write_codex_cache(path: &std::path::Path, cache: &CodexLiveCache) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        match std::fs::create_dir_all(parent) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let json = serde_json::to_vec_pretty(cache)?;
    // Per-process tmp name so concurrent writers — the Codex `stop` hook
    // (a separate process) and the TUI poller, or two TUI instances —
    // never share one tmp inode. A shared tmp defeats the write-tmp+rename
    // atomicity: two writers would interleave bytes and one's rename would
    // expose the other's half-written file to a concurrent reader.
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&json)?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ---- auth ----

#[derive(Debug, Deserialize)]
struct CodexAuth {
    #[serde(default)]
    tokens: Option<CodexTokens>,
}

#[derive(Debug, Deserialize)]
struct CodexTokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

/// Read `~/.codex/auth.json` → `(access_token, account_id)`. `None` when
/// the file is missing / unreadable / has no access token (i.e. Codex is
/// not logged in) — the caller treats that as hide-on-fail.
fn read_codex_auth() -> Option<(String, Option<String>)> {
    let path = dirs::home_dir()?.join(".codex").join("auth.json");
    let bytes = std::fs::read(path).ok()?;
    let auth: CodexAuth = serde_json::from_slice(&bytes).ok()?;
    let tokens = auth.tokens?;
    let token = tokens.access_token.filter(|t| !t.is_empty())?;
    Some((token, tokens.account_id))
}

/// Pull the usage endpoint and return the raw response body. Errors on
/// missing auth, non-2xx, or transport failure.
async fn fetch_usage(token: &str, account_id: Option<&str>) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(CODEX_HTTP_TIMEOUT_SECS))
        .build()?;
    let mut req = client
        .get(WHAM_USAGE_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("originator", CODEX_ORIGINATOR)
        .header("Accept", "application/json");
    if let Some(acct) = account_id {
        req = req.header("chatgpt-account-id", acct);
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("codex usage HTTP {status}");
    }
    Ok(resp.text().await?)
}

/// Read auth, fetch, parse, and return the populated cache. Any failure in
/// the chain surfaces as `Err` so `execute` can write the hidden cache.
async fn refresh() -> Result<CodexLiveCache> {
    let (token, account_id) = read_codex_auth()
        .ok_or_else(|| anyhow::anyhow!("codex not logged in (~/.codex/auth.json)"))?;
    let body = fetch_usage(&token, account_id.as_deref()).await?;
    parse_usage(&body, chrono::Utc::now().to_rfc3339())
}

/// Subcommand entry point: throttle → pull → merge-write.
///
/// Hide-on-fail: every error path writes the hidden cache and returns
/// `Ok`, so the command never breaks the caller (hook / poller) and never
/// panics.
///
/// `force` bypasses the throttle (used by `--force` for testing / manual
/// refresh).
pub async fn execute(force: bool) -> Result<()> {
    let Some(path) = codex_cache_path() else {
        return Ok(());
    };

    if !force {
        if let Some(existing) = read_codex_cache(&path) {
            if !is_stale(&existing.updated_at, ttl_secs(), chrono::Utc::now()) {
                return Ok(());
            }
        }
    }

    match refresh().await {
        Ok(cache) => {
            if let Err(e) = write_codex_cache(&path, &cache) {
                tracing::warn!(error = %e, "codex statusline: cache write failed");
            }
        }
        Err(e) => {
            // Hide-on-fail: clear windows, keep a fresh stamp so the TUI
            // hides the segment and the throttle still applies.
            tracing::debug!(error = %e, "codex statusline: usage pull failed; hiding segment");
            let _ = write_codex_cache(&path, &empty_cache(chrono::Utc::now().to_rfc3339()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// The real `wham/usage` body shape, captured from a live pull
    /// (percentages/epochs only — no PII). Regression anchor for the
    /// field names: `rate_limit.{primary,secondary}_window.{used_percent,
    /// reset_at}`, NOT the `primary`/`secondary` shape from older notes.
    const REAL_BODY: &str = r#"{
        "account_id": "REDACTED",
        "email": "REDACTED",
        "plan_type": "prolite",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {
                "used_percent": 10,
                "limit_window_seconds": 18000,
                "reset_after_seconds": 6117,
                "reset_at": 1781482874
            },
            "secondary_window": {
                "used_percent": 44,
                "limit_window_seconds": 604800,
                "reset_after_seconds": 310855,
                "reset_at": 1781787612
            }
        },
        "code_review_rate_limit": null
    }"#;

    #[test]
    fn parse_usage_maps_primary_to_5h_secondary_to_weekly() {
        let c = parse_usage(REAL_BODY, "2026-06-14T00:00:00Z".to_string()).expect("parses");
        assert_eq!(c.version, CODEX_CACHE_SCHEMA_VERSION);
        assert_eq!(c.plan_type.as_deref(), Some("prolite"));
        let fh = c.five_hour.expect("5h present");
        assert_eq!(fh.pct, 10);
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(fh.resets_at.as_ref().unwrap())
                .unwrap()
                .timestamp(),
            1781482874
        );
        let wk = c.seven_day.expect("weekly present");
        assert_eq!(wk.pct, 44);
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(wk.resets_at.as_ref().unwrap())
                .unwrap()
                .timestamp(),
            1781787612
        );
    }

    #[test]
    fn parse_usage_rounds_fractional_percent() {
        let body =
            r#"{"rate_limit":{"primary_window":{"used_percent":12.6,"reset_at":1781482874}}}"#;
        let c = parse_usage(body, "now".to_string()).expect("parses");
        assert_eq!(c.five_hour.unwrap().pct, 13);
        assert!(c.seven_day.is_none(), "no secondary → weekly None");
    }

    #[test]
    fn parse_usage_window_without_reset_at_omits_reset() {
        let body = r#"{"rate_limit":{"primary_window":{"used_percent":5}}}"#;
        let c = parse_usage(body, "now".to_string()).expect("parses");
        let fh = c.five_hour.unwrap();
        assert_eq!(fh.pct, 5);
        assert!(fh.resets_at.is_none());
    }

    #[test]
    fn parse_usage_no_rate_limit_yields_empty_windows() {
        // Well-formed body, logged in, but no quota block → both None.
        let c = parse_usage(r#"{"plan_type":"prolite"}"#, "now".to_string()).expect("parses");
        assert!(c.five_hour.is_none());
        assert!(c.seven_day.is_none());
        assert_eq!(c.plan_type.as_deref(), Some("prolite"));
    }

    #[test]
    fn parse_usage_malformed_json_errors() {
        assert!(parse_usage("not json", "now".to_string()).is_err());
        assert!(parse_usage("", "now".to_string()).is_err());
    }

    #[test]
    fn empty_cache_has_no_windows_but_fresh_stamp() {
        let c = empty_cache("2026-06-14T00:00:00Z".to_string());
        assert!(c.five_hour.is_none());
        assert!(c.seven_day.is_none());
        assert!(c.plan_type.is_none());
        assert_eq!(c.updated_at, "2026-06-14T00:00:00Z");
        assert_eq!(c.version, CODEX_CACHE_SCHEMA_VERSION);
    }

    #[test]
    fn is_stale_thresholds() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-14T00:01:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        // 30s old, ttl 60 → fresh
        assert!(!is_stale("2026-06-14T00:00:30Z", 60, now));
        // 60s old, ttl 60 → stale (>=)
        assert!(is_stale("2026-06-14T00:00:00Z", 60, now));
        // future stamp → stale (clock skew guard)
        assert!(is_stale("2026-06-14T00:05:00Z", 60, now));
        // garbage → stale
        assert!(is_stale("not-a-date", 60, now));
    }

    #[test]
    fn cache_write_read_roundtrips_and_is_atomic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("codex-live.json");
        let cache = parse_usage(REAL_BODY, "2026-06-14T00:00:00Z".to_string()).unwrap();
        write_codex_cache(&path, &cache).unwrap();
        assert!(
            !path.with_extension(format!("json.tmp.{}", std::process::id())).exists(),
            "tmp renamed away"
        );
        assert_eq!(read_codex_cache(&path).unwrap(), cache);
    }

    #[test]
    fn read_cache_rejects_wrong_schema_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("codex-live.json");
        let mut bad = empty_cache("2026-06-14T00:00:00Z".to_string());
        bad.version = 999;
        std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(read_codex_cache(&path).is_none());
    }

    #[test]
    fn read_cache_none_for_missing_file() {
        let dir = tempdir().unwrap();
        assert!(read_codex_cache(&dir.path().join("nope.json")).is_none());
    }
}
