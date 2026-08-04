//! The `ainb hangar pipeline show` renderer: four health lights over a stage
//! strip.
//!
//! ```text
//! ● daemon ok   ● roles covered   ● wip 1/2 Implement   ● 0 stuck
//!
//! Backlog │ Triage    │ Implement │ Review    │ QA       │ Done
//!         │ triager ● │ impl ●    │ review ●  │ test ✗   │
//!    2    │ 1         │ 1 (wip 2) │ 0 (wip 3) │ 0 (wip 1)│ 4
//! ```
//!
//! Pure formatting only: it takes an already-computed
//! [`PipelineHealth`](ainb_hangar_store::service::pipeline_health::PipelineHealth)
//! plus the caller's daemon-liveness answer and returns lines. That keeps the
//! whole render unit-testable with no database, no daemon and no terminal, which
//! is what makes the strip's exact shape a committed assertion rather than
//! something only visible by eye.
//!
//! Colour is a REQUIREMENT here, not decoration: the point of the strip is that a
//! stall is visible without reading it. Palette matches the TUI style guide, and
//! colour is dropped entirely when stdout is not a terminal so piped output stays
//! plain text.

use ainb_hangar_store::service::pipeline_health::{PipelineHealth, StageHealth};

/// Palette (TUI style guide): green = healthy, amber = at a limit but working,
/// red = the pipeline cannot move, muted = structure.
const GREEN: (u8, u8, u8) = (100, 200, 100);
const AMBER: (u8, u8, u8) = (255, 165, 0);
const RED: (u8, u8, u8) = (230, 100, 100);
const MUTED: (u8, u8, u8) = (120, 120, 140);
const SOFT_WHITE: (u8, u8, u8) = (220, 220, 230);
const RESET: &str = "\x1b[0m";

/// One coloured run of text inside a cell.
struct Seg {
    text: String,
    color: Option<(u8, u8, u8)>,
}

impl Seg {
    fn new(text: impl Into<String>, color: (u8, u8, u8)) -> Self {
        Self {
            text: text.into(),
            color: Some(color),
        }
    }
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: None,
        }
    }
}

/// A cell is a sequence of coloured runs; its width is the PLAIN width, so
/// padding is unaffected by whether colour is on.
type Cell = Vec<Seg>;

fn cell_width(cell: &Cell) -> usize {
    cell.iter().map(|s| s.text.chars().count()).sum()
}

fn render_cell(cell: &Cell, width: usize, color: bool) -> String {
    let mut out = String::new();
    for seg in cell {
        match (color, seg.color) {
            (true, Some((r, g, b))) => {
                out.push_str(&format!("\x1b[38;2;{r};{g};{b}m{}{RESET}", seg.text));
            }
            _ => out.push_str(&seg.text),
        }
    }
    for _ in cell_width(cell)..width {
        out.push(' ');
    }
    out
}

/// Render the whole `pipeline show` body: the light strip, a blank line, then the
/// three-row stage strip.
///
/// `daemon_alive` is supplied by the caller because the CLI and the TUI each
/// already have their own liveness notion (pid file / socket link) and a third
/// one invented here would be free to disagree with both.
#[must_use]
pub fn render(health: &PipelineHealth, daemon_alive: bool, color: bool) -> Vec<String> {
    let mut lines = vec![lights(health, daemon_alive, color), String::new()];
    lines.extend(stage_strip(health, color));
    lines
}

/// The four lights, left to right, on one line.
fn lights(health: &PipelineHealth, daemon_alive: bool, color: bool) -> String {
    let mut cells: Vec<Cell> = Vec::with_capacity(4);

    cells.push(if daemon_alive {
        vec![Seg::new("●", GREEN), Seg::plain(" daemon ok")]
    } else {
        vec![Seg::new("●", RED), Seg::plain(" daemon down")]
    });

    // The one that matters: a stage whose role nobody holds parks its cards
    // forever and never reports an error, so the light NAMES the missing role
    // rather than just going red.
    let uncovered = health.uncovered();
    cells.push(if uncovered.is_empty() {
        vec![Seg::new("●", GREEN), Seg::plain(" roles covered")]
    } else {
        let roles: Vec<&str> =
            uncovered.iter().filter_map(|s| s.services_role.as_deref()).collect();
        vec![
            Seg::new("●", RED),
            Seg::plain(format!(" no agent: {}", roles.join(", "))),
        ]
    });

    cells.push(match health.saturated() {
        Some(s) => vec![
            Seg::new("○", AMBER),
            Seg::plain(format!(
                " wip {}/{} {}",
                s.wip_active,
                s.wip_limit.unwrap_or_default(),
                s.name
            )),
        ],
        None => match tightest(health) {
            Some(s) => vec![
                Seg::new("●", GREEN),
                Seg::plain(format!(
                    " wip {}/{} {}",
                    s.wip_active,
                    s.wip_limit.unwrap_or_default(),
                    s.name
                )),
            ],
            None => vec![Seg::new("●", GREEN), Seg::plain(" wip unlimited")],
        },
    });

    let stuck = health.stuck();
    cells.push(vec![
        Seg::new("●", if stuck > 0 { RED } else { GREEN }),
        Seg::plain(format!(" {stuck} stuck")),
    ]);

    cells
        .iter()
        .map(|c| render_cell(c, cell_width(c), color))
        .collect::<Vec<_>>()
        .join("   ")
}

