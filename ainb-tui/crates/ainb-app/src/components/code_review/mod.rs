// ABOUTME: Code Review diff surface for the `G` git view, renderer-agnostic half.
// model = structured diff types; parse = git2 + similar parser; render = the
// review UI state and its keyboard/mouse interaction helpers. The Dracula
// syntax bridge and the draw functions live in `ainb-core`.

pub mod model;
pub mod parse;
pub mod render;
