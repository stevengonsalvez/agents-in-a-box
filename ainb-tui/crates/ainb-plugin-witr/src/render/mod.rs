//! Render paths for the witr plugin.
//!
//! cfx.4 ships the [`empty`] module — the "witr missing / outdated"
//! empty-state painter. Subsequent beads (cfx.5 / cfx.6) add the
//! per-tab process / ports / containers / locks renderers and the
//! detail overlay.
//!
//! Painters write directly into the protocol's [`WireBuffer`] rather
//! than going through `ratatui` — the empty state is simple enough
//! (a handful of lines of static text) that absolute-coord cell
//! pushes are clearer than dragging the ratatui buffer-conversion
//! machinery in. cfx.5 will revisit when the main screen needs real
//! layout.
//!
//! TODO(cfx.5): introduce `ratatui::buffer::Buffer` + a `buffer_to_wire`
//! conversion (mirror `ainb-plugin-burndown/src/plugin.rs` ~lines 608–640)
//! once the tab UI needs `Layout::default().constraints(...)`.

pub mod empty;

#[cfg(test)]
pub(crate) mod test_support;
