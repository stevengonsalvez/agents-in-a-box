//! Render the radial ego map into a ratatui [`Buffer`].
//!
//! Draw order is deliberate so nothing important gets clobbered:
//!
//! 1. **edges** — an ASCII line per edge (glyph chosen by slope), skipping the
//!    cells near each endpoint so a line never stabs into a node box.
//! 2. **edge decorations** — the coloured `rel_type` label at the line midpoint
//!    and a per-direction arrowhead just outside the arrow's target box.
//! 3. **node boxes** — `[label]` drawn last so they sit on top of any line.
//! 4. **header + help bar** — chrome above/below the canvas.
//!
//! Determinism: positions come straight from [`layout`], so the rendered token
//! set is reproducible and the tripwires assert exact `[entity]` / `rel_type` /
//! arrowhead tokens.

use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect as RRect};
use ratatui::style::{Color as RColor, Modifier as RModifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::ui::{CORNFLOWER_BLUE, GOLD, MUTED_GRAY, SELECTION_GREEN, SOFT_WHITE};

use super::ego::{Arrow, EgoSubgraph};
use super::layout::{Placed, layout};

/// Longest entity label rendered inside a `[…]` box (char count, pre-brackets).
/// Wide enough for every fixture entity; longer names truncate with `…`.
const MAX_LABEL: usize = 26;

/// Split `area` into the header line, the radial canvas, and the help bar.
/// Shared by [`render`] and the mouse hit-test so both agree on the canvas rect.
#[must_use]
pub fn split_area(area: RRect) -> [RRect; 3] {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // radial canvas
            Constraint::Length(1), // help bar
        ])
        .split(area);
    [rows[0], rows[1], rows[2]]
}

/// Lay the ego subgraph out within the canvas rect of `area`. Returns the placed
/// nodes in canvas-relative coordinates (add the canvas origin for absolute
/// cells). Shared by [`render`] and the hit-test so a click resolves against the
/// exact geometry that was painted.
#[must_use]
pub fn layout_in(area: RRect, ego: &EgoSubgraph) -> (RRect, Vec<Placed>) {
    let canvas = split_area(area)[1];
    let placed = layout(ego, canvas.width, canvas.height);
    (canvas, placed)
}

/// Render the radial map for `ego` with `selected` highlighted (matched by node
/// name). `hop` / overflow counts drive the header.
pub fn render(buf: &mut RBuffer, area: RRect, ego: &EgoSubgraph, selected: &str) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let [header, canvas, help] = split_area(area);

    render_header(buf, header, ego);

    if canvas.width > 0 && canvas.height > 0 {
        let placed = layout(ego, canvas.width, canvas.height);
        // 1 + 2: edges then their decorations.
        for edge in &ego.edges {
            if let (Some(from), Some(to)) = (find(&placed, &edge.from), find(&placed, &edge.to)) {
                draw_edge(buf, canvas, from, to, &edge.rel_type, edge.arrow);
            }
        }
        // 3: boxes on top.
        for node in &placed {
            draw_box(buf, canvas, node, selected);
        }
        if ego.nodes.len() == 1 {
            // Isolated centre: a hint under the lone box.
            let hint_y = canvas.y.saturating_add(canvas.height / 2).saturating_add(2);
            put_str(
                buf,
                canvas,
                canvas.x + 2,
                hint_y,
                "(no connections)",
                Style::default().fg(MUTED_GRAY).add_modifier(RModifier::ITALIC),
            );
        }
    }

    render_help(buf, help);
}

