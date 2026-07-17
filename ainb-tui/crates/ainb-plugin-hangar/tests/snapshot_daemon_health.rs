//! P8.5 — Daemon health pane render snapshot + dual-dim sparkline assertions.
//!
//! Seeds the wire [`DaemonHealthSnapshot`] (one connected `claude` runtime,
//! claim cache 12/64, 3 concurrent tasks, a 60-second throughput window whose
//! `completed` count climbs monotonically with a failure spike at `t=30`),
//! renders [`DaemonHealthState`] to a backing buffer, and pins the layout with
//! `insta::assert_snapshot!` (trailing newline trimmed per
//! `reference_insta_trailing_newline_trap`).
//!
//! Two dual-dim invariants are asserted **independently and non-vacuously**:
//!
//!   * the sparkline paints exactly 60 columns (one per throughput sample);
//!   * column heights are monotone over the monotone-increasing `completed`
//!     series (the throughput dimension);
//!   * the spike columns `[28..32]` carry `SPARK_RED` cells while a no-failure
//!     column does not (the failure-rate dimension), so height and red proportion
//!     are each verifiable on their own.

use ainb_hangar_proto::settings::{
    ClaimCache, DaemonHealthSnapshot, RuntimeHealthRow, THROUGHPUT_WINDOW, ThroughputSample,
};
use ainb_plugin_hangar::screen::daemon_health::{DaemonHealthState, render_daemon_health};
use ainb_plugin_hangar::widgets::sparkline_dual::{SPARK_GREEN, SPARK_RED};
use ainb_plugin_sdk::WireBuffer;

/// `ONLINE_GREEN` — the connected-runtime presence dot colour.
const ONLINE_GREEN: ainb_plugin_sdk::Color = ainb_plugin_sdk::Color::rgb(100, 200, 100);

/// The throughput window width as a `u16` column count (60 fits trivially).
fn window_u16() -> u16 {
    u16::try_from(THROUGHPUT_WINDOW).unwrap()
}

/// The seeded health snapshot: one connected `claude` runtime, claim cache 12/64,
/// 3 concurrent tasks, and a 60-second window where `completed` climbs `0..60`
/// with a failure spike centred at `t=30` (indices 28..32 carry failures).
fn seeded_snapshot() -> DaemonHealthSnapshot {
    let throughput: Vec<ThroughputSample> = (0..THROUGHPUT_WINDOW)
        .map(|i| {
            // `completed` climbs `1..=60` so every second has throughput (a
            // non-degenerate column) while staying strictly monotone.
            let completed = u32::try_from(i + 1).unwrap();
            // A sustained failure spike from `t=28` onward: a flat elevated
            // failure count keeps the *total* height monotone (completed rises by
            // one each step, failed is constant past the onset) while giving the
            // band a clearly-visible red proportion. The test only asserts red is
            // present across [28, 32) and absent before it.
            let failed = if i >= 28 { 5 } else { 0 };
            ThroughputSample {
                ts: 1_700_000_000 + i64::try_from(i).unwrap(),
                completed,
                failed,
            }
        })
        .collect();

    DaemonHealthSnapshot {
        runtimes: vec![RuntimeHealthRow {
            provider: "claude".into(),
            connected: true,
            pid: 14829,
        }],
        claim_cache: ClaimCache {
            used: 12,
            capacity: 64,
        },
        concurrent_tasks: 3,
        task_throughput_60s: throughput,
        // Match THIS build's version so the baseline fixture never trips the
        // version-skew banner — the skew case is exercised by its own test.
        daemon_version: env!("CARGO_PKG_VERSION").into(),
        db_error: None,
    }
}

