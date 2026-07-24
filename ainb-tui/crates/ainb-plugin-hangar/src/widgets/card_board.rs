//! Card-board widget (63l.1): a Linear-style status board — N columns side by
//! side, each a vertical stack of sleek bordered cards.
//!
//! This replaces the sparse [`SectionBands`](crate::screen::issue_list) density
//! pass on the issue list (vertical bands of bare rows) with a horizontal board:
//! one column per canonical [`IssueLifecycle`] status, each column a header
//! (status glyph + name + count, with room for the `⋯` context and `+` create
//! affordances the mouse layer wires in P0.2) above a scrollable stack of
//! bordered, rounded cards.
//!
//! ## Pure render + a layout model
//!
//! Like every widget here, the board is a **pure render helper**: it takes a
//! [`WireBuffer`], an area, and the data to paint, and emits cells. It holds no
//! state and does no IO. The selection and the per-column scroll offsets are
//! passed in by the caller's reducer state (the issue list owns them).
//!
//! Crucially, the render also *returns* a [`BoardLayout`] — the on-screen
//! geometry it just painted: every column's [`Rect`] and every visible card's
//! [`Rect`], tagged with the issue id it draws. P0.2 hit-tests a mouse click
//! against this layout to resolve "which card / which column header / which `+`
//! affordance was clicked" without re-deriving the geometry. The render is the
//! single source of truth for where things landed, so the hit-test can never
//! drift from the paint.
//!
//! ## Card anatomy
//!
//! ```text
//! ╭──────────────────╮      ╭──────────────────╮  ← rounded border (selected:
//! │ HGR-9            │      ┃ HGR-12           ┃    heavy clay border)
//! │ Refactor the API │      ┃ Wire the mouse   ┃
//! │ layer end to end │      ┃ hit-test into …  ┃  ← title, 2 lines, ellipsis
//! │ ◆ Urgent      ◔a │      ┃ ◆ High        ◔c ┃  ← priority chip + assignee
//! ╰──────────────────╯      ╰──────────────────╯
//! ```
//!
//! The id line is muted; the title wraps to two lines with an ellipsis on
//! overflow; the footer carries a colour-coded priority chip
//! ([`PriorityChip`]) and an assignee glyph. A selected card swaps the rounded
//! border for a heavy "clay" border so the eye lands on it immediately.
//!
//! ## Empty columns
//!
//! An empty column is never a void: it paints a centered dashed-border
//! placeholder ([`render_empty_placeholder`]) so the board reads as "this status
//! has no issues" rather than "the render broke".
//!
//! Char-safe throughout: every write iterates `char`s, never bytes
//! (`reference_rust_utf8_truncate_trap`), and every paint is clipped to the
//! column's right edge so a card never bleeds into its neighbour, even at the
//! 80×24 floor (`project_ainb_tui_width_aware_panels`).

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// A laid-out card: a column item drawn at `rect`, tagged with the issue id it
/// renders. P0.2 hit-tests a click against `rect` to resolve the clicked card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardLayout {
    /// The issue id this card renders (the value P0.2 opens on a click).
    pub issue_id: String,
    /// The card's on-screen rectangle (border-inclusive).
    pub rect: Rect,
}

/// A laid-out column: its on-screen rectangle, its header row, and the visible
/// cards painted inside it. The geometry the P0.2 mouse layer hit-tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnLayout {
    /// The canonical column index (`0..COLUMN_COUNT`, left-to-right).
    pub index: usize,
    /// The column's full on-screen rectangle (header + body).
    pub rect: Rect,
    /// The header row (the status glyph + name + count + affordances live here).
    pub header_y: u16,
    /// The cell (x) of the rendered `+` create affordance in the header, when it
    /// fit — P0.2 hit-tests a click on it to start the create flow. `None` when
    /// the column was too narrow to paint it.
    pub create_affordance_x: Option<u16>,
    /// The visible cards in this column, top-to-bottom (those past the scroll
    /// offset that fit in the body). A click outside every card rect but inside
    /// the column body is a column-background click (P0.2 focuses the column).
    pub cards: Vec<CardLayout>,
}

/// The full board geometry the render produced — one [`ColumnLayout`] per
/// status column. The single source of truth P0.2 hit-tests; it can never drift
/// from the paint because the same pass produces both.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoardLayout {
    /// The laid-out columns, left-to-right.
    pub columns: Vec<ColumnLayout>,
}

impl BoardLayout {
    /// The card whose rect contains `(x, y)`, if any — the P0.2 click resolver.
    /// Scans columns then cards; returns the first hit (rects never overlap).
    #[must_use]
    pub fn card_at(&self, x: u16, y: u16) -> Option<&CardLayout> {
        self.columns
            .iter()
            .flat_map(|c| c.cards.iter())
            .find(|card| card.rect.contains(x, y))
    }

    /// The column whose rect contains `(x, y)`, if any (P0.2 column focus /
    /// header hit-test). A click anywhere in the column — header or body —
    /// resolves here.
    #[must_use]
    pub fn column_at(&self, x: u16, y: u16) -> Option<&ColumnLayout> {
        self.columns.iter().find(|c| c.rect.contains(x, y))
    }
}

/// An inclusive-origin, exclusive-extent rectangle in cell coordinates.
///
/// Covers columns `x..x+w` and rows `y..y+h`. The geometry primitive the board
/// layout is expressed in (the SDK has no `Rect`, and pulling ratatui's into the
/// plugin would violate the no-host-deps rule —
/// `reference_plugin_sdk_no_host_deps`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge (inclusive).
    pub x: u16,
    /// Top edge (inclusive).
    pub y: u16,
    /// Width in cells.
    pub w: u16,
    /// Height in cells.
    pub h: u16,
}

impl Rect {
    /// Construct a rectangle at `(x, y)` of `w × h` cells.
    #[must_use]
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    /// One past the right edge (exclusive). Saturates so a wide rect near
    /// `u16::MAX` never wraps.
    #[must_use]
    pub const fn right(self) -> u16 {
        self.x.saturating_add(self.w)
    }

    /// One past the bottom edge (exclusive). Saturates near `u16::MAX`.
    #[must_use]
    pub const fn bottom(self) -> u16 {
        self.y.saturating_add(self.h)
    }