/// Header: `centre: <name>   hop:N   nodes:M (+K)`.
fn render_header(buf: &mut RBuffer, area: RRect, ego: &EgoSubgraph) {
    let shown = ego.nodes.iter().filter(|n| !n.overflow && n.ring > 0).count();
    let overflow: usize = ego.nodes.iter().filter(|n| n.overflow).map(|n| n.overflow_count).sum();
    let mut spans = vec![
        Span::styled("centre: ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            ego.center.clone(),
            Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
        ),
        Span::styled(
            format!("   hop:{}", ego.hop),
            Style::default().fg(SOFT_WHITE),
        ),
        Span::styled(format!("   nodes:{shown}"), Style::default().fg(SOFT_WHITE)),
    ];
    if overflow > 0 {
        spans.push(Span::styled(
            format!(" (+{overflow})"),
            Style::default().fg(MUTED_GRAY),
        ));
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// Help bar honest about Backspace (Esc is host-reserved).
fn render_help(buf: &mut RBuffer, area: RRect) {
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in [
        ("↑↓←→", "move"),
        ("⏎", "recentre"),
        ("h", "hops"),
        ("e", "expand"),
        ("o", "open"),
        ("v", "view"),
        ("Bksp", "back"),
    ] {
        spans.push(Span::styled(
            k.to_string(),
            Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {d}  "),
            Style::default().fg(MUTED_GRAY),
        ));
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// Find a placed node by name.
fn find<'a>(placed: &'a [Placed], name: &str) -> Option<&'a Placed> {
    placed.iter().find(|p| p.name == name)
}

/// The bracketed box text for a node, char-truncated to [`MAX_LABEL`].
fn box_text(name: &str) -> String {
    format!("[{}]", truncate_chars(name, MAX_LABEL))
}

/// Half the box width in cells (for centering + endpoint skip distance).
fn box_half(name: &str) -> u16 {
    (box_text(name).chars().count() as u16) / 2
}

/// Draw a node's `[label]` box centred on its anchor, clamped into the canvas.
/// Centre = gold/bold, selected = green highlight, overflow = muted, else white.
fn draw_box(buf: &mut RBuffer, canvas: RRect, node: &Placed, selected: &str) {
    let text = box_text(&node.name);
    let len = text.chars().count() as u16;
    let half = len / 2;
    // Centre the box on the anchor, then clamp so it stays fully inside.
    let rel_x = node.x.saturating_sub(half);
    let max_x = canvas.width.saturating_sub(len);
    let rel_x = rel_x.min(max_x);
    let abs_x = canvas.x + rel_x;
    let abs_y = canvas.y + node.y.min(canvas.height.saturating_sub(1));

    let is_centre = node.ring == 0;
    let is_selected = node.name == selected;
    let style = if is_selected {
        Style::default()
            .fg(SELECTION_GREEN)
            .add_modifier(RModifier::BOLD | RModifier::REVERSED)
    } else if is_centre {
        Style::default().fg(GOLD).add_modifier(RModifier::BOLD)
    } else if node.overflow {
        Style::default().fg(MUTED_GRAY).add_modifier(RModifier::ITALIC)
    } else {
        Style::default().fg(SOFT_WHITE)
    };
    put_str(buf, canvas, abs_x, abs_y, &text, style);
}

/// Draw one edge: an ASCII line between the two box anchors (skipping the cells
/// covered by each box), a coloured `rel_type` label at the midpoint, and a
/// per-direction arrowhead just outside the arrow's target box.
fn draw_edge(
    buf: &mut RBuffer,
    canvas: RRect,
    from: &Placed,
    to: &Placed,
    rel_type: &str,
    arrow: Arrow,
) {
    let (x0, y0) = (i32::from(from.x), i32::from(from.y));
    let (x1, y1) = (i32::from(to.x), i32::from(to.y));
    let dx = x1 - x0;
    let dy = y1 - y0;
    if dx == 0 && dy == 0 {
        return;
    }

    let glyph = line_glyph(dx, dy);
    let from_skip = i32::from(box_half(&from.name)) + 1;
    let to_skip = i32::from(box_half(&to.name)) + 1;
    let len = ((dx * dx + dy * dy) as f64).sqrt().max(1.0);

    // Walk the line in float steps; stamp the glyph on cells outside both boxes.
    let steps = dx.abs().max(dy.abs()).max(1);
    for s in 0..=steps {
        let t = f64::from(s) / f64::from(steps);
        let px = x0 as f64 + f64::from(dx) * t;
        let py = y0 as f64 + f64::from(dy) * t;
        let d_from = (f64::from(s) / f64::from(steps)) * len;
        let d_to = len - d_from;
        if d_from <= f64::from(from_skip) || d_to <= f64::from(to_skip) {
            continue;
        }
        put_cell(
            buf,
            canvas,
            px.round() as i32,
            py.round() as i32,
            glyph,
            Style::default().fg(MUTED_GRAY),
        );
    }

    // Midpoint coloured type label.
    let mid_x = (x0 + x1) / 2;
    let mid_y = (y0 + y1) / 2;
    put_str(
        buf,
        canvas,
        canvas.x.saturating_add(clamp_u16(mid_x, canvas.width)),
        canvas.y.saturating_add(clamp_u16(mid_y, canvas.height)),
        rel_type,
        Style::default().fg(rel_color(rel_type)).add_modifier(RModifier::BOLD),
    );

    // Arrowhead just outside the target box (Forward → near `to`; Backward →
    // near `from`; None → no head).
    match arrow {
        Arrow::None => {}
        Arrow::Forward => {
            let t = ((len - f64::from(to_skip)) / len).clamp(0.0, 1.0);
            let ax = x0 as f64 + f64::from(dx) * t;
            let ay = y0 as f64 + f64::from(dy) * t;
            put_cell(
                buf,
                canvas,
                ax.round() as i32,
                ay.round() as i32,
                arrow_glyph(dx, dy),
                arrow_style(),
            );
        }
        Arrow::Backward => {
            let t = (f64::from(from_skip) / len).clamp(0.0, 1.0);
            let ax = x0 as f64 + f64::from(dx) * t;
            let ay = y0 as f64 + f64::from(dy) * t;
            // Points back toward `from`: negate the vector.
            put_cell(
                buf,
                canvas,
                ax.round() as i32,
                ay.round() as i32,
                arrow_glyph(-dx, -dy),
                arrow_style(),
            );
        }
    }
}

fn arrow_style() -> Style {
    Style::default().fg(GOLD).add_modifier(RModifier::BOLD)
}

/// Colour a relationship-type label by its kind.
fn rel_color(rel_type: &str) -> RColor {
    match rel_type {
        "solves" => SELECTION_GREEN,
        "caused_by" | "causes" => RColor::Rgb(230, 120, 120), // soft red
        "requires" => CORNFLOWER_BLUE,
        _ => MUTED_GRAY, // relates_to / unknown
    }
}

/// Line glyph by dominant slope.
fn line_glyph(dx: i32, dy: i32) -> &'static str {
    if dx.abs() >= dy.abs() * 2 {
        "─"
    } else if dy.abs() >= dx.abs() * 2 {
        "│"
    } else if (dx > 0) == (dy > 0) {
        "╲" // same sign → down-right / up-left
    } else {
        "╱"
    }
}

/// 8-way arrowhead glyph for a direction vector (screen coords, y down).
fn arrow_glyph(dx: i32, dy: i32) -> &'static str {
    use std::f64::consts::PI;
    // Math angle with y flipped (up = +).
    let ang = f64::from(-dy).atan2(f64::from(dx)); // (-PI, PI]
    let sector = (((ang / (PI / 4.0)).round() as i32) % 8 + 8) % 8;
    match sector {
        0 => "→",
        1 => "↗",
        2 => "↑",
        3 => "↖",
        4 => "←",
        5 => "↙",
        6 => "↓",
        _ => "↘", // 7
    }
}

