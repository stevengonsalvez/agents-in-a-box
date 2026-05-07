// ABOUTME: `ainb statusline` subcommand for the Claude Code statusline hook.
//
// Reads JSON on stdin (the statusline payload Claude Code feeds to its
// statusLine.command), persists the rate-limit window data to a cache file
// in the OS-specific cache dir (via `dirs::cache_dir()`, e.g.
// `~/.cache/ainb/live.json` on Linux, `~/Library/Caches/ainb/live.json`
// on macOS), and emits a single-line powerline-styled status string on
// stdout for the user's prompt.
//
// Design constraints (the statusline runs on every prompt render):
//   - Must NEVER panic. All fallible operations swallow errors and emit
//     either a minimal status line or an empty line.
//   - Must be cheap. No network, no heavy parsing.
//   - Cache writes are atomic (write-tmp + rename) so a killed process
//     can never leave a half-written file.
//
// The cache schema is intentionally narrow and uses `serde_json::Value`
// on input so we don't break when Claude Code adds new fields.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;

/// Schema version for the live cache file (resolved via
/// [`cache_path`] — OS-specific cache dir).
pub const CACHE_SCHEMA_VERSION: u32 = 1;

/// One side of a rate-limit window (used twice: 5h + 7d).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RateWindow {
    /// Used percentage, 0..=100.
    pub pct: u8,
    /// ISO8601 reset timestamp as reported by Claude Code (passed through).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

