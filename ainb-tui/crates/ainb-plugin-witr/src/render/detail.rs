//! Detail overlay — the `/`-key modal showing the full field set of
//! the target process.
//!
//! Painted *over* the current tab body (the tab content is rendered
//! first, then this clears a centered box and draws the modal on top
//! — the underlying tab + selected tab survive, so closing with `q`
//! drops straight back to where the user was).
//!
//! Content is the snapshot's primary [`Process`](crate::model::Process)
//! expanded: every typed field plus a socket count, capabilities,
//! warnings, and presence flags for the opaque
//! `ResourceContext`/`FileContext` corners (cfx.3 holds those as
//! `Option<Value>`; the overlay just reports present/absent + a short
//! summary rather than decoding them — that typed refinement is a
//! post-v1 follow-up).
//!
//! Direct WireBuffer painting (no ratatui): a single centered modal
//! is a bordered box + key:value lines, well within reach of absolute
//! cell pushes and keeps the render layer dependency-consistent with
//! cfx.4/cfx.5.

use ainb_plugin_sdk::{Cell, Coord, Viewport, WireBuffer};

use crate::model::WitrSnapshot;

/// Largest modal we'll draw. Real terminals are wider, but a 64-col
/// modal keeps line lengths readable and leaves margin on both sides.
const MAX_MODAL_WIDTH: u16 = 64;
/// Margin between the modal edge and the viewport edge on each side.
const VIEWPORT_MARGIN: u16 = 2;
/// Vertical rows the modal chrome consumes (2 border rows top+bottom,
/// plus 2 vertical-margin rows). Subtracted from viewport height to
/// get the interior-row budget.
const VERTICAL_CHROME: u16 = 4;

/// Paint the detail overlay for `snap`'s primary process, centered in
/// `viewport`, over whatever's already in `buf`.
///
/// No-op on viewports too small to host even a minimal box (a 1-row
/// or 1-col viewport just keeps the tab body).
pub fn render_overlay(buf: &mut WireBuffer, viewport: Viewport, snap: &WitrSnapshot) {
    let lines = compose_detail(snap);
    paint_modal(buf, viewport, "Process detail (q to close)", &lines);
}

/// Build the detail body as a flat list of display lines. Pure — the
/// whole field-selection contract is unit-testable without painting.
pub fn compose_detail(snap: &WitrSnapshot) -> Vec<String> {
    let p = &snap.process;
    let mut out = Vec::with_capacity(24);

    out.push(format!("PID        {}   PPID {}", p.pid, p.ppid));
    out.push(format!("Command    {}", dash_if_empty(&p.command)));
    out.push(format!("Cmdline    {}", dash_if_empty(&p.cmdline)));
    out.push(format!("Exe        {}", dash_if_empty(&p.exe)));
    out.push(format!(
        "StartedAt  {}",
        p.started_at.as_deref().unwrap_or("-")
    ));
    out.push(format!("User       {}", dash_if_empty(&p.user)));
    out.push(format!(
        "CPU / Mem  {:.1}%  /  {:.1}%",
        p.cpu_percent, p.memory_percent
    ));
    out.push(format!("WorkingDir {}", dash_if_empty(&p.working_dir)));
    out.push(format!("GitRepo    {}", dash_if_empty(&p.git_repo)));
    out.push(format!("GitBranch  {}", dash_if_empty(&p.git_branch)));
    out.push(format!("Container  {}", dash_if_empty(&p.container)));
    out.push(format!("Service    {}", dash_if_empty(&p.service)));
    out.push(format!("Health     {}", dash_if_empty(&p.health)));
    out.push(format!("Forked     {}", dash_if_empty(&p.forked)));
    out.push(format!("Sockets    {} open", p.sockets.len()));

    if !p.capabilities.is_empty() {
        out.push(format!("Caps       {}", p.capabilities.join(", ")));
    }

    // Source context — what put the process there.
    out.push(String::new());
    out.push(format!(
        "Source     {} {}",
        dash_if_empty(&snap.source.kind),
        dash_if_empty(&snap.source.name)
    ));

    // Opaque nested corners — report presence, don't decode.
    out.push(format!(
        "ResourceCtx {}   FileCtx {}",
        present_flag(snap.resource_context.is_some()),
        present_flag(snap.file_context.is_some()),
    ));

    // Warnings last — the most action-relevant lines.
    if snap.warnings.is_empty() {
        out.push("Warnings   none".to_string());
    } else {
        out.push("Warnings".to_string());
        for w in &snap.warnings {
            out.push(format!("  ! {w}"));
        }
    }

    out
}

