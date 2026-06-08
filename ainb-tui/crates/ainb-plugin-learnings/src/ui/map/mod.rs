//! Radial ego local-graph map — the 3rd Graph sub-mode (`v` toggles to it).
//!
//! A deterministic, Obsidian-style local-graph rendered on the character grid:
//! the list-selected entity at the centre with its typed neighbours on hop
//! rings. Pipeline, one module per stage:
//!
//! ```text
//! ego    → extract centre + N-hop neighbourhood (capped → +N more)
//! layout → deterministic (x, y) per node (centre at mid, hop-k on ring k)
//! render → boxed [entity] nodes · ASCII edges · arrowheads · type labels
//! state  → centre / hop / selection / expanded / recentre-animation frame
//! ```
//!
//! Determinism is the whole point: no physics, no RNG — so the tripwires can
//! assert exact entity / edge-label tokens instead of fuzzy coordinates.

pub mod ego;
pub mod layout;