    /// Whether `(px, py)` falls inside the rectangle (`x..right × y..bottom`).
    /// The hit-test predicate P0.2 resolves a mouse click with.
    #[must_use]
    pub const fn contains(self, px: u16, py: u16) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
}

/// The data the board needs per item — a flattened, render-only view of an issue.
///
/// The caller maps its `IssueRow`s into these. Holds only what a card paints, so
/// the widget never re-parses the wire row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardCard {
    /// The issue id (the value the layout tags each card with; P0.2 opens it).
    pub issue_id: String,
    /// The human-facing display id painted on the id line (e.g. `HGR-9`); the
    /// raw id is used when the daemon supplied no display id.
    pub display_id: String,
    /// The issue title, wrapped to two lines with an ellipsis on overflow.
    pub title: String,
    /// The priority chip rendered in the footer.
    pub priority: PriorityChip,
    /// The assignee's initial glyph in the footer (the first char of the
    /// `type:id` assignee), or `None` when unassigned.
    pub assignee_initial: Option<char>,
    /// Whether the issue links an upstream GitHub/Jira issue (`issue.external_ref`,
    /// 0043): drives a subtle `⧉` glyph flush-right on the id line for traceability.
    pub linked: bool,
    /// This issue's sub-issue roll-up `(done, total)` (migration 0046), or `None`
    /// when it has no children. Drives a `⊟ done/total` footer badge — gold once
    /// complete — so a parent card visibly flips to `1/1` when its last child
    /// finishes (the board-observable side of the child-done cascade).
    pub subtasks: Option<(u32, u32)>,
}

/// One status column's input to the board.
///
/// Carries the header glyph + name, the scroll offset, and the cards it holds.
/// The caller builds these from its reducer state (the issue list groups its
/// visible rows by column).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardColumn {
    /// The status glyph painted before the name (e.g. `◔` in-progress).
    pub glyph: char,
    /// The status name (e.g. `In Progress`).
    pub name: String,
    /// The cards in this column, top-to-bottom in display order.
    pub cards: Vec<BoardCard>,
    /// First visible card index — the per-column vertical scroll offset. Cards
    /// before this are scrolled off the top; the render paints from here down.
    pub scroll_offset: usize,
}

impl BoardColumn {
    /// The header count suffix — the *total* card count (not just the visible
    /// slice), so `In Progress (12)` reflects the whole column even when only a
    /// few cards fit.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.cards.len()
    }
}

/// The five priority chips, colour-coded by urgency (Urgent red … None grey).
///
/// The caller maps the wire `priority` scalar (`0..3`, higher = more urgent)
/// into one of these via [`PriorityChip::from_priority`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityChip {
    /// P0 — the most urgent (red).
    Urgent,
    /// P1 — high (orange).
    High,
    /// P2 — medium (amber).
    Medium,
    /// P3 — low (grey-blue).
    Low,
    /// No explicit priority (muted grey).
    None,
}

impl PriorityChip {
    /// Map the wire `priority` scalar to a chip. The scale is `0..3` with HIGHER
    /// = MORE URGENT (the same scale as `IssueRow::priority`): `3` → Urgent (P0),
    /// `2` → High (P1), `1` → Medium (P2), `0` → None (P3/routine — the default).
    /// Anything above the range clamps to Urgent (fail-loud, never silent).
    #[must_use]
    pub const fn from_priority(priority: i64) -> Self {
        match priority {
            0 => Self::None,
            1 => Self::Medium,
            2 => Self::High,
            _ => Self::Urgent,
        }
    }

    /// The chip label painted in the card footer.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Urgent => "Urgent",
            Self::High => "High",
            Self::Medium => "Medium",
            Self::Low => "Low",
            Self::None => "None",
        }
    }

    /// The chip's accent colour (Urgent red / High orange / Medium amber / Low
    /// grey / None muted grey).
    #[must_use]
    pub const fn color(self) -> Color {
        match self {
            Self::Urgent => Color::rgb(235, 90, 90),
            Self::High => Color::rgb(235, 150, 70),
            Self::Medium => Color::rgb(225, 200, 90),
            Self::Low => Color::rgb(150, 160, 190),
            Self::None => MUTED_GRAY,
        }
    }

    /// The diamond chip glyph (a filled diamond for any explicit priority, a
    /// hollow one for None) painted before the label.
    #[must_use]
    pub const fn glyph(self) -> char {
        match self {
            Self::None => '◇',
            _ => '◆',
        }
    }
}

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Column header accent fallback (when a column name has no mapped accent).
const GOLD: Color = Color::rgb(255, 215, 0);
/// Muted text for ids and counts.
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Primary card title text.
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// The dim slate-blue id accent (matches the issue-list `ID_ACCENT`).
const ID_ACCENT: Color = Color::rgb(150, 160, 190);
/// The heavy "clay" border colour of a selected card — a warm terracotta so the
/// selection reads as a raised, tactile tile against the muted unselected cards.
const CLAY: Color = Color::rgb(210, 130, 90);
/// Empty-placeholder dashed-border colour: dimmer than the muted text so an
/// empty column recedes without vanishing.
const PLACEHOLDER_GRAY: Color = Color::rgb(80, 80, 95);
/// Subtle column plumbing — the header underline and the inter-column
/// separator. Low-contrast against the dark bg so it reads as structure, not
/// decoration ("transparent" borders).
const COLUMN_BORDER: Color = Color::rgb(52, 58, 80);
/// Resting card border — a dim blue-grey so unselected cards read as tiles.
const CARD_BORDER: Color = Color::rgb(70, 80, 110);
/// Card fill behind content: the palette panel background, one step above the
/// app background so cards sit on a raised surface.
const CARD_BG: Color = Color::rgb(30, 30, 40);
/// Selected-card fill (the palette list-highlight background).
const CARD_BG_SELECTED: Color = Color::rgb(40, 40, 60);

