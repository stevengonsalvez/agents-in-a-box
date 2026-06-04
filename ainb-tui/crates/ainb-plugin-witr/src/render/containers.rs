//! Painter for the Containers tab.
//!
//! Surface area for v1:
//!
//! - `Process.container` — container ID/name if witr resolved one.
//! - `Process.service` — service-manager service if any.
//! - `Source` (kind + name + unit_file) — what put the process there.
//!
//! Witr's container detection lives in `pkg/source/container/` and
//! tags `Source.Type` as `"container"` for docker/podman/crictl
//! discoveries. We surface the typed `Source` plus the `Process`
//! fields rather than try to reconstruct a richer container struct
//! from the opaque corners — cfx.6 detail overlay can drill into
//! `ResourceContext` if needed.

use ainb_plugin_sdk::{Cell, Coord, WireBuffer};

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

    let rows = compose_rows(snap);
    for row in rows {
        if y >= last_y {
            break;
        }
        paint_row(buf, y, area_width, &row);
        y = y.saturating_add(1);
    }
}

fn compose_rows(snap: &WitrSnapshot) -> Vec<String> {
    let mut rows = Vec::with_capacity(8);
    rows.push("Container & service".to_string());
    rows.push(String::new());

    let container = if snap.process.container.is_empty() {
        "-"
    } else {
        snap.process.container.as_str()
    };
    let service = if snap.process.service.is_empty() {
        "-"
    } else {
        snap.process.service.as_str()
    };
    rows.push(format!("  Container : {container}"));
    rows.push(format!("  Service   : {service}"));
    rows.push(String::new());
    rows.push("Source".to_string());
    rows.push(format!("  Kind      : {}", snap.source.kind));
    rows.push(format!(
        "  Name      : {}",
        if snap.source.name.is_empty() {
            "-"
        } else {
            &snap.source.name
        }
    ));
    if !snap.source.unit_file.is_empty() {
        rows.push(format!("  Unit file : {}", snap.source.unit_file));
    }
    if !snap.source.description.is_empty() {
        rows.push(format!("  Note      : {}", snap.source.description));
    }
    rows
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
    fn renders_section_headings_and_dash_when_empty() {
        let snap = snap_with(serde_json::json!({
            "Target": {"Type": "name", "Value": "x"},
            "ResolvedTarget": "x",
            "Process": {"PID": 1, "PPID": 0, "Command": "x"},
            "Ancestry": [],
            "Source": {"Type": "unknown", "Name": ""},
            "Warnings": []
        }));
        let mut buf = WireBuffer::new(80, 10);
        render(&mut buf, 0, 10, 80, &snap);
        assert!(buffer_contains(&buf, "Container & service"));
        assert!(buffer_contains(&buf, "Container : -"));
        assert!(buffer_contains(&buf, "Service   : -"));
        assert!(buffer_contains(&buf, "Source"));
        assert!(buffer_contains(&buf, "Kind      : unknown"));
    }

    #[test]
    fn renders_typed_container_and_service_when_present() {
        let snap = snap_with(serde_json::json!({
            "Target": {"Type": "container", "Value": "abc"},
            "ResolvedTarget": "abc",
            "Process": {
                "PID": 1234,
                "PPID": 1,
                "Command": "redis",
                "Container": "redis-prod",
                "Service": "redis.service"
            },
            "Ancestry": [],
            "Source": {
                "Type": "container",
                "Name": "redis-prod",
                "Description": "podman container",
                "UnitFile": ""
            },
            "Warnings": []
        }));
        let mut buf = WireBuffer::new(80, 12);
        render(&mut buf, 0, 12, 80, &snap);
        assert!(buffer_contains(&buf, "Container : redis-prod"));
        assert!(buffer_contains(&buf, "Service   : redis.service"));
        assert!(buffer_contains(&buf, "Kind      : container"));
        assert!(buffer_contains(&buf, "Note      : podman container"));
    }

    #[test]
    fn surfaces_unit_file_only_when_set() {
        let snap = snap_with(serde_json::json!({
            "Target": {"Type": "name", "Value": "nginx"},
            "ResolvedTarget": "nginx",
            "Process": {"PID": 1234, "PPID": 1, "Command": "nginx"},
            "Ancestry": [],
            "Source": {
                "Type": "systemd",
                "Name": "nginx",
                "UnitFile": "/etc/systemd/system/nginx.service"
            },
            "Warnings": []
        }));
        let mut buf = WireBuffer::new(80, 12);
        render(&mut buf, 0, 12, 80, &snap);
        assert!(buffer_contains(
            &buf,
            "Unit file : /etc/systemd/system/nginx.service"
        ));
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
