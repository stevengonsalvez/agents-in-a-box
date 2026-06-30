//! File-tree widget — renders a skill's flat file list as an indented tree.
//!
//! The skill manager (P4.6) shows the selected skill's files in the middle pane.
//! The daemon supplies a flat list of paths ([`SkillFile`]); this widget splits
//! each on `/` and paints an indented tree (directories implied by path
//! segments). Pure render — it holds no state and does no IO; selection lives in
//! the owning screen.
//!
//! Reused by the P8 daemon-health log-file viewer.

use ainb_hangar_proto::events::SkillFile;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Cornflower-blue for directory segments.
const DIR: Color = Color::rgb(100, 149, 237);
/// Soft-white for file names.
const FILE: Color = Color::rgb(220, 220, 230);
/// Selection accent (`▶`).
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);

/// Render the `files` as an indented tree at `(x, top)` within `width` cells,
/// stopping before `bottom`. `selected` highlights the file at that flat index
/// with a `▶` marker.
///
/// Each path is split on `/`; the final segment is the file (painted white),
/// preceding segments are indentation depth (two spaces per level). A leading
/// directory line is emitted whenever the directory prefix changes from the
/// previous file, so `assets/a.md` + `assets/b.md` share one `assets/` header.
pub fn render_file_tree(
    buf: &mut WireBuffer,
    x: u16,
    top: u16,
    bottom: u16,
    width: u16,
    files: &[SkillFile],
    selected: usize,
) {
    let right = x.saturating_add(width);
    let mut row = top;
    let mut prev_dir: Vec<&str> = Vec::new();
    for (i, file) in files.iter().enumerate() {
        if row >= bottom {
            break;
        }
        let segments: Vec<&str> = file.path.split('/').collect();
        let (dirs, name) = segments.split_at(segments.len().saturating_sub(1));
        // Emit directory headers for any new prefix segment.
        for (depth, dir) in dirs.iter().enumerate() {
            if prev_dir.get(depth) != Some(dir) {
                if row >= bottom {
                    break;
                }
                let indent = x + u16::try_from(depth * 2).unwrap_or(0);
                put_str(buf, indent, row, &format!("{dir}/"), DIR, right);
                row += 1;
            }
        }
        prev_dir = dirs.to_vec();
        if row >= bottom {
            break;
        }
        let indent = x + u16::try_from(dirs.len() * 2).unwrap_or(0);
        let mut cx = indent;
        if i == selected {
            cx = put_char(buf, cx, row, '▶', SELECTION_GREEN, right);
        } else {
            cx = put_char(buf, cx, row, ' ', FILE, right);
        }
        cx = put_char(buf, cx, row, ' ', FILE, right);
        put_str(
            buf,
            cx,
            row,
            name.first().copied().unwrap_or(""),
            FILE,
            right,
        );
        row += 1;
    }
}

/// Write `s` at `(x, row)` in `color`, clipping at `right`. Returns next column.
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        cx = put_char(buf, cx, row, ch, color, right);
    }
    cx
}

/// Write one `ch` at `(x, row)` in `color`, clipping at `right`.
fn put_char(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color, right: u16) -> u16 {
    if x >= right {
        return x;
    }
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
    x.saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_text(buf: &WireBuffer, row: u16, width: u16) -> String {
        let mut out = vec![' '; width as usize];
        for (coord, cell) in &buf.cells {
            if coord.y == row && coord.x < width {
                if let Some(ch) = cell.symbol.chars().next() {
                    out[coord.x as usize] = ch;
                }
            }
        }
        out.into_iter().collect()
    }

    /// Files in the same directory share a single directory header.
    #[test]
    fn shared_directory_header_emitted_once() {
        let files = vec![
            SkillFile {
                path: "SKILL.md".into(),
            },
            SkillFile {
                path: "assets/a.md".into(),
            },
            SkillFile {
                path: "assets/b.md".into(),
            },
        ];
        let mut buf = WireBuffer::new(30, 10);
        render_file_tree(&mut buf, 0, 0, 10, 30, &files, 0);
        // Row 0: SKILL.md ; row 1: assets/ ; row 2: a.md ; row 3: b.md
        assert!(row_text(&buf, 0, 30).contains("SKILL.md"));
        assert!(row_text(&buf, 1, 30).contains("assets/"));
        assert!(row_text(&buf, 2, 30).contains("a.md"));
        assert!(row_text(&buf, 3, 30).contains("b.md"));
        // `assets/` is not repeated for b.md.
        assert!(!row_text(&buf, 3, 30).contains("assets/"));
    }
}
