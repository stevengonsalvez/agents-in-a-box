//! ABI v2 [`Plugin`] implementation for the learnings browser.
//!
//! P3 (scaffold) ships the empty-state shell only: a titled panel that
//! renders the `🧠 Learnings` header + an empty-state hint. The data
//! layer (P4) and the Browse / Detail / Search / Graph tabs (P5–P8) hang
//! off this struct in later phases.
//!
//! Render mirrors burndown: paint locally into a ratatui `Buffer`, then
//! convert to a sparse [`WireBuffer`] cell stream for the host
//! ([`buffer_to_wire`]). The host re-paints each cell at its `(x, y)`.

use async_trait::async_trait;

use ainb_plugin_sdk::{
    Cell, Color, Coord, HostClient, InitContext, Plugin, RenderParams, Result, WireBuffer,
};
use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::Rect as RRect;
use ratatui::style::{Color as RColor, Modifier as RModifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use crate::config::LearningsConfig;

/// Static manifest TOML compiled into the binary. The SDK `Server` uses
/// this on `plugin/init` to echo `name`/`version` back to the host.
const MANIFEST_TOML: &str = include_str!("../manifest.toml");

/// Title token painted in the panel header. Unique + stable so the
/// tmux-ui-tripwire can assert an exact match (never a substring-OR).
pub const TITLE_TOKEN: &str = "🧠 Learnings";

/// Empty-state body shown until the data layer (P4) + tabs (P5–P8) land.
const EMPTY_HINT: &str = "No learnings loaded yet.";

/// Fallback viewport if the host sends a degenerate 0×0 — matches the
/// historical 80×24 baseline used across the in-tree plugins.
const FALLBACK_VIEWPORT: (u16, u16) = (80, 24);

/// TUI palette (see `../.claude/skills/tui-screen/SKILL.md`). Gold title,
/// cornflower borders.
const GOLD: RColor = RColor::Rgb(255, 215, 0);
const CORNFLOWER_BLUE: RColor = RColor::Rgb(100, 149, 237);
const MUTED_GRAY: RColor = RColor::Rgb(120, 120, 140);

/// Learnings plugin state.
///
/// P3 holds only the resolved [`LearningsConfig`]; the snapshot caches,
/// record store, and per-tab UI state arrive in P4+.
#[derive(Debug, Default)]
pub struct LearningsPlugin {
    /// Resolved config injected at `plugin/init` and applied in
    /// [`Self::on_init`]. Holds the schema defaults until init runs.
    config: LearningsConfig,
}

impl LearningsPlugin {
    /// Borrow the resolved config. Used by the data layer (P4) to locate
    /// the KB dirs; exposed now so tests can assert the parse landed.
    #[must_use]
    pub const fn config(&self) -> &LearningsConfig {
        &self.config
    }

    /// Replace the resolved config from an injected
    /// `PluginInitParams.config` JSON value.
    ///
    /// This is the parse path [`Self::on_init`] consumes via
    /// [`InitContext::config`]. Kept as a `&mut self` method (rather than
    /// inline in `on_init`) so unit tests can drive it without the SDK
    /// `Server`.
    pub fn apply_init_config(&mut self, value: &serde_json::Value) {
        self.config = LearningsConfig::from_init_config(value);
    }
}

#[async_trait]
impl Plugin for LearningsPlugin {
    fn manifest(&self) -> &'static str {
        MANIFEST_TOML
    }

    /// One-shot init.
    ///
    /// The host resolves the `[plugins.learnings]` table (project → user →
    /// system, P1) and injects it on `PluginInitParams.config`; the SDK
    /// forwards it via [`InitContext::config`]. We parse it into the typed
    /// [`LearningsConfig`] (defaulting any absent key) and stash it on the
    /// plugin so the data layer (P4) can locate the KB dirs.
    async fn on_init(&mut self, _host: &HostClient, ctx: InitContext<'_>) -> Result<()> {
        self.apply_init_config(ctx.config);
        Ok(())
    }

    async fn render(&mut self, _host: &HostClient, params: RenderParams) -> Result<WireBuffer> {
        let (w, h) = match (params.viewport.width, params.viewport.height) {
            (0, _) | (_, 0) => FALLBACK_VIEWPORT,
            (w, h) => (w, h),
        };
        let area = RRect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut rbuf = RBuffer::empty(area);
        render_empty_state(&mut rbuf, area);
        Ok(buffer_to_wire(&rbuf, area))
    }
}

