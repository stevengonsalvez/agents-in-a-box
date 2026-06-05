// ABOUTME: Warp-style Code Review diff surface for the `G` git view.
// model = structured diff types; parse = git2 + similar parser; highlight =
// Dracula syntax bridge; render = the unified sidebar-tree + diff-block surface
// plus its keyboard/mouse interaction helpers.

pub mod highlight;
pub mod model;
pub mod parse;
pub mod render;