/// Per-status column accent, keyed on the column's display name so every
/// board (issues, kanban, skills, autopilots) inherits it without an API
/// change. Unknown names fall back to the gold header accent.
const fn column_accent(name: &str) -> Color {
    // `const fn` can't match on &str; compare bytes.
    match name.as_bytes() {
        b"Backlog" => Color::rgb(150, 160, 190),
        b"Todo" | b"Queued" => Color::rgb(100, 149, 237),
        b"In Progress" | b"Running" => Color::rgb(235, 185, 80),
        b"In Review" => Color::rgb(185, 140, 235),
        b"Done" => Color::rgb(110, 200, 130),
        b"Failed" => Color::rgb(220, 90, 90),
        _ => GOLD,
    }
}

// ---------------------------------------------------------------------------
// Geometry constants
// ---------------------------------------------------------------------------

/// The minimum width a column needs to paint a usable card.
///
/// Below `MIN_COL_W × COLUMN_COUNT` the board horizontally clips — it paints as
/// many whole columns as fit and drops the overflow rather than squeezing every
/// column into an unreadable sliver.
pub const MIN_COL_W: u16 = 14;

/// Rows a card occupies: top border, id line, two title lines, footer, bottom
/// border = 6 rows.
const CARD_ROWS: u16 = 6;

/// The header occupies the column's first row; the card body starts one row
/// below it (a blank spacer row between the header and the first card keeps the
/// board from feeling cramped).
const HEADER_ROWS: u16 = 2;

/// Cells the right-aligned header affordances (`⋯` + `+`) reserve at the column's
/// right edge. The header title is clipped to stop before the `⋯` (painted at
/// `right-4` by [`render_header_affordances`]) so a long FSM-mapped title never
/// garbles into the glyphs — while a title that already fits ahead of the zone is
/// left untouched (only the overrun is trimmed).
const HEADER_AFFORDANCE_W: u16 = 4;

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

/// Render the card board into `buf` over the area `(0, top)`..`(area_w, bottom)`
/// and return the [`BoardLayout`] it painted (the P0.2 hit-test geometry).
///
/// `columns` are the status columns left-to-right; `selected` is the
/// `(column_index, card_index)` of the selected card (the card index is into the
/// column's full card list, not the visible slice), or `None` when nothing is
/// selected. The selected card gets the heavy clay border.
///
/// The body width `area_w` is split into [`COLUMN_COUNT`]-many equal columns of
/// at least [`MIN_COL_W`]. When `area_w` cannot fit every column at the minimum
/// width, the board paints as many whole columns as fit (left-to-right) and
/// clips the rest — a column is never squeezed below `MIN_COL_W` into an
/// unreadable sliver. The returned layout contains only the painted columns.
#[must_use]
pub fn render_card_board(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    columns: &[BoardColumn],
    selected: Option<(usize, usize)>,
) -> BoardLayout {
    let n = columns.len().max(1);
    // Each column gets an equal share, floored at the readable minimum. When the
    // area can't fit every column at the minimum, `visible_cols` caps how many we
    // actually paint (horizontal clip) — the rest are dropped, not squeezed.
    let col_w = (area_w / u16::try_from(n).unwrap_or(u16::MAX)).max(MIN_COL_W);
    let visible_cols = (area_w / col_w).max(1) as usize;

    let mut layout = BoardLayout::default();
    for (i, column) in columns.iter().enumerate().take(visible_cols) {
        let x0 = u16::try_from(i).unwrap_or(0).saturating_mul(col_w);
        // The last *painted* column absorbs any width remainder so the board
        // fills the area edge-to-edge without a ragged right gutter.
        let this_w = if i + 1 == visible_cols.min(columns.len()) {
            area_w.saturating_sub(x0)
        } else {
            col_w
        };
        let sel_card = selected.and_then(|(sc, ci)| (sc == i).then_some(ci));
        let col_rect = Rect::new(x0, top, this_w, bottom.saturating_sub(top));
        let is_last = i + 1 == visible_cols.min(columns.len());
        let col_layout = render_column(buf, col_rect, i, column, sel_card, is_last);
        layout.columns.push(col_layout);
    }
    layout
}

/// Render one column inside `area`: the header, then its scrolled, clipped card
/// stack (or the dashed empty-state placeholder when it holds no cards). Returns
/// the column's [`ColumnLayout`].
fn render_column(
    buf: &mut WireBuffer,
    area: Rect,
    index: usize,
    column: &BoardColumn,
    selected_card: Option<usize>,
    is_last: bool,
) -> ColumnLayout {
    let x0 = area.x;
    let col_w = area.w;
    let right = area.right();
    let bottom = area.bottom();
    let header_y = area.y;

    // Every column except the last cedes its rightmost cell to a gutter that
    // carries the subtle inter-column separator, so lanes read as distinct
    // panels instead of card borders butting flush against each other.
    let lane_w = if is_last {
        col_w
    } else {
        col_w.saturating_sub(1)
    };

    // Header: `◔ In Progress (12)` — glyph + name in the column's status
    // accent, bold; the count recedes in muted grey. The context `⋯` and
    // create `+` affordances stay right-aligned (hit-test wired in P0.2).
    let accent = column_accent(&column.name);
    let create_affordance_x = render_header_affordances(buf, x0, header_y, col_w);
    // Clip the title so it never runs into the right-aligned `⋯ +` affordances:
    // when they paint (`⋯` at right-4, `+` at right-2) the title stops before the
    // `⋯`, so an FSM-mapped header (`Queued ↦queued`) stays legible in a narrow
    // column instead of garbling into the glyphs.
    let header_right = match create_affordance_x {
        Some(_) => right.saturating_sub(HEADER_AFFORDANCE_W).max(x0),
        None => right,
    };
    let name_part = format!("{} {}", column.glyph, column.name);
    let hx = put_str_bold(buf, x0, header_y, &name_part, accent, header_right);
    put_str(
        buf,
        hx,
        header_y,
        &format!(" ({})", column.count()),
        MUTED_GRAY,
        header_right,
    );

    // Subtle underline on the spacer row beneath the header, so each column
    // reads as a framed lane rather than a floating label.
    let underline_y = header_y.saturating_add(1);
    for x in x0..x0.saturating_add(lane_w) {
        put_char(buf, x, underline_y, '─', COLUMN_BORDER, right);
    }
    // Inter-column separator in the ceded gutter cell, header to floor.
    if !is_last && col_w >= 2 {
        let sep_x = right.saturating_sub(1);
        for y in header_y..bottom {
            put_char(buf, sep_x, y, '│', COLUMN_BORDER, right);
        }
    }

    let body_top = header_y.saturating_add(HEADER_ROWS);
    let mut cards = Vec::new();

    if column.cards.is_empty() {
        // An empty column is a centered dashed placeholder, not a void.
        render_empty_placeholder(buf, x0, lane_w, body_top, bottom);
    } else {
        let mut y = body_top;
        for (idx, card) in column.cards.iter().enumerate().skip(column.scroll_offset) {
            // Stop once the next whole card would overflow the body — a partial
            // card is never painted (it would read as a render glitch).
            if y.saturating_add(CARD_ROWS) > bottom {
                break;
            }
            let is_selected = selected_card == Some(idx);
            let rect = Rect::new(x0, y, lane_w, CARD_ROWS);
            render_card(buf, rect, card, is_selected);
            cards.push(CardLayout {
                issue_id: card.issue_id.clone(),
                rect,
            });
            // One blank spacer row between cards.
            y = y.saturating_add(CARD_ROWS).saturating_add(1);
        }
    }

    ColumnLayout {
        index,
        rect: area,
        header_y,
        create_affordance_x,
        cards,
    }
}

