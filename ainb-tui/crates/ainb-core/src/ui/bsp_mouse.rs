//! Mouse hit-test + drag-resize primitives for the BSP layout.
//!
//! [`TileRegion`] records one painted leaf — its [`LeafId`] and the
//! [`Rect`] it occupies on the screen. The host stamps a fresh
//! `Vec<TileRegion>` every frame after walking the BSP tree; the input
//! loop reads that vec on the next mouse event to map `(col, row)` to a
//! leaf via [`hit_test`].
//!
//! [`hit_test`] returns `HitTarget::Interior` for clicks inside a leaf
//! (focus / scroll routing) and `HitTarget::Border { edge }` for clicks
//! exactly on a tile border (start of a drag-to-resize gesture).
//!
//! [`apply_drag`] adjusts the parent [`Split`]'s `ratio` proportionally
//! to the drag delta in cells. Min-size enforcement is left to the
//! caller (use [`LayoutNode::walk_checked`] after applying the drag).

use ratatui::layout::Rect;

use crate::ui::bsp::LeafId;

/// Stamped per-frame: one leaf's id + the rect it was painted into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileRegion {
    pub leaf_id: LeafId,
    pub rect: Rect,
}

/// Which edge of a tile a border-click landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderEdge {
    Top,
    Bottom,
    Left,
    Right,
}

/// Result of [`hit_test`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    /// Click inside a leaf, not on its border.
    Interior { leaf_id: LeafId },
    /// Click on a tile border — start of a drag-to-resize gesture.
    Border { leaf_id: LeafId, edge: BorderEdge },
}

/// Map a `(col, row)` pixel coordinate to the leaf it falls on (and
/// whether it landed exactly on a border edge). Returns `None` if the
/// point is outside every tile.
pub fn hit_test(regions: &[TileRegion], col: u16, row: u16) -> Option<HitTarget> {
    for region in regions {
        let r = region.rect;
        let in_x = col >= r.x && col < r.x.saturating_add(r.width);
        let in_y = row >= r.y && row < r.y.saturating_add(r.height);
        if !in_x || !in_y {
            continue;
        }
        // is this point exactly on an edge?
        let on_left = col == r.x;
        let on_right = col + 1 == r.x.saturating_add(r.width);
        let on_top = row == r.y;
        let on_bottom = row + 1 == r.y.saturating_add(r.height);
        let edge = if on_left {
            Some(BorderEdge::Left)
        } else if on_right {
            Some(BorderEdge::Right)
        } else if on_top {
            Some(BorderEdge::Top)
        } else if on_bottom {
            Some(BorderEdge::Bottom)
        } else {
            None
        };
        return Some(match edge {
            Some(edge) => HitTarget::Border {
                leaf_id: region.leaf_id.clone(),
                edge,
            },
            None => HitTarget::Interior {
                leaf_id: region.leaf_id.clone(),
            },
        });
    }
    None
}

/// Compute a new ratio for a horizontal (side-by-side) split's parent
/// after a drag of `dx` cells. `parent_width` is the parent split's
/// painted width.
///
/// Returns the clamped new ratio in `0.05..=0.95` so drags can never
/// fully collapse a tile (callers should still validate via
/// `walk_checked` for the real min-size enforcement).
pub fn apply_drag_horizontal(current_ratio: f32, dx: i32, parent_width: u16) -> f32 {
    if parent_width == 0 {
        return current_ratio.clamp(0.05, 0.95);
    }
    let delta = dx as f32 / parent_width as f32;
    (current_ratio + delta).clamp(0.05, 0.95)
}

/// Same as [`apply_drag_horizontal`] but for a vertical (stacked) split.
pub fn apply_drag_vertical(current_ratio: f32, dy: i32, parent_height: u16) -> f32 {
    if parent_height == 0 {
        return current_ratio.clamp(0.05, 0.95);
    }
    let delta = dy as f32 / parent_height as f32;
    (current_ratio + delta).clamp(0.05, 0.95)
}
