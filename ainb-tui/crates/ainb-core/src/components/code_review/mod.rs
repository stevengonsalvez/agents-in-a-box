// ABOUTME: Warp-style Code Review diff surface for the `G` git view.
// Structured diff model + git2/similar parser (this phase); Dracula syntax
// highlighting and the unified render surface are added in later phases.

pub mod highlight;
pub mod model;
pub mod parse;
pub mod render;

pub use render::CodeReviewUi;
