//! Painter for the Processes tab — the primary target process plus
//! its ancestry chain.
//!
//! Row format (left to right):
//!
//! ```text
//! PID    PPID   COMMAND                  USER       RSS    CPU%
//! ```
//!
//! Header row first, then the primary process, then each ancestor
//! in order (root closest to the top). Width-aware: long commands
//! are truncated with `…`; on narrow viewports the USER / RSS / CPU
//! columns drop off the right.

use ainb_plugin_sdk::{Cell, Coord, WireBuffer};

use crate::model::{Process, WitrSnapshot};

/// Render the Processes tab body into `buf` starting at `origin_y`.
///
/// `area_height` includes the body only (the tab strip lives at
/// row 0 and is not this painter's concern). `area_width` clips
/// horizontally.
pub fn render(
    buf: &mut WireBuffer,
    origin_y: u16,
    area_height: u16,
    area_width: u16,
    snap: &WitrSnapshot,
) {
    if area_height == 0 || area_width == 0 {
        return;
    }
    paint_row(buf, origin_y, area_width, &header());

    let body_first_y = origin_y.saturating_add(1);
    if body_first_y >= origin_y.saturating_add(area_height) {
        return;
    }

    // Primary process first.
    paint_process_row(buf, body_first_y, area_width, &snap.process);

    // Ancestry chain — closest parent first.
    let mut y = body_first_y.saturating_add(1);
    let last_y = origin_y.saturating_add(area_height);
    for ancestor in &snap.ancestry {
        if y >= last_y {
            break;
        }
        paint_process_row(buf, y, area_width, ancestor);
        y = y.saturating_add(1);
    }
}

fn header() -> String {
    // Single-line header. Spaces stay regular (not Unicode) so the
    // wire format is byte-deterministic.
    format!(
        "{:<6} {:<6} {:<24} {:<10} {:>10} {:>6}",
        "PID", "PPID", "COMMAND", "USER", "RSS", "CPU%"
    )
}

fn paint_process_row(buf: &mut WireBuffer, y: u16, width: u16, p: &Process) {
    let rss = format_bytes(p.memory_rss);
    let cpu = format!("{:>5.1}%", p.cpu_percent);
    // `format!` width specifiers count bytes — fine for the ASCII-only
    // pid/ppid/cpu numerics. `command` and `user` flow through
    // `truncate` (chars-aware) and are usually ASCII; multi-byte
    // glyphs in either column will drift alignment by 1-2 cells per
    // wide char. Acceptable for v1 — most-watched processes have
    // ASCII names. cfx.6's detail overlay can lean on `unicode-width`
    // if richer locale data demands it.
    let row = format!(
        "{:<6} {:<6} {:<24} {:<10} {:>10} {:>6}",
        truncate(&p.pid.to_string(), 6),
        truncate(&p.ppid.to_string(), 6),
        truncate(&p.command, 24),
        truncate(&p.user, 10),
        truncate(&rss, 10),
        truncate(&cpu, 6),
    );
    paint_row(buf, y, width, &row);
}

