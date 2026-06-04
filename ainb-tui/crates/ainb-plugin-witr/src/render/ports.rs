//! Painter for the Ports tab.
//!
//! Two data sources from a [`WitrSnapshot`]:
//!
//! - `Process.sockets`: per-process socket list (opaque
//!   `Vec<serde_json::Value>` for v1 — each socket emitted by witr
//!   carries `Port`/`Address`/`State`/`Protocol`/`Inode`).
//! - `SocketInfo`: top-level summary for `port`-typed targets
//!   (opaque `serde_json::Value` — `Port`/`State`/`LocalAddr`/
//!   `RemoteAddr`/`Explanation`/`Workaround`).
//!
//! We render whichever is present. On `port`-typed targets `SocketInfo`
//! is always set; on `pid`/`name`-typed targets the per-process
//! `sockets` array carries the data.
//!
//! The opaque-`Value` access is the cfx.3 model trade-off: typed
//! Socket structs ship in cfx.6 when the detail overlay needs richer
//! interaction. cfx.5 uses `pointer()` lookups to keep the surface
//! tight and forward-compat with witr v0.4 schema drift.

use ainb_plugin_sdk::{Cell, Coord, WireBuffer};
use serde_json::Value;

use crate::model::WitrSnapshot;

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

    // Header.
    paint_row(
        buf,
        origin_y,
        area_width,
        "PORT   PROTO  STATE        ADDRESS",
    );

    let mut y = origin_y.saturating_add(1);
    let last_y = origin_y.saturating_add(area_height);

    // 1. Top-level SocketInfo (port-typed targets).
    if let Some(socket_info) = snap.socket_info.as_ref() {
        if y < last_y {
            paint_row(buf, y, area_width, &format_socket_info(socket_info));
            y = y.saturating_add(1);
        }
    }

    // 2. Per-process sockets list.
    for socket in &snap.process.sockets {
        if y >= last_y {
            break;
        }
        paint_row(buf, y, area_width, &format_socket(socket));
        y = y.saturating_add(1);
    }

    if y == origin_y.saturating_add(1) && y < last_y {
        // Nothing painted past the header — surface a hint.
        paint_row(buf, y, area_width, "  (no port data for this target)");
    }
}

fn format_socket_info(v: &Value) -> String {
    let port = pointer_string(v, "/Port");
    let state = pointer_string(v, "/State");
    let local = pointer_string(v, "/LocalAddr");
    // Witr's `SocketInfo` upstream doesn't carry a Protocol field
    // today (verified against witr v0.3.2 pkg/model/socket.go), so we
    // default to "tcp" — but if upstream adds one in v0.4 we want to
    // render the real value rather than silently shadow it.
    let proto = match v.pointer("/Protocol") {
        Some(Value::String(s)) => s.clone(),
        _ => "tcp".to_string(),
    };
    format!("{port:<6} {proto:<6} {state:<12} {local}")
}

fn format_socket(v: &Value) -> String {
    let port = pointer_string(v, "/Port");
    let proto = pointer_string(v, "/Protocol");
    let state = pointer_string(v, "/State");
    let addr = pointer_string(v, "/Address");
    format!(
        "{:<6} {:<6} {:<12} {addr}",
        truncate(&port, 6),
        truncate(&proto, 6),
        truncate(&state, 12),
    )
}

fn pointer_string(v: &Value, path: &str) -> String {
    match v.pointer(path) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
        None => "-".to_string(),
    }
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
    let keep = max_chars.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::test_support::buffer_contains;

    fn snap_with(json: serde_json::Value) -> WitrSnapshot {
        serde_json::from_value(json).expect("fixture parses")
    }

    #[test]
    fn renders_header_always() {
        let snap = snap_with(serde_json::json!({
            "Target": {"Type": "name", "Value": "x"},
            "ResolvedTarget": "x",
            "Process": {"PID": 1, "PPID": 0, "Command": "x"},
            "Ancestry": [],
            "Source": {"Type": "unknown", "Name": ""},
            "Warnings": []
        }));
        let mut buf = WireBuffer::new(60, 5);
        render(&mut buf, 0, 5, 60, &snap);
        assert!(buffer_contains(&buf, "PORT"));
        assert!(buffer_contains(&buf, "PROTO"));
    }

    #[test]
    fn renders_top_level_socket_info_when_present() {
        let snap = snap_with(serde_json::json!({
            "Target": {"Type": "port", "Value": "5432"},
            "ResolvedTarget": "5432",
            "Process": {"PID": 1, "PPID": 0, "Command": "postgres"},
            "Ancestry": [],
            "Source": {"Type": "systemd", "Name": "pg"},
            "Warnings": [],
            "SocketInfo": {
                "Port": 5432,
                "State": "LISTEN",
                "LocalAddr": "0.0.0.0",
                "RemoteAddr": ""
            }
        }));
        let mut buf = WireBuffer::new(60, 5);
        render(&mut buf, 0, 5, 60, &snap);
        assert!(buffer_contains(&buf, "5432"));
        assert!(buffer_contains(&buf, "LISTEN"));
        assert!(buffer_contains(&buf, "0.0.0.0"));
    }

    #[test]
    fn renders_per_process_socket_list() {
        let snap = snap_with(serde_json::json!({
            "Target": {"Type": "pid", "Value": "1234"},
            "ResolvedTarget": "1234",
            "Process": {
                "PID": 1234,
                "PPID": 1,
                "Command": "nginx",
                "Sockets": [
                    {"Port": 80, "Protocol": "tcp", "State": "LISTEN", "Address": "*"},
                    {"Port": 443, "Protocol": "tcp", "State": "LISTEN", "Address": "*"}
                ]
            },
            "Ancestry": [],
            "Source": {"Type": "systemd", "Name": "nginx"},
            "Warnings": []
        }));
        let mut buf = WireBuffer::new(80, 8);
        render(&mut buf, 0, 8, 80, &snap);
        assert!(buffer_contains(&buf, "80"));
        assert!(buffer_contains(&buf, "443"));
        assert!(buffer_contains(&buf, "LISTEN"));
        assert!(buffer_contains(&buf, "tcp"));
    }

    #[test]
    fn surfaces_no_port_data_hint_when_empty() {
        let snap = snap_with(serde_json::json!({
            "Target": {"Type": "name", "Value": "x"},
            "ResolvedTarget": "x",
            "Process": {"PID": 1, "PPID": 0, "Command": "x"},
            "Ancestry": [],
            "Source": {"Type": "unknown", "Name": ""},
            "Warnings": []
        }));
        let mut buf = WireBuffer::new(60, 5);
        render(&mut buf, 0, 5, 60, &snap);
        assert!(buffer_contains(&buf, "no port data"));
    }

    #[test]
    fn does_not_panic_on_zero_dimensions() {
        let snap = snap_with(serde_json::json!({
            "Target": {"Type": "name", "Value": "x"},
            "ResolvedTarget": "x",
            "Process": {"PID": 1, "PPID": 0, "Command": "x"},
            "Ancestry": [],
            "Source": {"Type": "unknown", "Name": ""},
            "Warnings": []
        }));
        let mut buf = WireBuffer::new(0, 0);
        render(&mut buf, 0, 0, 0, &snap);
        assert!(buf.cells.is_empty());
    }
}