/// On-disk cache schema for the live cache file (resolved via
/// [`cache_path`] — OS-specific cache dir, e.g. `~/.cache/ainb/live.json`
/// on Linux, `~/Library/Caches/ainb/live.json` on macOS).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LiveCache {
    pub version: u32,
    /// ISO8601 timestamp of when this cache entry was written.
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<RateWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day: Option<RateWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub today_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Resolve the OS-specific cache file path (via `dirs::cache_dir()`,
/// e.g. `~/.cache/ainb/live.json` on Linux, `~/Library/Caches/ainb/live.json`
/// on macOS). Returns `None` if no cache dir is available (mostly a
/// guard for sandbox environments without a HOME).
pub fn cache_path() -> Option<PathBuf> {
    let dir = dirs::cache_dir().or_else(|| dirs::home_dir().map(|h| h.join(".cache")))?;
    Some(dir.join("ainb").join("live.json"))
}

/// Parse a Claude Code statusline payload from raw stdin bytes into a
/// `LiveCache`. Returns `None` if the payload is empty or not valid JSON
/// (the statusline must degrade gracefully — see module docs).
pub fn parse_payload(raw: &[u8]) -> Option<LiveCache> {
    let text = std::str::from_utf8(raw).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    Some(payload_to_cache(&value))
}

fn payload_to_cache(value: &serde_json::Value) -> LiveCache {
    let five_hour = value.pointer("/rate_limits/five_hour").and_then(rate_window_from_value);
    let seven_day = value.pointer("/rate_limits/seven_day").and_then(rate_window_from_value);
    let today_cost_usd = value.pointer("/cost/total_cost_usd").and_then(serde_json::Value::as_f64);
    let context_pct = value
        .pointer("/context_window/used_percentage")
        .and_then(serde_json::Value::as_f64)
        .map(round_pct);
    let model = value
        .pointer("/model/display_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    LiveCache {
        version: CACHE_SCHEMA_VERSION,
        updated_at: chrono::Utc::now().to_rfc3339(),
        five_hour,
        seven_day,
        today_cost_usd,
        context_pct,
        model,
    }
}

fn rate_window_from_value(value: &serde_json::Value) -> Option<RateWindow> {
    let pct = value
        .get("used_percentage")
        .and_then(serde_json::Value::as_f64)
        .map(round_pct)?;
    let resets_at = value.get("resets_at").and_then(serde_json::Value::as_str).map(str::to_string);
    Some(RateWindow { pct, resets_at })
}

fn round_pct(p: f64) -> u8 {
    p.clamp(0.0, 100.0).round() as u8
}

/// Atomically write a `LiveCache` to `path` (via `<path>.tmp` + rename).
/// Creates the parent directory if needed. Best-effort: returns `Err` on
/// IO failure but the caller is expected to swallow.
pub fn write_cache(path: &std::path::Path, cache: &LiveCache) -> Result<()> {
    if let Some(parent) = path.parent() {
        match std::fs::create_dir_all(parent) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let json = serde_json::to_vec_pretty(cache)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&json)?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Read a `LiveCache` from disk if present and parseable. Returns `None`
/// for missing files, IO errors, malformed JSON, or version mismatches.
pub fn read_cache(path: &std::path::Path) -> Option<LiveCache> {
    let bytes = std::fs::read(path).ok()?;
    let cache: LiveCache = serde_json::from_slice(&bytes).ok()?;
    if cache.version != CACHE_SCHEMA_VERSION {
        return None;
    }
    Some(cache)
}

/// Render the powerline-styled status string. Hand-rolled ANSI escapes
/// keep this dependency-free. The format is intentionally plain (no
/// nerd-font glyphs) so it works in any terminal.
pub fn render_powerline(cache: &LiveCache) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(model) = &cache.model {
        parts.push(ansi_fg(SOFT_WHITE, model));
    }

    if let Some(ctx) = cache.context_pct {
        parts.push(format!(
            "{} {}%",
            ansi_fg(MUTED, "ctx"),
            ansi_fg(pct_color(ctx, 60, 85), &ctx.to_string())
        ));
    }

    if let Some(rw) = &cache.five_hour {
        parts.push(format!(
            "{} {}%",
            ansi_fg(MUTED, "5h"),
            ansi_fg(pct_color(rw.pct, 60, 85), &rw.pct.to_string())
        ));
    }

    if let Some(rw) = &cache.seven_day {
        parts.push(format!(
            "{} {}%",
            ansi_fg(MUTED, "wk"),
            ansi_fg(pct_color(rw.pct, 70, 90), &rw.pct.to_string())
        ));
    }

    if let Some(cost) = cache.today_cost_usd {
        parts.push(ansi_fg(GOLD, &format!("${cost:.2}")));
    }

    parts.join(&format!(" {} ", ansi_fg(MUTED, "·")))
}

// Hand-rolled ANSI 24-bit escape sequences. Avoid pulling in a colored
// crate just for the few escapes the powerline needs.
const RESET: &str = "\x1b[0m";
const SOFT_WHITE: (u8, u8, u8) = (220, 220, 230);
const MUTED: (u8, u8, u8) = (120, 120, 140);
const GOLD: (u8, u8, u8) = (255, 215, 0);
const GREEN: (u8, u8, u8) = (100, 200, 100);
const AMBER: (u8, u8, u8) = (255, 165, 0);
const RED: (u8, u8, u8) = (230, 100, 100);

fn ansi_fg(rgb: (u8, u8, u8), text: &str) -> String {
    format!("\x1b[38;2;{};{};{}m{}{}", rgb.0, rgb.1, rgb.2, text, RESET)
}

fn pct_color(pct: u8, amber_at: u8, red_at: u8) -> (u8, u8, u8) {
    if pct >= red_at {
        RED
    } else if pct >= amber_at {
        AMBER
    } else {
        GREEN
    }
}

/// Subcommand entry point. Reads stdin to EOF, persists the cache, and
/// (in default render mode) prints a powerline status string on stdout.
///
/// Two modes:
///
/// * `cache_only = false` (default): write cache + emit powerline string.
///   All errors are swallowed and degrade to an empty line — the
///   statusline runs every prompt render and must never break the user's
///   shell.
/// * `cache_only = true`: side-channel mode for users running their own
///   statusline. Write cache + emit nothing on stdout. Surfaces malformed
///   JSON / cache-write failures on stderr with a non-zero exit so a
///   broken pipeline is visible instead of silently rotting the cache.
pub fn execute(cache_only: bool) -> Result<()> {
    let mut buf = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut buf);

    match run_with(&buf, cache_path().as_deref(), cache_only) {
        Ok(Some(line)) => {
            // Newline so it integrates cleanly with shell prompt drawing.
            println!("{line}");
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(e) if cache_only => {
            // Surface the failure so the calling script (and the user)
            // can see why the cache isn't refreshing. stderr only —
            // stdout stays silent in cache-only mode by contract.
            // Exit directly so `anyhow::Error`'s default "Error: ..."
            // chain doesn't double-print on top of our message.
            eprintln!("ainb statusline --cache-only: {e}");
            std::process::exit(1);
        }
        Err(_) => {
            // Default render mode: degrade to an empty line so we never
            // break the user's shell prompt.
            println!();
            Ok(())
        }
    }
}

/// Pure core. Parses `buf`, writes the cache (if `cache_path` is
/// `Some`), and returns what should be written to stdout.
///
/// Returned values:
/// * `Ok(Some(line))` — print `line` then newline (default render mode).
/// * `Ok(None)`       — print nothing (cache-only mode happy path).
/// * `Err(e)`         — caller decides whether to surface (cache-only)
///   or swallow (default render mode).
///
/// Splitting parse + write + render away from stdin/stdout lets us
/// exercise both modes against an in-memory buffer and a temp cache
/// path in tests.
pub fn run_with(
    buf: &[u8],
    cache_path: Option<&std::path::Path>,
    cache_only: bool,
) -> Result<Option<String>> {
    let cache = parse_payload(buf)
        .ok_or_else(|| anyhow::anyhow!("malformed or empty statusline JSON payload on stdin"))?;

    // Always attempt the cache write (the whole point of both modes).
    // In cache-only mode we surface errors; in render mode the wrapper
    // swallows them so the prompt keeps working.
    if let Some(path) = cache_path {
        write_cache(path, &cache)?;
    }

    if cache_only {
        Ok(None)
    } else {
        Ok(Some(render_powerline(&cache)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_payload_returns_none_for_empty_stdin() {
        assert!(parse_payload(b"").is_none());
        assert!(parse_payload(b"   \n\t  ").is_none());
    }

    #[test]
    fn parse_payload_returns_none_for_malformed_json() {
        assert!(parse_payload(b"not json").is_none());
        assert!(parse_payload(b"{unterminated").is_none());
    }

    #[test]
    fn parse_payload_full_schema_populates_all_fields() {
        let raw = br#"{
            "model": {"display_name": "Opus 4.7"},
            "context_window": {"used_percentage": 32.4},
            "rate_limits": {
                "five_hour": {"used_percentage": 12.1, "resets_at": "2026-05-07T00:00:00Z"},
                "seven_day": {"used_percentage": 3.0, "resets_at": "2026-05-13T00:00:00Z"}
            },
            "cost": {"total_cost_usd": 4.21}
        }"#;
        let cache = parse_payload(raw).expect("should parse");
        assert_eq!(cache.version, CACHE_SCHEMA_VERSION);
        assert_eq!(cache.model.as_deref(), Some("Opus 4.7"));
        assert_eq!(cache.context_pct, Some(32));
        assert_eq!(cache.five_hour.as_ref().unwrap().pct, 12);
        assert_eq!(
            cache.five_hour.as_ref().unwrap().resets_at.as_deref(),
            Some("2026-05-07T00:00:00Z")
        );
        assert_eq!(cache.seven_day.as_ref().unwrap().pct, 3);
        assert_eq!(cache.today_cost_usd, Some(4.21));
    }

    #[test]
    fn parse_payload_partial_only_fills_present_fields() {
        let raw = br#"{
            "rate_limits": {
                "five_hour": {"used_percentage": 50}
            }
        }"#;
        let cache = parse_payload(raw).expect("should parse");
        assert!(cache.model.is_none());
        assert!(cache.context_pct.is_none());
        assert!(cache.today_cost_usd.is_none());
        assert!(cache.seven_day.is_none());
        let fh = cache.five_hour.unwrap();
        assert_eq!(fh.pct, 50);
        assert!(fh.resets_at.is_none());
    }

    #[test]
    fn round_pct_clamps_and_rounds() {
        assert_eq!(round_pct(0.0), 0);
        assert_eq!(round_pct(0.4), 0);
        assert_eq!(round_pct(0.6), 1);
        assert_eq!(round_pct(99.5), 100);
        assert_eq!(round_pct(150.0), 100);
        assert_eq!(round_pct(-5.0), 0);
    }

    #[test]
    fn write_cache_is_atomic_and_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("live.json");
        let cache = LiveCache {
            version: CACHE_SCHEMA_VERSION,
            updated_at: "2026-05-07T00:00:00Z".to_string(),
            five_hour: Some(RateWindow {
                pct: 42,
                resets_at: Some("2026-05-07T05:00:00Z".to_string()),
            }),
            seven_day: None,
            today_cost_usd: Some(1.23),
            context_pct: Some(20),
            model: Some("Sonnet 4.5".to_string()),
        };
        write_cache(&path, &cache).unwrap();

        // No leftover .tmp file
        let tmp = path.with_extension("json.tmp");
        assert!(!tmp.exists(), "tmp should be renamed away");

        let read = read_cache(&path).expect("should read back");
        assert_eq!(read, cache);
    }

    #[test]
    fn read_cache_rejects_wrong_schema_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("live.json");
        let mut bad = LiveCache {
            version: 999,
            updated_at: "2026-05-07T00:00:00Z".to_string(),
            five_hour: None,
            seven_day: None,
            today_cost_usd: None,
            context_pct: None,
            model: None,
        };
        bad.version = 999;
        std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(read_cache(&path).is_none());
    }

    #[test]
    fn read_cache_returns_none_for_missing_file() {
        let dir = tempdir().unwrap();
        assert!(read_cache(&dir.path().join("nope.json")).is_none());
    }

    #[test]
    fn render_powerline_emits_each_present_field() {
        let cache = LiveCache {
            version: CACHE_SCHEMA_VERSION,
            updated_at: "2026-05-07T00:00:00Z".to_string(),
            five_hour: Some(RateWindow {
                pct: 90,
                resets_at: None,
            }),
            seven_day: Some(RateWindow {
                pct: 50,
                resets_at: None,
            }),
            today_cost_usd: Some(2.5),
            context_pct: Some(45),
            model: Some("Opus".to_string()),
        };
        let line = render_powerline(&cache);
        assert!(line.contains("Opus"));
        assert!(line.contains("ctx"));
        assert!(line.contains("45"));
        assert!(line.contains("5h"));
        assert!(line.contains("90"));
        assert!(line.contains("wk"));
        assert!(line.contains("$2.50"));
    }

    #[test]
    fn render_powerline_minimal_for_empty_cache() {
        let cache = LiveCache {
            version: CACHE_SCHEMA_VERSION,
            updated_at: "2026-05-07T00:00:00Z".to_string(),
            five_hour: None,
            seven_day: None,
            today_cost_usd: None,
            context_pct: None,
            model: None,
        };
        // Just don't panic; result may be empty string.
        let _ = render_powerline(&cache);
    }

    /// `--cache-only`: cache is written, stdout payload is `None`
    /// (i.e. the wrapper prints nothing).
    #[test]
    fn cache_only_mode_writes_cache_and_emits_nothing_on_stdout() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("live.json");
        let raw = br#"{
            "model": {"display_name": "Opus 4.7"},
            "rate_limits": {"five_hour": {"used_percentage": 42}}
        }"#;

        let out = run_with(raw, Some(&path), true).expect("cache-only must succeed");
        assert!(out.is_none(), "cache-only must emit nothing on stdout");

        let cache = read_cache(&path).expect("cache file must exist and be parseable");
        assert_eq!(cache.model.as_deref(), Some("Opus 4.7"));
        assert_eq!(cache.five_hour.unwrap().pct, 42);
    }

    /// `--cache-only` is the only mode that surfaces errors. A malformed
    /// payload must return `Err` so the wrapper can exit non-zero and
    /// log to stderr — silent rot would defeat the whole purpose of the
    /// flag (users can't tell their pipeline broke).
    #[test]
    fn cache_only_mode_returns_error_on_malformed_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("live.json");

        let err = run_with(b"not json at all", Some(&path), true)
            .expect_err("malformed JSON must error in cache-only mode");
        let msg = format!("{err}");
        assert!(msg.contains("malformed"), "error must explain cause: got {msg}");
        assert!(!path.exists(), "no cache file should be written on parse failure");
    }

    /// Both modes share a single parse + write path, so the bytes on
    /// disk must be identical except for the `updated_at` timestamp
    /// (which is intrinsically time-dependent — we normalize it before
    /// comparing).
    #[test]
    fn cache_only_mode_writes_same_schema_as_full_mode() {
        let dir = tempdir().unwrap();
        let render_path = dir.path().join("render.json");
        let cache_only_path = dir.path().join("cache_only.json");
        let raw = br#"{
            "model": {"display_name": "Sonnet 4.5"},
            "context_window": {"used_percentage": 40},
            "rate_limits": {
                "five_hour": {"used_percentage": 12, "resets_at": "2026-05-07T05:00:00Z"},
                "seven_day": {"used_percentage": 3,  "resets_at": "2026-05-13T00:00:00Z"}
            },
            "cost": {"total_cost_usd": 4.21}
        }"#;

        let line = run_with(raw, Some(&render_path), false).expect("render mode must succeed");
        assert!(line.is_some(), "render mode must produce a powerline string");

        let none = run_with(raw, Some(&cache_only_path), true).expect("cache-only must succeed");
        assert!(none.is_none(), "cache-only must produce no stdout");

        let mut a = read_cache(&render_path).expect("render cache exists");
        let mut b = read_cache(&cache_only_path).expect("cache-only cache exists");
        // Normalize the only field that differs between two real-time
        // writes: the wall-clock timestamp.
        a.updated_at.clear();
        b.updated_at.clear();
        assert_eq!(a, b, "both modes must persist byte-identical schema");
    }

    #[test]
    fn pct_color_thresholds_5h() {
        // 5h thresholds are amber=60, red=85
        assert_eq!(pct_color(0, 60, 85), GREEN);
        assert_eq!(pct_color(59, 60, 85), GREEN);
        assert_eq!(pct_color(60, 60, 85), AMBER);
        assert_eq!(pct_color(84, 60, 85), AMBER);
        assert_eq!(pct_color(85, 60, 85), RED);
        assert_eq!(pct_color(100, 60, 85), RED);
    }
}