fn paint_row(buf: &mut WireBuffer, y: u16, width: u16, text: &str) {
    let mut x: u16 = 0;
    for ch in text.chars() {
        if x >= width {
            break;
        }
        buf.push(Coord::new(x, y), Cell::new(ch.to_string()));
        x = x.saturating_add(1);
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    // Reserve one cell for the ellipsis.
    let keep = max_chars.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

fn format_bytes(bytes: u64) -> String {
    // Binary units (1024-based) with binary-correct labels —
    // matches `vm_stat` on macOS and `free -h` on Linux. Using `K/M/G`
    // with a 1024 divisor would mislead readers familiar with SI
    // prefixes.
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{:.1}GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1}MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1}KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::test_support::{buffer_contains, flatten_rows_trimmed};

    fn snap(primary_pid: i64, ancestors: &[(i64, &str)]) -> WitrSnapshot {
        let ancestry: Vec<_> = ancestors
            .iter()
            .map(|(pid, cmd)| {
                serde_json::json!({
                    "PID": pid,
                    "PPID": 1,
                    "Command": cmd
                })
            })
            .collect();
        let value = serde_json::json!({
            "Target": {"Type": "pid", "Value": format!("{primary_pid}")},
            "ResolvedTarget": format!("{primary_pid}"),
            "Process": {
                "PID": primary_pid,
                "PPID": 1,
                "Command": "nginx",
                "User": "root",
                "MemoryRSS": 12 * 1024 * 1024,
                "CPUPercent": 1.5_f64,
            },
            "Ancestry": ancestry,
            "Source": {"Type": "systemd", "Name": "nginx"},
            "Warnings": []
        });
        serde_json::from_value(value).expect("fixture parses")
    }

    #[test]
    fn renders_header_and_primary_process() {
        let mut buf = WireBuffer::new(80, 10);
        render(&mut buf, 0, 10, 80, &snap(1234, &[]));
        assert!(buffer_contains(&buf, "PID"));
        assert!(buffer_contains(&buf, "COMMAND"));
        assert!(buffer_contains(&buf, "1234"));
        assert!(buffer_contains(&buf, "nginx"));
        assert!(buffer_contains(&buf, "root"));
    }

    #[test]
    fn renders_each_ancestor_on_its_own_row() {
        let mut buf = WireBuffer::new(80, 10);
        render(
            &mut buf,
            0,
            10,
            80,
            &snap(1234, &[(800, "shell"), (1, "init")]),
        );
        let rows = flatten_rows_trimmed(&buf);
        let has_shell = rows.iter().any(|r| r.contains("800") && r.contains("shell"));
        let has_init = rows.iter().any(|r| r.contains("init"));
        assert!(has_shell, "shell ancestor row painted: {rows:?}");
        assert!(has_init, "init ancestor row painted: {rows:?}");
    }

    #[test]
    fn truncates_long_command_with_ellipsis() {
        let value = serde_json::json!({
            "Target": {"Type": "pid", "Value": "1"},
            "ResolvedTarget": "1",
            "Process": {
                "PID": 1,
                "PPID": 0,
                "Command": "this-command-name-is-far-too-long-for-the-column"
            },
            "Ancestry": [],
            "Source": {"Type": "unknown", "Name": ""},
            "Warnings": []
        });
        let s: WitrSnapshot = serde_json::from_value(value).unwrap();
        let mut buf = WireBuffer::new(80, 5);
        render(&mut buf, 0, 5, 80, &s);
        assert!(
            buffer_contains(&buf, "this-command-name-is-fa…")
                || buffer_contains(&buf, "this-command-name-is-f…"),
            "command should be truncated with ellipsis",
        );
    }

    #[test]
    fn formats_memory_in_human_units() {
        assert_eq!(format_bytes(0), "0B");
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(2048), "2.0KiB");
        assert_eq!(format_bytes(4 * 1024 * 1024), "4.0MiB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0GiB");
    }

    #[test]
    fn does_not_panic_on_zero_height_viewport() {
        let mut buf = WireBuffer::new(80, 0);
        render(&mut buf, 0, 0, 80, &snap(1, &[]));
        assert!(buf.cells.is_empty(), "0-height area paints nothing");
    }

    #[test]
    fn does_not_panic_on_zero_width_viewport() {
        let mut buf = WireBuffer::new(0, 5);
        render(&mut buf, 0, 5, 0, &snap(1, &[]));
        assert!(buf.cells.is_empty(), "0-width area paints nothing");
    }

    #[test]
    fn clips_ancestry_rows_to_available_height() {
        let mut buf = WireBuffer::new(80, 3);
        render(
            &mut buf,
            0,
            3,
            80,
            &snap(1234, &[(800, "a"), (700, "b"), (600, "c"), (500, "d")]),
        );
        // 3 rows = 1 header + primary + 1 ancestor — `b/c/d` clipped.
        let rows = flatten_rows_trimmed(&buf);
        assert!(rows[0].contains("PID"));
        assert!(rows[1].contains("nginx"));
        // The ancestor row painted must be the FIRST listed (`a`).
        assert!(rows[2].contains('a') || rows[2].contains("800"));
    }
}
