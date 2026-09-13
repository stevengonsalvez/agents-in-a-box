// ABOUTME: Warp-style Code Review diff surface for the `G` git view.
// model = structured diff types; parse = git2 + similar parser; highlight =
// Dracula syntax bridge; render = the unified sidebar-tree + diff-block surface
// plus its keyboard/mouse interaction helpers. The model, parser and review UI
// state live in `ainb_app::components::code_review`.

pub use ainb_app::components::code_review::*;

pub mod highlight;
pub mod render;
