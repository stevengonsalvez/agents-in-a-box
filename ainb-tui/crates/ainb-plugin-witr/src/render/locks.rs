//! Painter for the Locks tab.
//!
//! Reads `FileContext` (opaque `Value` for v1) and renders whatever
//! the upstream emits. Common shapes (per witr's `pkg/model/filecontext.go`):
//!
//! - `Path`, `Type` (file vs lock), `Holders` (array of PIDs), `Mode`.
//!
//! We dump the top-level keys as a `key: value` listing rather than
//! try to lay out a table — until cfx.6 refines `FileContext` into
//! a typed struct, the surface is best-effort. When `FileContext`
//! is null/missing, render an empty-state hint.

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
    let mut y = origin_y;
    let last_y = origin_y.saturating_add(area_height);
    paint_row(buf, y, area_width, "File locks & descriptors");
    y = y.saturating_add(1);

    let Some(ctx) = snap.file_context.as_ref() else {
        if y < last_y {
            paint_row(buf, y, area_width, "  (no file_context for this target)");
        }
        return;
    };

    let rows = describe_value(ctx);
    for row in rows {
        if y >= last_y {
            break;
        }
        paint_row(buf, y, area_width, &row);
        y = y.saturating_add(1);
    }
}

/// Maximum lock-array entries we surface inline. cfx.6's detail
/// overlay can drill into the full array when needed; the tab itself
/// stays bounded so a process with 1000 file locks doesn't blow up
/// the render budget.
const MAX_LOCK_ITEMS: usize = 20;

fn describe_value(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                out.push(format!("  {k:<14}: {}", scalar_or_summary(val)));
            }
        }
        Value::Array(items) => {
            out.push(format!("  {} entries:", items.len()));
            for (i, item) in items.iter().enumerate().take(MAX_LOCK_ITEMS) {
                out.push(format!("    [{i}] {}", scalar_or_summary(item)));
            }
            if items.len() > MAX_LOCK_ITEMS {
                out.push(format!("    … and {} more", items.len() - MAX_LOCK_ITEMS));
            }
        }
        _ => out.push(format!("  value: {}", scalar_or_summary(v))),
    }
    out
}

fn scalar_or_summary(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(a) => format!("[{} items]", a.len()),
        Value::Object(o) => format!("{{{} fields}}", o.len()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::test_support::buffer_contains;

    fn snap_with(json: serde_json::Value) -> WitrSnapshot {
        serde_json::from_value(json).expect("fixture parses")
    }

    #[test]
    fn surfaces_no_data_hint_when_file_context_absent() {
        let snap = snap_with(serde_json::json!({
            "Target": {"Type": "name", "Value": "x"},
            "ResolvedTarget": "x",
            "Process": {"PID": 1, "PPID": 0, "Command": "x"},
            "Ancestry": [],
            "Source": {"Type": "unknown", "Name": ""},
            "Warnings": []
        }));
        let mut buf = WireBuffer::new(80, 5);
        render(&mut buf, 0, 5, 80, &snap);
        assert!(buffer_contains(&buf, "no file_context"));
    }

    #[test]
    fn renders_object_keys_when_file_context_is_object() {
        let snap = snap_with(serde_json::json!({
            "Target": {"Type": "file", "Value": "/tmp/x.lock"},
            "ResolvedTarget": "/tmp/x.lock",
            "Process": {"PID": 1, "PPID": 0, "Command": "x"},
            "Ancestry": [],
            "Source": {"Type": "unknown", "Name": ""},
            "Warnings": [],
            "FileContext": {
                "Path": "/tmp/x.lock",
                "Type": "lock",
                "Holders": [1234, 5678]
            }
        }));
        let mut buf = WireBuffer::new(80, 8);
        render(&mut buf, 0, 8, 80, &snap);
        assert!(buffer_contains(&buf, "Path"));
        assert!(buffer_contains(&buf, "/tmp/x.lock"));
        assert!(buffer_contains(&buf, "Type"));
        assert!(buffer_contains(&buf, "lock"));
        assert!(buffer_contains(&buf, "Holders"));
        assert!(buffer_contains(&buf, "[2 items]"));
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