/// Write a string at absolute `(x, y)` if it falls inside `area`, clipping the
/// tail to the area's right edge. Char-safe (never byte-slices).
fn put_str(buf: &mut RBuffer, area: RRect, x: u16, y: u16, s: &str, style: Style) {
    if y < area.y || y >= area.y.saturating_add(area.height) {
        return;
    }
    if x < area.x {
        return;
    }
    let right = area.x.saturating_add(area.width);
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        let cell = buf.get_mut(cx, y);
        cell.set_symbol(&ch.to_string());
        cell.set_style(style);
        cx = cx.saturating_add(1);
    }
}

/// Set a single cell at absolute `(x, y)` (i32 in, bounds-checked against area).
fn put_cell(buf: &mut RBuffer, area: RRect, x: i32, y: i32, glyph: &str, style: Style) {
    if x < i32::from(area.x) || y < i32::from(area.y) {
        return;
    }
    let (x, y) = (x as u16, y as u16);
    if x >= area.x.saturating_add(area.width) || y >= area.y.saturating_add(area.height) {
        return;
    }
    let cell = buf.get_mut(x, y);
    cell.set_symbol(glyph);
    cell.set_style(style);
}

/// Clamp an i32 cell coord into `0..max` and return it as `u16` (canvas-relative).
fn clamp_u16(v: i32, max: u16) -> u16 {
    if v < 0 {
        0
    } else if v >= i32::from(max) {
        max.saturating_sub(1)
    } else {
        v as u16
    }
}