/// Paint the right-aligned `⋯` (context) and `+` (create) header affordances
/// when the column is wide enough to hold them after the title. Returns the `x`
/// of the `+` glyph when it was painted (the P0.2 create hit-test target), else
/// `None`. The glyphs are muted so they read as secondary controls.
fn render_header_affordances(buf: &mut WireBuffer, x0: u16, y: u16, col_w: u16) -> Option<u16> {
    // `+` sits at the right edge, `⋯` one cell to its left, with a one-cell
    // inset from the border. Only painted when the column is wide enough that
    // they don't collide with a (short) title — a sliver column skips them.
    if col_w < MIN_COL_W {
        return None;
    }
    let right = x0.saturating_add(col_w);
    let plus_x = right.saturating_sub(2);
    let ctx_x = right.saturating_sub(4);
    put_char(buf, ctx_x, y, '⋯', MUTED_GRAY, right);
    put_char(buf, plus_x, y, '+', MUTED_GRAY, right);
    Some(plus_x)
}

/// Render one bordered, rounded card inside `rect`. A selected card uses a heavy
/// clay border; an unselected card a rounded muted border.
///
/// Layout inside the border (`rect.w - 2` content cells per row):
/// - row 0: top border
/// - row 1: the muted id line (`HGR-9`)
/// - rows 2..=3: the title, wrapped to two lines with an ellipsis
/// - row 4: the footer — priority chip (left) + assignee initial (right)
/// - row 5: bottom border
fn render_card(buf: &mut WireBuffer, rect: Rect, card: &BoardCard, selected: bool) {
    let border_color = if selected { CLAY } else { CARD_BORDER };
    let glyphs = if selected {
        BorderGlyphs::HEAVY
    } else {
        BorderGlyphs::ROUNDED
    };
    // Panel fill first (bg-only spaces), then every border/text cell carries
    // the same bg — strictly rect-local, no whole-buffer scan. The host paints
    // cells in push order, so the later glyph cells win over the fill.
    let fill = if selected { CARD_BG_SELECTED } else { CARD_BG };
    fill_rect(buf, rect, fill);
    draw_border(buf, rect, glyphs, border_color, fill);

    // Content runs inside the border: x in `[rect.x+1, rect.right()-1)`.
    let inner_x = rect.x.saturating_add(1);
    let inner_right = rect.right().saturating_sub(1);
    let inner_w = inner_right.saturating_sub(inner_x);

    // Id line (muted slate-blue).
    let id_y = rect.y.saturating_add(1);
    put_str_bg(
        buf,
        inner_x,
        id_y,
        &clip(&card.display_id, inner_w),
        ID_ACCENT,
        fill,
        inner_right,
    );
    // A subtle `⧉` flush-right on the id line marks a card that links an upstream
    // issue (0043) — traceability at a glance without stealing a whole row.
    if card.linked {
        let glyph_x = inner_right.saturating_sub(1);
        if glyph_x >= inner_x {
            put_char_bg(buf, glyph_x, id_y, '⧉', ID_ACCENT, fill, inner_right);
        }
    }

    // Title, wrapped to exactly two lines with an ellipsis on overflow. The
    // selection reads off the heavy clay border, so the title keeps the same
    // soft-white in both states (the border, not the text colour, carries it).
    let [line1, line2] = wrap_two_lines(&card.title, inner_w);
    put_str_bg(
        buf,
        inner_x,
        rect.y.saturating_add(2),
        &line1,
        SOFT_WHITE,
        fill,
        inner_right,
    );
    put_str_bg(
        buf,
        inner_x,
        rect.y.saturating_add(3),
        &line2,
        SOFT_WHITE,
        fill,
        inner_right,
    );

    // Footer: priority chip on the left, assignee initial flushed right.
    let footer_y = rect.y.saturating_add(4);
    let chip = format!("{} {}", card.priority.glyph(), card.priority.label());
    put_str_bg(
        buf,
        inner_x,
        footer_y,
        &clip(&chip, inner_w),
        card.priority.color(),
        fill,
        inner_right,
    );
    if let Some(initial) = card.assignee_initial {
        // `◔<initial>` flushed to the right edge of the content area.
        let glyph_x = inner_right.saturating_sub(2);
        let init_x = inner_right.saturating_sub(1);
        if glyph_x >= inner_x {
            put_char_bg(buf, glyph_x, footer_y, '◔', ID_ACCENT, fill, inner_right);
            put_char_bg(
                buf,
                init_x,
                footer_y,
                initial,
                SOFT_WHITE,
                fill,
                inner_right,
            );
        }
    }

    // Sub-issue roll-up badge (0046): `⊟ done/total`, painted just right of the
    // priority chip. Gold once every child is complete (the parent card flips to
    // `1/1` after its last sub-issue finishes), muted while work remains. Only
    // drawn when the card actually has children.
    if let Some((done, total)) = card.subtasks {
        if total > 0 {
            let badge = format!("⊟ {done}/{total}");
            let badge_x =
                inner_x.saturating_add(u16::try_from(chip.chars().count() + 1).unwrap_or(0));
            let color = if done >= total { GOLD } else { MUTED_GRAY };
            // Keep clear of the flush-right assignee glyph (2 cells wide).
            let badge_right = inner_right.saturating_sub(3);
            // Render the badge ONLY when the WHOLE thing fits before `badge_right`
            // (CodeRabbit): a clipped `⊟ 1/` fragment misreads the roll-up, so a
            // narrow card omits the badge entirely rather than truncating it.
            let badge_w = u16::try_from(badge.chars().count()).unwrap_or(u16::MAX);
            if badge_x.saturating_add(badge_w) <= badge_right {
                put_str_bg(buf, badge_x, footer_y, &badge, color, fill, badge_right);
            }
        }
    }
}