/// Flatten the buffer into a `\n`-joined glyph map, each line `trim_end`-ed and
/// the whole map trailing-newline-trimmed (insta trap).
fn glyph_map(buf: &WireBuffer, cols: u16) -> String {
    let mut grid = vec![vec![' '; cols as usize]; buf.height as usize];
    for (coord, cell) in &buf.cells {
        if coord.y < buf.height && coord.x < cols {
            if let Some(ch) = cell.symbol.chars().next() {
                grid[coord.y as usize][coord.x as usize] = ch;
            }
        }
    }
    grid.into_iter()
        .map(|r| r.into_iter().collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end_matches('\n')
        .to_string()
}

/// The full daemon-health pane renders the runtime row, the claim-cache bar, the
/// concurrency count, and the throughput sparkline header.
#[test]
fn render_full_health_pane_snapshot() {
    let state = DaemonHealthState::from_snapshot(seeded_snapshot());
    let mut buf = WireBuffer::new(80, 20);
    render_daemon_health(&mut buf, 80, 0, 20, &state);
    let full = glyph_map(&buf, 80);

    assert!(full.contains("Daemon health"), "title:\n{full}");
    assert!(full.contains("runtime: claude"), "runtime row:\n{full}");
    assert!(full.contains("connected"), "runtime status:\n{full}");
    assert!(full.contains("pid 14829"), "runtime pid:\n{full}");
    assert!(full.contains("claim cache: 12/64"), "claim cache:\n{full}");
    assert!(full.contains("18%"), "claim cache pct (12/64):\n{full}");
    assert!(full.contains("concurrent: 3"), "concurrency:\n{full}");
    assert!(
        full.contains("throughput (60s):"),
        "sparkline header:\n{full}"
    );

    insta::assert_snapshot!(full);
}

/// The sparkline paints exactly 60 columns (one per throughput sample).
#[test]
fn sparkline_has_60_columns() {
    let state = DaemonHealthState::from_snapshot(seeded_snapshot());
    // Wide enough to fit all 60 columns without clipping.
    let mut buf = WireBuffer::new(70, 20);
    render_daemon_health(&mut buf, 70, 0, 20, &state);

    // The sparkline lives below the header rows; identify it as the rows where
    // the block glyphs appear. Count distinct x columns carrying a spark glyph.
    let spark_glyphs = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let mut cols: Vec<u16> = buf
        .cells
        .iter()
        .filter(|(_, cell)| cell.symbol.chars().next().is_some_and(|c| spark_glyphs.contains(&c)))
        .map(|(coord, _)| coord.x)
        .collect();
    cols.sort_unstable();
    cols.dedup();
    assert_eq!(
        cols.len(),
        THROUGHPUT_WINDOW,
        "the sparkline must paint exactly {THROUGHPUT_WINDOW} columns, got {}",
        cols.len()
    );
    // Columns are contiguous 0..60 (one per sample, left-aligned).
    assert_eq!(cols.first(), Some(&0));
    assert_eq!(cols.last(), Some(&(window_u16() - 1)));
}

/// Column heights are monotone over the monotone-increasing `completed` series
/// (the throughput dimension is independently verifiable).
#[test]
fn sparkline_heights_monotone_over_completed() {
    let state = DaemonHealthState::from_snapshot(seeded_snapshot());
    let mut buf = WireBuffer::new(70, 20);
    render_daemon_health(&mut buf, 70, 0, 20, &state);

    let spark_glyphs = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let height_at = |x: u16| {
        buf.cells
            .iter()
            .filter(|(coord, cell)| {
                coord.x == x
                    && cell.symbol.chars().next().is_some_and(|c| spark_glyphs.contains(&c))
            })
            .count()
    };
    let mut prev = 0;
    for x in 1..window_u16() {
        let h = height_at(x);
        assert!(
            h >= prev,
            "column heights must be monotone (completed climbs): x={x} height {h} < {prev}"
        );
        prev = h;
    }
    // The tallest (last) column is the highest — the peak throughput second.
    assert!(height_at(59) >= height_at(0));
}

/// The spike band `[28..32]` carries red cells (the failure-rate dimension),
/// while a no-failure column carries only green — both independently verifiable.
#[test]
fn sparkline_spike_band_is_red() {
    let state = DaemonHealthState::from_snapshot(seeded_snapshot());
    let mut buf = WireBuffer::new(70, 20);
    render_daemon_health(&mut buf, 70, 0, 20, &state);

    let red_at =
        |x: u16| buf.cells.iter().any(|(coord, cell)| coord.x == x && cell.fg == Some(SPARK_RED));
    let green_at = |x: u16| {
        buf.cells
            .iter()
            .any(|(coord, cell)| coord.x == x && cell.fg == Some(SPARK_GREEN))
    };
    // Every column in the failure band [28, 32) carries at least one red cell.
    for x in 28..32u16 {
        assert!(
            red_at(x),
            "the failure-spike band column x={x} must carry a red cell"
        );
    }
    // A no-failure column (t=10) carries green but no red.
    assert!(green_at(10), "a success column must carry green");
    assert!(
        !red_at(10),
        "a no-failure column must carry no red (red proportion = 0)"
    );
}

/// The connected runtime's presence dot is painted `ONLINE_GREEN` (non-vacuous
/// colour check on the runtimes list).
#[test]
fn connected_runtime_dot_is_green() {
    let state = DaemonHealthState::from_snapshot(seeded_snapshot());
    let mut buf = WireBuffer::new(80, 20);
    render_daemon_health(&mut buf, 80, 0, 20, &state);
    let green_dot = buf
        .cells
        .iter()
        .any(|(_, cell)| cell.symbol == "●" && cell.fg == Some(ONLINE_GREEN));
    assert!(
        green_dot,
        "the connected runtime's `●` presence dot must be painted ONLINE_GREEN"
    );
}

/// `OFFLINE_RED` — the warning/banner colour on the daemon-health pane.
const OFFLINE_RED: ainb_plugin_sdk::Color = ainb_plugin_sdk::Color::rgb(220, 120, 100);

/// A `db_error` in the snapshot renders the red DATABASE UNREACHABLE banner
/// with the daemon's diagnosis — the stale-binary-serving-a-newer-database trap
/// must scream, never render silently-blank zeros.
#[test]
fn db_error_renders_red_banner() {
    let mut snap = seeded_snapshot();
    snap.db_error = Some("database schema (migration 41) is AHEAD of this daemon binary".into());
    let state = DaemonHealthState::from_snapshot(snap);
    let mut buf = WireBuffer::new(100, 24);
    render_daemon_health(&mut buf, 100, 0, 24, &state);
    let full = glyph_map(&buf, 100);

    assert!(
        full.contains("DATABASE UNREACHABLE"),
        "banner headline:\n{full}"
    );
    assert!(
        full.contains("migration 41"),
        "banner carries the daemon's diagnosis:\n{full}"
    );
    // Non-vacuous colour check: the banner glyphs are painted OFFLINE_RED.
    let red_banner = buf
        .cells
        .iter()
        .any(|(_, cell)| cell.symbol == "✗" && cell.fg == Some(OFFLINE_RED));
    assert!(red_banner, "the banner `✗` must be painted OFFLINE_RED");
}

/// An EMPTY `daemon_version` (a daemon that predates version reporting) renders
/// the stale-binary warning instead of a version line.
#[test]
fn empty_version_renders_stale_daemon_warning() {
    let mut snap = seeded_snapshot();
    snap.daemon_version = String::new();
    let state = DaemonHealthState::from_snapshot(snap);
    let mut buf = WireBuffer::new(100, 24);
    render_daemon_health(&mut buf, 100, 0, 24, &state);
    let full = glyph_map(&buf, 100);

    assert!(
        full.contains("stale binary"),
        "pre-version daemon must be flagged stale:\n{full}"
    );
    assert!(
        !full.contains("daemon: 1.16"),
        "no version line when the daemon reported none:\n{full}"
    );
}

/// The healthy path renders the version line and NO banner (the banner must
/// never false-positive on a healthy daemon).
#[test]
fn healthy_snapshot_renders_version_no_banner() {
    let state = DaemonHealthState::from_snapshot(seeded_snapshot());
    let mut buf = WireBuffer::new(100, 24);
    render_daemon_health(&mut buf, 100, 0, 24, &state);
    let full = glyph_map(&buf, 100);

    assert!(
        full.contains(&format!("daemon: {}", env!("CARGO_PKG_VERSION"))),
        "version line:\n{full}"
    );
    assert!(
        !full.contains("DATABASE UNREACHABLE"),
        "no banner on healthy:\n{full}"
    );
    assert!(
        !full.contains("VERSION SKEW"),
        "no skew banner on match:\n{full}"
    );
    assert!(!full.contains("stale binary"), "no stale warning:\n{full}");
}

/// A daemon whose reported version differs from this client build renders the
/// red VERSION SKEW banner with both versions + the restart command. This is
/// the post-`brew upgrade` case: an OLD daemon still serving a NEW client.
#[test]
fn version_skew_renders_red_banner() {
    let mut snap = seeded_snapshot();
    snap.daemon_version = "1.14.0 (oldsha, ci)".into(); // != this build's version
    let state = DaemonHealthState::from_snapshot(snap);
    let mut buf = WireBuffer::new(100, 24);
    render_daemon_health(&mut buf, 100, 0, 24, &state);
    let full = glyph_map(&buf, 100);

    assert!(
        full.contains("DAEMON VERSION SKEW"),
        "skew headline:\n{full}"
    );
    assert!(
        full.contains("1.14.0"),
        "banner names the daemon version:\n{full}"
    );
    assert!(
        full.contains("ainb hangar daemon restart"),
        "banner gives the fix:\n{full}"
    );
}

/// A daemon reporting THIS client's exact version renders NO skew banner (the
/// banner must not false-positive when versions agree).
#[test]
fn matching_version_renders_no_skew_banner() {
    let mut snap = seeded_snapshot();
    snap.daemon_version = env!("CARGO_PKG_VERSION").into();
    let state = DaemonHealthState::from_snapshot(snap);
    let mut buf = WireBuffer::new(100, 24);
    render_daemon_health(&mut buf, 100, 0, 24, &state);
    let full = glyph_map(&buf, 100);

    assert!(
        !full.contains("VERSION SKEW"),
        "no skew banner on match:\n{full}"
    );
}