fn dash_if_empty(s: &str) -> &str {
    if s.is_empty() { "-" } else { s }
}

fn present_flag(present: bool) -> &'static str {
    if present { "present" } else { "-" }
}

/// Paint a centered, bordered modal box containing `title` + `lines`,
/// clearing the area beneath it so the tab body doesn't bleed through.
fn paint_modal(buf: &mut WireBuffer, viewport: Viewport, title: &str, lines: &[String]) {
    let vw = viewport.width;
    let vh = viewport.height;
    // Need room for at least a border (2x2) + one content row.
    if vw < 2 * VIEWPORT_MARGIN + 3 || vh < 5 {
        return;
    }

    let modal_w = (vw - 2 * VIEWPORT_MARGIN).min(MAX_MODAL_WIDTH);
    // Inner content width (inside the two border columns + 1 pad each side).
    let inner_w = modal_w.saturating_sub(4);

    // Height: title row + content rows + 2 border rows, capped to fit.
    let want_rows = lines.len() as u16 + 1; // +1 title
    let max_inner_rows = vh.saturating_sub(VERTICAL_CHROME);
    let inner_rows = want_rows.min(max_inner_rows).max(1);
    let modal_h = inner_rows + 2;

    let x0 = (vw.saturating_sub(modal_w)) / 2;
    let y0 = (vh.saturating_sub(modal_h)) / 2;

    // 1. Clear interior + border cells (so underlying tab text is hidden).
    for dy in 0..modal_h {
        for dx in 0..modal_w {
            put(buf, x0 + dx, y0 + dy, ' ');
        }
    }

    // 2. Border.
    draw_border(buf, x0, y0, modal_w, modal_h);

    // 3. Title row (row y0+1, inside left pad).
    let tx = x0 + 2;
    let mut content_y = y0 + 1;
    paint_clipped(buf, tx, content_y, inner_w, title);
    content_y += 1;

    // 4. Content rows, clipped to inner_w, stopping before bottom border.
    let last_content_y = y0 + modal_h - 1; // bottom border row
    for (idx, line) in lines.iter().enumerate() {
        if content_y >= last_content_y {
            break;
        }
        // If more lines remain than rows left, spend the last usable
        // row on a "… N more" indicator instead of a content line —
        // matches the truncation convention in `locks.rs`.
        let rows_left = last_content_y - content_y;
        let lines_left = (lines.len() - idx) as u16;
        if rows_left == 1 && lines_left > 1 {
            paint_clipped(
                buf,
                tx,
                content_y,
                inner_w,
                &format!("… {} more", lines_left),
            );
            break;
        }
        paint_clipped(buf, tx, content_y, inner_w, line);
        content_y += 1;
    }
}

fn draw_border(buf: &mut WireBuffer, x0: u16, y0: u16, w: u16, h: u16) {
    let right = x0 + w - 1;
    let bottom = y0 + h - 1;
    put(buf, x0, y0, '┌');
    put(buf, right, y0, '┐');
    put(buf, x0, bottom, '└');
    put(buf, right, bottom, '┘');
    for dx in 1..w - 1 {
        put(buf, x0 + dx, y0, '─');
        put(buf, x0 + dx, bottom, '─');
    }
    for dy in 1..h - 1 {
        put(buf, x0, y0 + dy, '│');
        put(buf, right, y0 + dy, '│');
    }
}