/// The capped stage with the least headroom — the one worth naming when nothing
/// is saturated yet.
///
/// Ties break on the BUSIER stage: `1/2` and `0/1` both have one slot left, but
/// the stage already doing work is the one an operator is watching.
fn tightest(health: &PipelineHealth) -> Option<&StageHealth> {
    health.stages.iter().filter(|s| s.wip_limit.is_some()).min_by_key(|s| {
        (
            s.wip_limit.unwrap_or_default() - s.wip_active,
            -s.wip_active,
        )
    })
}

/// The three-row stage strip: names, role dots, card counts.
fn stage_strip(health: &PipelineHealth, color: bool) -> Vec<String> {
    if health.stages.is_empty() {
        return vec!["no stages on this board".to_string()];
    }
    let mut names: Vec<Cell> = Vec::new();
    let mut roles: Vec<Cell> = Vec::new();
    let mut counts: Vec<Cell> = Vec::new();

    for stage in &health.stages {
        names.push(vec![Seg::new(stage.name.clone(), SOFT_WHITE)]);
        roles.push(role_cell(stage));
        counts.push(count_cell(stage));
    }

    let widths: Vec<usize> = (0..health.stages.len())
        .map(|i| cell_width(&names[i]).max(cell_width(&roles[i])).max(cell_width(&counts[i])))
        .collect();

    let sep = if color {
        format!("\x1b[38;2;{};{};{}m │ {RESET}", MUTED.0, MUTED.1, MUTED.2)
    } else {
        " │ ".to_string()
    };
    [names, roles, counts]
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(i, cell)| render_cell(cell, widths[i], color))
                .collect::<Vec<_>>()
                .join(&sep)
                .trim_end()
                .to_string()
        })
        .collect()
}

/// The per-stage role dot: `✗` when NO agent holds the role (red — cards can
/// never be pulled), `○` when the role is held but every holder is at its own
/// `max_concurrent_tasks` (amber — ordinary busyness), `●` when someone can pull
/// right now (green). A stage with no role gate renders blank.
fn role_cell(stage: &StageHealth) -> Cell {
    let Some(role) = stage.services_role.as_deref() else {
        return vec![Seg::plain("")];
    };
    let (glyph, color) = if stage.role_agents == 0 {
        ("✗", RED)
    } else if stage.role_agents_free == 0 {
        ("○", AMBER)
    } else {
        ("●", GREEN)
    };
    vec![
        Seg::new(role.to_string(), MUTED),
        Seg::plain(" "),
        Seg::new(glyph, color),
    ]
}