/// Fill `rect` with background-only spaces — the raised card surface the
/// border and text then paint over.
fn fill_rect(buf: &mut WireBuffer, rect: Rect, bg: Color) {
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            let mut cell = Cell::new(" ");
            cell.bg = Some(bg);
            buf.push(Coord::new(x, y), cell);
        }
    }
}

/// The four corner + two edge glyphs for a card border. Rounded for an
/// unselected card, heavy for the selected one.
#[derive(Clone, Copy)]
struct BorderGlyphs {
    top_left: char,
    top_right: char,
    bottom_left: char,
    bottom_right: char,
    horizontal: char,
    vertical: char,
}

impl BorderGlyphs {
    /// Rounded-corner light border (the resting card look).
    const ROUNDED: Self = Self {
        top_left: '╭',
        top_right: '╮',
        bottom_left: '╰',
        bottom_right: '╯',
        horizontal: '─',
        vertical: '│',
    };
    /// Heavy border (the selected card's raised "clay" look).
    const HEAVY: Self = Self {
        top_left: '┏',
        top_right: '┓',
        bottom_left: '┗',
        bottom_right: '┛',
        horizontal: '━',
        vertical: '┃',
    };
}

/// Draw a rectangular border around `rect` using `glyphs` in `color`. Clips every
/// write to the buffer bounds; a degenerate rect (`w < 2` or `h < 2`) paints
/// nothing rather than overlapping its own corners.
fn draw_border(buf: &mut WireBuffer, rect: Rect, glyphs: BorderGlyphs, color: Color, bg: Color) {
    if rect.w < 2 || rect.h < 2 {
        return;
    }
    let left = rect.x;
    let right = rect.right().saturating_sub(1);
    let top = rect.y;
    let bottom = rect.bottom().saturating_sub(1);
    let bound = rect.right();

    put_char_bg(buf, left, top, glyphs.top_left, color, bg, bound);
    put_char_bg(buf, right, top, glyphs.top_right, color, bg, bound);
    put_char_bg(buf, left, bottom, glyphs.bottom_left, color, bg, bound);
    put_char_bg(buf, right, bottom, glyphs.bottom_right, color, bg, bound);
    // Top + bottom edges.
    for x in (left.saturating_add(1))..right {
        put_char_bg(buf, x, top, glyphs.horizontal, color, bg, bound);
        put_char_bg(buf, x, bottom, glyphs.horizontal, color, bg, bound);
    }
    // Left + right edges.
    for y in (top.saturating_add(1))..bottom {
        put_char_bg(buf, left, y, glyphs.vertical, color, bg, bound);
        put_char_bg(buf, right, y, glyphs.vertical, color, bg, bound);
    }
}

/// Paint a centered dashed-border placeholder filling the column body.
///
/// Spans `[body_top, bottom)` so an empty column reads as "no issues" rather than
/// a void. The placeholder is a short dashed box centered horizontally with a
/// muted `— empty —` caption, in the dim placeholder grey.
pub fn render_empty_placeholder(
    buf: &mut WireBuffer,
    x0: u16,
    col_w: u16,
    body_top: u16,
    bottom: u16,
) {
    if bottom <= body_top || col_w < 4 {
        return;
    }
    // A 3-row dashed box, centered in the column's body band.
    let box_h: u16 = 3;
    let body_h = bottom.saturating_sub(body_top);
    let box_top = body_top.saturating_add(body_h.saturating_sub(box_h) / 2);
    let inset = 1u16;
    let left = x0.saturating_add(inset);
    let width = col_w.saturating_sub(inset.saturating_mul(2)).max(2);
    let right = left.saturating_add(width).saturating_sub(1);
    let bound = x0.saturating_add(col_w);

    // Dashed top + bottom edges (`╌`), dashed sides (`╎`), rounded corners.
    put_char(buf, left, box_top, '╭', PLACEHOLDER_GRAY, bound);
    put_char(buf, right, box_top, '╮', PLACEHOLDER_GRAY, bound);
    let box_bottom = box_top.saturating_add(box_h).saturating_sub(1);
    put_char(buf, left, box_bottom, '╰', PLACEHOLDER_GRAY, bound);
    put_char(buf, right, box_bottom, '╯', PLACEHOLDER_GRAY, bound);
    for x in (left.saturating_add(1))..right {
        put_char(buf, x, box_top, '╌', PLACEHOLDER_GRAY, bound);
        put_char(buf, x, box_bottom, '╌', PLACEHOLDER_GRAY, bound);
    }
    put_char(
        buf,
        left,
        box_top.saturating_add(1),
        '╎',
        PLACEHOLDER_GRAY,
        bound,
    );
    put_char(
        buf,
        right,
        box_top.saturating_add(1),
        '╎',
        PLACEHOLDER_GRAY,
        bound,
    );

    // Centered `— empty —` caption on the middle row.
    let caption = "— empty —";
    let cap_w = u16::try_from(caption.chars().count()).unwrap_or(0);
    if cap_w < width {
        let cap_x = left
            .saturating_add(1)
            .saturating_add((width.saturating_sub(2).saturating_sub(cap_w)) / 2);
        put_str(
            buf,
            cap_x,
            box_top.saturating_add(1),
            caption,
            PLACEHOLDER_GRAY,
            right,
        );
    }
}

// ---------------------------------------------------------------------------
// Text helpers (char-safe — never byte-slice)
// ---------------------------------------------------------------------------