/// Paint the P3 empty-state shell: a rounded, cornflower-bordered panel
/// titled `🧠 Learnings` (gold, bold) with a muted hint line in the body.
fn render_empty_state(buf: &mut RBuffer, area: RRect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .title(TITLE_TOKEN)
        .title_style(Style::default().fg(GOLD).add_modifier(RModifier::BOLD));

    let inner = block.inner(area);
    block.render(area, buf);

    if inner.width > 0 && inner.height > 0 {
        Paragraph::new(EMPTY_HINT)
            .style(Style::default().fg(MUTED_GRAY))
            .render(inner, buf);
    }
}

/// Convert a ratatui `Buffer` to the SDK's sparse [`WireBuffer`] cell
/// stream. Row-major so the `Vec<(Coord, Cell)>` is deterministic;
/// fully-blank default cells are dropped to keep the wire payload sparse.
/// Mirrors burndown's `buffer_to_wire`.
fn buffer_to_wire(rbuf: &RBuffer, area: RRect) -> WireBuffer {
    let mut wire = WireBuffer::new(area.width, area.height);
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = rbuf.get(area.x + x, area.y + y);
            let symbol = cell.symbol();
            let fg = ratatui_color(cell.fg);
            let bg = ratatui_color(cell.bg);
            let modifier = ratatui_modifiers(cell.modifier);
            if symbol == " " && fg.is_none() && bg.is_none() && modifier == 0 {
                continue;
            }
            wire.push(
                Coord::new(x, y),
                Cell {
                    symbol: symbol.to_string(),
                    fg,
                    bg,
                    modifier,
                },
            );
        }
    }
    wire
}

fn ratatui_color(c: RColor) -> Option<Color> {
    match c {
        RColor::Reset => None,
        RColor::Black => Some(Color::rgb(0, 0, 0)),
        RColor::Red => Some(Color::rgb(170, 0, 0)),
        RColor::Green => Some(Color::rgb(0, 170, 0)),
        RColor::Yellow => Some(Color::rgb(170, 170, 0)),
        RColor::Blue => Some(Color::rgb(0, 0, 170)),
        RColor::Magenta => Some(Color::rgb(170, 0, 170)),
        RColor::Cyan => Some(Color::rgb(0, 170, 170)),
        RColor::Gray => Some(Color::rgb(170, 170, 170)),
        RColor::DarkGray => Some(Color::rgb(85, 85, 85)),
        RColor::LightRed => Some(Color::rgb(255, 85, 85)),
        RColor::LightGreen => Some(Color::rgb(85, 255, 85)),
        RColor::LightYellow => Some(Color::rgb(255, 255, 85)),
        RColor::LightBlue => Some(Color::rgb(85, 85, 255)),
        RColor::LightMagenta => Some(Color::rgb(255, 85, 255)),
        RColor::LightCyan => Some(Color::rgb(85, 255, 255)),
        RColor::White => Some(Color::rgb(255, 255, 255)),
        RColor::Indexed(_) => None,
        RColor::Rgb(r, g, b) => Some(Color::rgb(r, g, b)),
    }
}

fn ratatui_modifiers(m: RModifier) -> u16 {
    let mut out = 0_u16;
    if m.contains(RModifier::BOLD) {
        out |= 1;
    }
    if m.contains(RModifier::DIM) {
        out |= 2;
    }
    if m.contains(RModifier::ITALIC) {
        out |= 4;
    }
    if m.contains(RModifier::UNDERLINED) {
        out |= 8;
    }
    if m.contains(RModifier::REVERSED) {
        out |= 16;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_paints_title_into_buffer() {
        // Pure render path (no Server): the title token must appear in the
        // first row's cells.
        let area = RRect {
            x: 0,
            y: 0,
            width: 40,
            height: 6,
        };
        let mut rbuf = RBuffer::empty(area);
        render_empty_state(&mut rbuf, area);
        let wire = buffer_to_wire(&rbuf, area);
        let row0: String = wire
            .cells
            .iter()
            .filter(|(c, _)| c.y == 0)
            .map(|(_, cell)| cell.symbol.as_str())
            .collect();
        assert!(
            row0.contains("Learnings"),
            "title row must contain the Learnings token, got: {row0:?}"
        );
    }

    #[test]
    fn apply_init_config_updates_state() {
        let mut p = LearningsPlugin::default();
        p.apply_init_config(&serde_json::json!({ "learnings_dir": "/kb" }));
        assert_eq!(p.config().learnings_dir, "/kb");
    }
}