/// The per-stage count row: card count, its WIP cap when it has one, and a `⏳n`
/// suffix when cards are stuck here.
fn count_cell(stage: &StageHealth) -> Cell {
    let mut cell = vec![Seg::new(stage.cards.to_string(), SOFT_WHITE)];
    if let Some(limit) = stage.wip_limit {
        let color = if stage.wip_saturated() { AMBER } else { MUTED };
        cell.push(Seg::new(
            format!(" (wip {}/{limit})", stage.wip_active),
            color,
        ));
    }
    if stage.stuck > 0 {
        cell.push(Seg::new(format!(" ⏳{}", stage.stuck), RED));
    }
    cell
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(
        name: &str,
        role: Option<&str>,
        wip_limit: Option<i64>,
        wip_active: i64,
        role_agents: i64,
        role_agents_free: i64,
        cards: i64,
        stuck: i64,
    ) -> StageHealth {
        StageHealth {
            column_id: format!("col-{name}"),
            name: name.to_string(),
            ord: 0,
            services_role: role.map(ToString::to_string),
            wip_limit,
            wip_active,
            role_agents,
            role_agents_free,
            cards,
            stuck,
        }
    }

    /// The default six-stage pipeline with a missing tester renders the exact
    /// strip an operator reads: `roles covered` RED naming `tester`, a `✗` under
    /// QA, and every other stage green. Colour is off so the assertion is on the
    /// LAYOUT; `lights_are_coloured` covers the escapes.
    #[test]
    fn renders_the_default_pipeline_with_an_uncovered_role() {
        let health = PipelineHealth {
            stages: vec![
                stage("Backlog", None, None, 0, 0, 0, 2, 0),
                stage("Triage", Some("triager"), None, 0, 1, 1, 1, 0),
                stage("Implement", Some("implementer"), Some(2), 1, 2, 1, 1, 0),
                stage("Review", Some("reviewer"), Some(3), 0, 1, 1, 0, 0),
                stage("QA", Some("tester"), Some(1), 0, 0, 0, 1, 1),
                stage("Done", None, None, 0, 0, 0, 4, 0),
            ],
        };
        let out = render(&health, true, false);
        assert_eq!(
            out,
            vec![
                "● daemon ok   ● no agent: tester   ● wip 1/2 Implement   ● 1 stuck",
                "",
                "Backlog │ Triage    │ Implement     │ Review      │ QA             │ Done",
                "        │ triager ● │ implementer ● │ reviewer ●  │ tester ✗       │",
                "2       │ 1         │ 1 (wip 1/2)   │ 0 (wip 0/3) │ 1 (wip 0/1) ⏳1 │ 4",
            ],
            "rendered strip:\n{}",
            out.join("\n")
        );
    }

    /// Every light flips green when the roster covers every stage, and a stage
    /// whose holders are all busy reads amber `○`, NOT red — busy is not broken.
    #[test]
    fn healthy_pipeline_reports_all_green_and_busy_reads_amber() {
        let health = PipelineHealth {
            stages: vec![
                stage("Triage", Some("triager"), None, 0, 1, 0, 1, 0),
                stage("Review", Some("reviewer"), Some(3), 1, 2, 2, 1, 0),
            ],
        };
        let out = render(&health, true, false);
        assert_eq!(
            out[0],
            "● daemon ok   ● roles covered   ● wip 1/3 Review   ● 0 stuck"
        );
        assert!(
            out[3].contains("triager ○"),
            "all-busy holders read amber: {}",
            out[3]
        );
        assert!(
            out[3].contains("reviewer ●"),
            "a free holder reads green: {}",
            out[3]
        );
    }

    /// A dead daemon and a saturated stage each own their light.
    #[test]
    fn daemon_down_and_saturated_wip_each_light_up() {
        let health = PipelineHealth {
            stages: vec![stage(
                "Implement",
                Some("implementer"),
                Some(2),
                2,
                1,
                0,
                3,
                0,
            )],
        };
        let out = render(&health, false, false);
        assert_eq!(
            out[0],
            "● daemon down   ● roles covered   ○ wip 2/2 Implement   ● 0 stuck"
        );
    }

    /// With colour on, the lights carry 24-bit escapes and every cell is still
    /// padded to its PLAIN width, so a coloured strip lines up column-for-column
    /// with the plain one.
    ///
    /// TWO stages on purpose. With one stage the only padded region is the end of
    /// the line, which `stage_strip` then `trim_end`s away in BOTH the coloured
    /// and the plain path, so the assertion collapses to `"tester ✗" ==
    /// "tester ✗"` and passes however the padding is computed. A second stage puts
    /// an interior cell in front of a `│` separator, which makes the pad width
    /// load-bearing: get it wrong and the separator moves.
    #[test]
    fn lights_are_coloured_and_padding_ignores_escapes() {
        let health = PipelineHealth {
            stages: vec![
                stage("QA", Some("tester"), Some(1), 0, 0, 0, 1, 0),
                stage("Review", Some("reviewer"), None, 0, 1, 1, 0, 0),
            ],
        };
        let colored = render(&health, true, true);
        assert!(
            colored[0].contains("\x1b[38;2;230;100;100m●"),
            "{}",
            colored[0]
        );

        // The plain strip, pinned exactly: each column is as wide as its widest
        // of the three rows (11 for QA's `1 (wip 0/1)`, 10 for Review's role
        // cell), so the separator sits at the same offset on every row.
        let plain = render(&health, true, false);
        assert_eq!(
            &plain[2..],
            [
                "QA          │ Review",
                "tester ✗    │ reviewer ●",
                "1 (wip 0/1) │ 0",
            ],
            "rendered strip:\n{}",
            plain.join("\n")
        );

        // Colouring changes the bytes and nothing else. If the pad count were
        // taken off the RENDERED width the escapes would eat the padding and the
        // separator would slide left on every coloured row.
        for row in 2..plain.len() {
            assert_eq!(
                strip_ansi(&colored[row]),
                plain[row],
                "row {row} must be identical once the escapes are stripped"
            );
        }
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