/// Wrap `s` to exactly two lines of at most `w` chars each. The first line takes
/// the leading `w` chars (word boundaries are not honoured — the cards are
/// narrow, char wrapping reads cleaner than ragged word breaks); the second line
/// takes the next `w` chars, ellipsised if the title still overflows. A short
/// title leaves the second line blank. Char-safe (multi-byte aware).
fn wrap_two_lines(s: &str, w: u16) -> [String; 2] {
    let w = w as usize;
    if w == 0 {
        return [String::new(), String::new()];
    }
    let chars: Vec<char> = s.chars().collect();
    let line1: String = chars.iter().take(w).collect();
    if chars.len() <= w {
        return [line1, String::new()];
    }
    let rest = &chars[w..];
    if rest.len() <= w {
        let line2: String = rest.iter().collect();
        return [line1, line2];
    }
    // Still overflowing on the second line: take `w-1` chars + an ellipsis.
    let mut line2: String = rest.iter().take(w.saturating_sub(1)).collect();
    line2.push('…');
    [line1, line2]
}

/// Clip `s` to at most `w` display columns (char-based, multi-byte safe).
fn clip(s: &str, w: u16) -> String {
    s.chars().take(w as usize).collect()
}

/// Write a single `ch` at `(x, row)` in `color` if `x < right`, returning the
/// next column.
fn put_char(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color, right: u16) -> u16 {
    if x >= right {
        return x;
    }
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
    x.saturating_add(1)
}

/// Write `s` at `(x, row)` in `color`, clipping by **chars** at column `right`
/// (exclusive). Returns the next free column. Multi-byte safe.
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        cx = put_char(buf, cx, row, ch, color, right);
    }
    cx
}

/// [`put_char`] carrying the card's panel background.
fn put_char_bg(
    buf: &mut WireBuffer,
    x: u16,
    row: u16,
    ch: char,
    color: Color,
    bg: Color,
    right: u16,
) -> u16 {
    if x >= right {
        return x;
    }
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    cell.bg = Some(bg);
    buf.push(Coord::new(x, row), cell);
    x.saturating_add(1)
}

/// [`put_str`] carrying the card's panel background.
fn put_str_bg(
    buf: &mut WireBuffer,
    x: u16,
    row: u16,
    s: &str,
    color: Color,
    bg: Color,
    right: u16,
) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        cx = put_char_bg(buf, cx, row, ch, color, bg, right);
    }
    cx
}