fn paint_clipped(buf: &mut WireBuffer, x0: u16, y: u16, max_w: u16, text: &str) {
    let mut x = x0;
    let limit = x0.saturating_add(max_w);
    for ch in text.chars() {
        if x >= limit {
            break;
        }
        put(buf, x, y, ch);
        x = x.saturating_add(1);
    }
}

fn put(buf: &mut WireBuffer, x: u16, y: u16, ch: char) {
    buf.push(Coord::new(x, y), Cell::new(ch.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::test_support::{buffer_contains, flatten_rows_trimmed};

    fn snap(json: serde_json::Value) -> WitrSnapshot {
        serde_json::from_value(json).expect("fixture parses")
    }

    fn rich() -> WitrSnapshot {
        snap(serde_json::json!({
            "Target": {"Type": "pid", "Value": "1234"},
            "ResolvedTarget": "1234",
            "Process": {
                "PID": 1234, "PPID": 800, "Command": "nginx",
                "Cmdline": "nginx: master process",
                "Exe": "/usr/sbin/nginx",
                "StartedAt": "2025-04-10T12:00:00Z",
                "User": "root", "CPUPercent": 1.5_f64, "MemoryPercent": 2.3_f64,
                "WorkingDir": "/etc/nginx", "GitRepo": "", "GitBranch": "",
                "Container": "", "Service": "nginx.service",
                "Health": "healthy", "Forked": "not-forked",
                "Sockets": [{"Port": 80}, {"Port": 443}],
                "Capabilities": ["CAP_NET_BIND_SERVICE"]
            },
            "Ancestry": [],
            "Source": {"Type": "systemd", "Name": "nginx.service"},
            "Warnings": ["running as root"]
        }))
    }

    #[test]
    fn compose_includes_all_typed_fields() {
        let lines = compose_detail(&rich());
        let blob = lines.join("\n");
        for needle in [
            "PID",
            "1234",
            "PPID",
            "800",
            "nginx",
            "master process",
            "/usr/sbin/nginx",
            "2025-04-10T12:00:00Z",
            "root",
            "nginx.service",
            "healthy",
            "not-forked",
            "Sockets    2 open",
            "CAP_NET_BIND_SERVICE",
        ] {
            assert!(
                blob.contains(needle),
                "compose must include {needle:?}: {blob}"
            );
        }
    }

    #[test]
    fn compose_surfaces_warnings() {
        let lines = compose_detail(&rich());
        assert!(lines.iter().any(|l| l.contains("running as root")));
    }

    #[test]
    fn compose_says_none_when_no_warnings() {
        let s = snap(serde_json::json!({
            "Target": {"Type": "pid", "Value": "1"},
            "ResolvedTarget": "1",
            "Process": {"PID": 1, "PPID": 0, "Command": "init"},
            "Ancestry": [],
            "Source": {"Type": "init", "Name": "init"},
            "Warnings": []
        }));
        let lines = compose_detail(&s);
        assert!(lines.iter().any(|l| l == "Warnings   none"));
    }

    #[test]
    fn compose_reports_opaque_context_presence() {
        let with_ctx = snap(serde_json::json!({
            "Target": {"Type": "file", "Value": "/x"},
            "ResolvedTarget": "/x",
            "Process": {"PID": 1, "PPID": 0, "Command": "x"},
            "Ancestry": [],
            "Source": {"Type": "unknown", "Name": ""},
            "Warnings": [],
            "ResourceContext": {"cpu": 1},
            "FileContext": {"path": "/x"}
        }));
        let lines = compose_detail(&with_ctx).join("\n");
        assert!(lines.contains("ResourceCtx present"));
        assert!(lines.contains("FileCtx present"));
    }

    #[test]
    fn compose_dashes_absent_opaque_context() {
        let lines = compose_detail(&rich()).join("\n");
        // rich() has neither ResourceContext nor FileContext.
        assert!(lines.contains("ResourceCtx -"));
        assert!(lines.contains("FileCtx -"));
    }

    #[test]
    fn render_overlay_paints_bordered_modal() {
        let mut buf = WireBuffer::new(80, 30);
        render_overlay(
            &mut buf,
            Viewport {
                width: 80,
                height: 30,
            },
            &rich(),
        );
        assert!(buffer_contains(&buf, "Process detail"));
        assert!(buffer_contains(&buf, "1234"));
        assert!(buffer_contains(&buf, "nginx"));
        // Border glyphs present.
        let rows = flatten_rows_trimmed(&buf);
        let joined = rows.join("\n");
        assert!(joined.contains('┌') && joined.contains('┘'), "border drawn");
    }

    #[test]
    fn render_overlay_clears_underlying_text_behind_modal() {
        // Pre-fill the buffer with junk, then overlay — cells under
        // the modal interior must be overwritten (cleared to space or
        // modal content), not show the junk.
        let mut buf = WireBuffer::new(80, 30);
        for x in 0..80 {
            for y in 0..30 {
                put(&mut buf, x, y, 'X');
            }
        }
        render_overlay(
            &mut buf,
            Viewport {
                width: 80,
                height: 30,
            },
            &rich(),
        );
        // The modal is centered; its center cell must not be 'X'.
        // Last-write-wins on (x,y) so we inspect the final value at center.
        let cx = 40u16;
        let cy = 15u16;
        let final_at_center: Option<&str> = buf
            .cells
            .iter()
            .rev()
            .find(|(c, _)| c.x == cx && c.y == cy)
            .map(|(_, cell)| cell.symbol.as_str());
        assert_ne!(
            final_at_center,
            Some("X"),
            "modal must clear/overwrite the cell beneath its center"
        );
    }

    #[test]
    fn render_overlay_no_panic_on_tiny_viewport() {
        for (w, h) in [(1, 1), (4, 2), (10, 4), (0, 0), (3, 5)] {
            let mut buf = WireBuffer::new(w, h);
            render_overlay(
                &mut buf,
                Viewport {
                    width: w,
                    height: h,
                },
                &rich(),
            );
            // No panic; any painted cell stays in-bounds.
            for (c, _) in &buf.cells {
                assert!(c.x < w && c.y < h, "cell ({},{}) out of {w}x{h}", c.x, c.y);
            }
        }
    }

    #[test]
    fn render_overlay_shows_more_indicator_when_content_overflows() {
        // Short modal (few interior rows) + a process with many fields
        // forces overflow. The last usable row must show "… N more"
        // rather than silently dropping lines.
        let mut buf = WireBuffer::new(60, 9); // ~5 interior rows
        render_overlay(
            &mut buf,
            Viewport {
                width: 60,
                height: 9,
            },
            &rich(),
        );
        assert!(
            buffer_contains(&buf, "more"),
            "overflow must paint a '… N more' indicator: {:?}",
            flatten_rows_trimmed(&buf),
        );
    }

    #[test]
    fn render_overlay_handles_null_nested_without_crash() {
        let s = snap(serde_json::json!({
            "Target": {"Type": "name", "Value": "x"},
            "ResolvedTarget": "x",
            "Process": {"PID": 1, "PPID": 0, "Command": "x"},
            "Ancestry": [],
            "Source": {"Type": "unknown", "Name": ""},
            "Warnings": [],
            "SocketInfo": null,
            "ResourceContext": null,
            "FileContext": null
        }));
        let mut buf = WireBuffer::new(60, 24);
        render_overlay(
            &mut buf,
            Viewport {
                width: 60,
                height: 24,
            },
            &s,
        );
        assert!(buffer_contains(&buf, "Process detail"));
    }
}