/// Char-based truncate with an ellipsis (never byte-slices — UTF-8 safe).
fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Relationship;
    use crate::ui::map::ego::DEFAULT_NODE_CAP;

    fn rel(source: &str, target: &str, rel_type: &str) -> Relationship {
        Relationship {
            source: source.into(),
            target: target.into(),
            rel_type: rel_type.into(),
            description: String::new(),
            strength: Some(5),
        }
    }

    /// Flatten the buffer to a string of cell symbols, row by row.
    fn dump(buf: &RBuffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf.get(x, y).symbol());
            }
            out.push('\n');
        }
        out
    }

    /// Flatten ONLY the radial canvas rows — excludes the header and the help
    /// bar, whose `↑↓←→` legend would otherwise false-match arrowhead checks.
    fn dump_canvas(buf: &RBuffer) -> String {
        let canvas = split_area(buf.area)[1];
        let mut out = String::new();
        for y in canvas.y..canvas.y.saturating_add(canvas.height) {
            for x in canvas.x..canvas.x.saturating_add(canvas.width) {
                out.push_str(buf.get(x, y).symbol());
            }
            out.push('\n');
        }
        out
    }

    fn render_fixture_buf(center: &str) -> RBuffer {
        let rels = vec![
            rel("audit-after-rebase", "stale plan execution", "solves"),
            rel("stale plan execution", "git pull --rebase", "caused_by"),
        ];
        let ego = EgoSubgraph::build(&rels, center, 1, DEFAULT_NODE_CAP, false);
        let area = RRect::new(0, 0, 80, 24);
        let mut buf = RBuffer::empty(area);
        render(&mut buf, area, &ego, center);
        buf
    }

    fn render_fixture(center: &str) -> String {
        dump(&render_fixture_buf(center))
    }

    #[test]
    fn renders_centre_and_neighbor_boxes() {
        let screen = render_fixture("audit-after-rebase");
        assert!(
            screen.contains("[audit-after-rebase]"),
            "centre box:\n{screen}"
        );
        assert!(
            screen.contains("[stale plan execution]"),
            "neighbour box:\n{screen}"
        );
    }

    #[test]
    fn renders_typed_edge_label() {
        let screen = render_fixture("audit-after-rebase");
        assert!(screen.contains("solves"), "edge type label:\n{screen}");
    }

    #[test]
    fn renders_an_arrowhead_for_directed_edge() {
        // Scope to the canvas so the help-bar `↑↓←→` legend can't false-match.
        let canvas = dump_canvas(&render_fixture_buf("audit-after-rebase"));
        let arrows = ['→', '←', '↑', '↓', '↗', '↖', '↙', '↘'];
        assert!(
            arrows.iter().any(|a| canvas.contains(*a)),
            "expected a directional arrowhead in the canvas:\n{canvas}"
        );
    }

    #[test]
    fn header_shows_centre_and_hop() {
        let screen = render_fixture("audit-after-rebase");
        assert!(
            screen.contains("centre: audit-after-rebase"),
            "header:\n{screen}"
        );
        assert!(screen.contains("hop:1"), "hop in header:\n{screen}");
    }

    #[test]
    fn relates_to_edge_has_no_arrowhead() {
        let rels = vec![rel("c", "peer", "relates_to")];
        let ego = EgoSubgraph::build(&rels, "c", 1, DEFAULT_NODE_CAP, false);
        let area = RRect::new(0, 0, 80, 24);
        let mut buf = RBuffer::empty(area);
        render(&mut buf, area, &ego, "c");
        let canvas = dump_canvas(&buf);
        let arrows = ['→', '←', '↑', '↓', '↗', '↖', '↙', '↘'];
        assert!(
            !arrows.iter().any(|a| canvas.contains(*a)),
            "relates_to must render no arrowhead in the canvas:\n{canvas}"
        );
        assert!(
            dump(&buf).contains("relates_to"),
            "still labels the edge:\n{}",
            dump(&buf)
        );
    }

    #[test]
    fn isolated_centre_shows_no_connections_hint() {
        let rels = vec![rel("other-a", "other-b", "solves")];
        let ego = EgoSubgraph::build(&rels, "lonely", 1, DEFAULT_NODE_CAP, false);
        let area = RRect::new(0, 0, 80, 24);
        let mut buf = RBuffer::empty(area);
        render(&mut buf, area, &ego, "lonely");
        let screen = dump(&buf);
        assert!(screen.contains("[lonely]"), "lone centre box:\n{screen}");
        assert!(screen.contains("(no connections)"), "hint:\n{screen}");
    }

    #[test]
    fn arrow_glyph_maps_cardinal_directions() {
        assert_eq!(arrow_glyph(5, 0), "→");
        assert_eq!(arrow_glyph(-5, 0), "←");
        assert_eq!(arrow_glyph(0, -5), "↑"); // up = negative screen-y
        assert_eq!(arrow_glyph(0, 5), "↓");
    }
}