/// [`put_str`] with the BOLD style bit set (wire modifier bit 1 — the
/// runtime's ratatui interop maps it to `Modifier::BOLD`).
fn put_str_bold(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        cell.modifier = 1;
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
    cx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A board card with the given id, title, priority and assignee.
    fn card(id: &str, title: &str, priority: PriorityChip, assignee: Option<char>) -> BoardCard {
        BoardCard {
            issue_id: id.to_string(),
            display_id: id.to_string(),
            title: title.to_string(),
            priority,
            assignee_initial: assignee,
            linked: false,
            subtasks: None,
        }
    }

    /// A column with a glyph, name, and cards (no scroll).
    fn column(glyph: char, name: &str, cards: Vec<BoardCard>) -> BoardColumn {
        BoardColumn {
            glyph,
            name: name.to_string(),
            cards,
            scroll_offset: 0,
        }
    }

    /// The five canonical columns, one representative card per non-empty one.
    fn five_columns() -> Vec<BoardColumn> {
        vec![
            column(
                '☰',
                "Backlog",
                vec![card("HGR-1", "Triage inbox", PriorityChip::Low, Some('a'))],
            ),
            column(
                '○',
                "Todo",
                vec![card(
                    "HGR-2",
                    "Write the parser",
                    PriorityChip::Medium,
                    Some('b'),
                )],
            ),
            column(
                '◔',
                "In Progress",
                vec![card(
                    "HGR-3",
                    "Refactor the API layer end to end",
                    PriorityChip::Urgent,
                    Some('c'),
                )],
            ),
            column(
                '◑',
                "In Review",
                vec![card(
                    "HGR-4",
                    "Wire the mouse hit-test",
                    PriorityChip::High,
                    Some('d'),
                )],
            ),
            column('●', "Done", vec![]), // empty column
        ]
    }

    /// Reconstruct a row's text from the buffer; unwritten cells are spaces.
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

    /// The full painted text (row-major) for substring assertions.
    fn painted_text(buf: &WireBuffer) -> String {
        let mut out = String::new();
        for y in 0..buf.height {
            out.push_str(&row_text(buf, y, buf.width));
            out.push('\n');
        }
        out
    }

    /// All five column headers render with their glyph, name, and live count at a
    /// realistic 120×24 board.
    #[test]
    fn renders_five_column_headers_with_counts() {
        let mut buf = WireBuffer::new(120, 24);
        let _ = render_card_board(&mut buf, 120, 0, 23, &five_columns(), None);
        let painted = painted_text(&buf);
        for header in [
            "Backlog (1)",
            "Todo (1)",
            "In Progress (1)",
            "In Review (1)",
            "Done (0)",
        ] {
            assert!(
                painted.contains(header),
                "missing header {header:?} in:\n{painted}"
            );
        }
        // The status glyphs lead their headers.
        assert!(painted.contains('◔'), "in-progress glyph: {painted}");
    }

    /// A card paints its id line, its title (wrapped), and a coloured priority
    /// chip with the right label.
    #[test]
    fn card_shows_id_title_and_priority_chip() {
        let mut buf = WireBuffer::new(120, 24);
        let _ = render_card_board(&mut buf, 120, 0, 23, &five_columns(), None);
        let painted = painted_text(&buf);
        assert!(painted.contains("HGR-3"), "card id: {painted}");
        // The long title wraps — its leading chars appear.
        assert!(painted.contains("Refactor"), "card title: {painted}");
        // The Urgent chip label + glyph render.
        assert!(painted.contains("Urgent"), "priority chip label: {painted}");
        assert!(painted.contains('◆'), "priority chip glyph: {painted}");

        // The Urgent chip is painted in the urgent red, not the default text.
        let urgent_red = PriorityChip::Urgent.color();
        let has_red = buf.cells.iter().any(|(_, c)| c.fg == Some(urgent_red));
        assert!(
            has_red,
            "the urgent chip must be painted in the urgent colour"
        );
    }

    /// A card that links an upstream issue (0043) paints a subtle `⧉` glyph on its
    /// id line; a card with `linked: false` shows none.
    #[test]
    fn linked_card_shows_the_link_glyph() {
        let mut linked = card("HGR-9", "Ship it", PriorityChip::Low, Some('a'));
        linked.linked = true;
        let cols = vec![column('☰', "Backlog", vec![linked])];
        let mut buf = WireBuffer::new(120, 24);
        let _ = render_card_board(&mut buf, 120, 0, 23, &cols, None);
        assert!(
            painted_text(&buf).contains('⧉'),
            "linked card shows the ⧉ glyph"
        );

        // An unlinked card (the default) paints no glyph.
        let mut buf = WireBuffer::new(120, 24);
        let _ = render_card_board(&mut buf, 120, 0, 23, &five_columns(), None);
        assert!(
            !painted_text(&buf).contains('⧉'),
            "unlinked cards show no link glyph"
        );
    }

    /// A selected card swaps the rounded border for the heavy clay border — the
    /// heavy corner glyphs appear and are painted in the clay colour.
    #[test]
    fn selected_card_uses_heavy_clay_border() {
        let mut buf = WireBuffer::new(120, 24);
        // Select the single card in column 2 (In Progress).
        let _ = render_card_board(&mut buf, 120, 0, 23, &five_columns(), Some((2, 0)));
        let painted = painted_text(&buf);
        // Heavy top-left corner present (the rounded `╭` is the unselected look).
        assert!(painted.contains('┏'), "heavy border corner: {painted}");
        // And painted in clay.
        let clay = CLAY;
        let heavy_in_clay = buf.cells.iter().any(|(_, c)| c.symbol == "┏" && c.fg == Some(clay));
        assert!(heavy_in_clay, "the heavy border must be painted in clay");
        // The UNSELECTED cards keep the rounded corner.
        assert!(
            painted.contains('╭'),
            "unselected rounded corner: {painted}"
        );
    }

    /// An empty column paints the centered dashed-border placeholder, not a void.
    #[test]
    fn empty_column_shows_dashed_placeholder() {
        let mut buf = WireBuffer::new(120, 24);
        let _ = render_card_board(&mut buf, 120, 0, 23, &five_columns(), None);
        let painted = painted_text(&buf);
        // The dashed edge glyph + caption mark the empty Done column.
        assert!(painted.contains('╌'), "dashed placeholder edge: {painted}");
        assert!(painted.contains("empty"), "placeholder caption: {painted}");

        // The placeholder sits under the Done column (the 5th, rightmost). Its
        // dashed glyph must land in the rightmost fifth of the board.
        let dashed_x = buf
            .cells
            .iter()
            .filter(|(_, c)| c.symbol == "╌")
            .map(|(coord, _)| coord.x)
            .min()
            .expect("a dashed glyph is painted");
        assert!(
            dashed_x >= 96,
            "placeholder must sit in the Done column, got x={dashed_x}"
        );
    }

    /// No cell is written outside the 80×24 floor with a dense fixture.
    #[test]
    fn no_overflow_at_80x24_floor() {
        const FLOOR_W: u16 = 80;
        const FLOOR_H: u16 = 24;
        // Pack every column with several cards so the scroll/clip path runs.
        let columns: Vec<BoardColumn> = (0..COLUMN_COUNT_TEST)
            .map(|i| {
                let cards: Vec<BoardCard> = (0..8)
                    .map(|j| {
                        card(
                            &format!("HGR-{i}-{j}"),
                            "A title long enough to wrap across two lines and then ellipsise",
                            PriorityChip::from_priority(i64::from(j % 4)),
                            Some('z'),
                        )
                    })
                    .collect();
                column('◔', "In Progress", cards)
            })
            .collect();

        let mut buf = WireBuffer::new(FLOOR_W, FLOOR_H);
        let _ = render_card_board(&mut buf, FLOOR_W, 1, FLOOR_H - 1, &columns, Some((0, 0)));
        for (coord, _) in &buf.cells {
            assert!(
                coord.x < FLOOR_W && coord.y < FLOOR_H,
                "card board wrote out-of-bounds cell at ({}, {})",
                coord.x,
                coord.y,
            );
        }
    }

    /// The number of canonical columns the floor test packs (mirrors the
    /// caller's five-status board).
    const COLUMN_COUNT_TEST: usize = 5;

    /// The returned layout tags each painted card with its issue id and a rect
    /// that contains the card's cells — the geometry P0.2 hit-tests.
    #[test]
    fn layout_tags_cards_with_hit_testable_rects() {
        let mut buf = WireBuffer::new(120, 24);
        let layout = render_card_board(&mut buf, 120, 0, 23, &five_columns(), None);
        assert_eq!(layout.columns.len(), 5, "five columns laid out");

        // The In Progress column (index 2) has one card tagged HGR-3.
        let col = &layout.columns[2];
        assert_eq!(col.index, 2);
        assert_eq!(col.cards.len(), 1);
        let card_layout = &col.cards[0];
        assert_eq!(card_layout.issue_id, "HGR-3");

        // A point inside that card's rect resolves to it via card_at.
        let r = card_layout.rect;
        let hit = layout.card_at(r.x + 1, r.y + 1);
        assert_eq!(hit.map(|c| c.issue_id.as_str()), Some("HGR-3"));

        // A point in the empty Done column resolves to no card but to the column.
        let done = &layout.columns[4];
        let mid_x = done.rect.x + done.rect.w / 2;
        let mid_y = done.rect.y + done.rect.h / 2;
        assert!(
            layout.card_at(mid_x, mid_y).is_none(),
            "empty column has no card"
        );
        assert_eq!(
            layout.column_at(mid_x, mid_y).map(|c| c.index),
            Some(4),
            "click in the Done column resolves to it",
        );
    }

    /// Each column header exposes a `+` create affordance x in the layout (the
    /// P0.2 create hit-test target), inside the column's rect.
    #[test]
    fn header_exposes_create_affordance_for_p0_2() {
        let mut buf = WireBuffer::new(120, 24);
        let layout = render_card_board(&mut buf, 120, 0, 23, &five_columns(), None);
        for col in &layout.columns {
            let plus_x = col.create_affordance_x.expect("a wide column paints a + affordance");
            assert!(
                plus_x >= col.rect.x && plus_x < col.rect.right(),
                "the + affordance must sit inside the column rect",
            );
        }
    }

    /// Per-column scroll: a `scroll_offset` skips the leading cards, so the first
    /// visible card in the layout is the offset card, and the scrolled-off card's
    /// id never paints.
    #[test]
    fn per_column_scroll_skips_leading_cards() {
        let cards: Vec<BoardCard> = (0..6)
            .map(|i| card(&format!("HGR-{i}"), "Title", PriorityChip::None, None))
            .collect();
        let mut col = column('◔', "In Progress", cards);
        col.scroll_offset = 3; // skip HGR-0..HGR-2
        let columns = vec![col];

        let mut buf = WireBuffer::new(40, 24);
        let layout = render_card_board(&mut buf, 40, 0, 23, &columns, None);
        let painted = painted_text(&buf);

        // The scrolled-off cards never paint.
        assert!(
            !painted.contains("HGR-0"),
            "scrolled-off card must not paint: {painted}"
        );
        assert!(
            painted.contains("HGR-3"),
            "the offset card must paint: {painted}"
        );
        // And the layout's first visible card is the offset card.
        assert_eq!(
            layout.columns[0].cards.first().map(|c| c.issue_id.as_str()),
            Some("HGR-3")
        );
    }

    /// Priority scalar → chip mapping (HIGHER = MORE URGENT, default 0 = None).
    #[test]
    fn priority_scalar_maps_to_chip() {
        assert_eq!(PriorityChip::from_priority(0), PriorityChip::None);
        assert_eq!(PriorityChip::from_priority(1), PriorityChip::Medium);
        assert_eq!(PriorityChip::from_priority(2), PriorityChip::High);
        assert_eq!(PriorityChip::from_priority(3), PriorityChip::Urgent);
        // Out of range clamps to Urgent (fail-loud).
        assert_eq!(PriorityChip::from_priority(9), PriorityChip::Urgent);
    }

    /// A narrow area horizontally clips: it paints only the columns that fit at
    /// the minimum width, and the layout reflects the clip (fewer columns).
    #[test]
    fn narrow_area_clips_columns_horizontally() {
        // Only room for ~2 columns at MIN_COL_W (30 / 14 = 2).
        let mut buf = WireBuffer::new(30, 24);
        let layout = render_card_board(&mut buf, 30, 0, 23, &five_columns(), None);
        assert!(
            layout.columns.len() < 5,
            "a narrow board must clip columns, got {}",
            layout.columns.len(),
        );
        assert!(!layout.columns.is_empty(), "at least one column survives");
        // No overflow past the area.
        for (coord, _) in &buf.cells {
            assert!(
                coord.x < 30,
                "clipped board wrote past the area at x={}",
                coord.x
            );
        }
    }

    /// A long FSM-mapped header in a narrow column is clipped short of the
    /// right-aligned `⋯ +` affordances, so the title never garbles into them
    /// (the P4 narrow-Boards regression). The `⋯`/`+` glyphs paint cleanly and no
    /// header char bleeds through the cells between and after them.
    #[test]
    fn narrow_header_does_not_collide_with_affordances() {
        // One 14-wide (MIN_COL_W) column with an over-long, FSM-suffixed header.
        let columns = vec![column(
            '◈',
            "Queued ↦queued",
            vec![card("HGR-1", "Wire the RPC", PriorityChip::None, None)],
        )];
        let mut buf = WireBuffer::new(MIN_COL_W, 24);
        let _ = render_card_board(&mut buf, MIN_COL_W, 0, 23, &columns, None);

        // The affordances sit cleanly at their slots (`⋯` right-4, `+` right-2)…
        let right = MIN_COL_W;
        let ctx_x = right - 4;
        let plus_x = right - 2;
        let row0 = row_text(&buf, 0, MIN_COL_W);
        let cells: Vec<char> = row0.chars().collect();
        assert_eq!(
            cells[ctx_x as usize], '⋯',
            "the ⋯ affordance paints: {row0:?}"
        );
        assert_eq!(
            cells[plus_x as usize], '+',
            "the + affordance paints: {row0:?}"
        );
        // …and the header no longer bleeds THROUGH them: the cell between `⋯` and
        // `+`, and the inset cell past `+`, stay blank. In the garbled original a
        // clipped-but-unbounded header painted characters into exactly these cells
        // (`↦⋯u+u`), interleaving with the glyphs.
        assert_eq!(
            cells[(ctx_x + 1) as usize],
            ' ',
            "no header char may sit between ⋯ and +: {row0:?}"
        );
        assert_eq!(
            cells[(right - 1) as usize],
            ' ',
            "the inset past + stays blank: {row0:?}"
        );
        // And the leading title still reads (clipped, not truncated into nothing).
        assert!(
            row0.starts_with("◈ Queued"),
            "clipped header stays legible: {row0:?}"
        );
    }

    /// A multi-byte title wraps without panicking on a byte split
    /// (`reference_rust_utf8_truncate_trap`).
    #[test]
    fn multibyte_title_wraps_without_panic() {
        let columns = vec![column(
            '◔',
            "In Progress",
            vec![card(
                "HGR-1",
                "日本語のタイトルはとても長いのでラップされる必要があります",
                PriorityChip::High,
                Some('あ'),
            )],
        )];
        let mut buf = WireBuffer::new(40, 24);
        // Must not panic.
        let _ = render_card_board(&mut buf, 40, 0, 23, &columns, None);
    }

    /// The Rect hit-test predicate is half-open: it includes the origin and
    /// excludes the far edges.
    #[test]
    fn rect_contains_is_half_open() {
        let r = Rect::new(10, 5, 4, 3); // covers x 10..14, y 5..8
        assert!(r.contains(10, 5), "top-left inclusive");
        assert!(r.contains(13, 7), "last cell inclusive");
        assert!(!r.contains(14, 7), "right edge exclusive");
        assert!(!r.contains(13, 8), "bottom edge exclusive");
        assert!(!r.contains(9, 5), "left of origin excluded");
    }
}
