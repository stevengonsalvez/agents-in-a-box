//! Binary space-partition layout tree for the ainb main content region.
//!
//! `LayoutNode` is the recursive tree: a leaf names a component / plugin
//! (`Pane`), an internal node bisects its parent's `Rect` either side-by-side
//! (`SplitDir::Horizontal`) or stacked (`SplitDir::Vertical`).
//!
//! Hosting code calls `walk` (unchecked) or `walk_checked` (with min-size
//! enforcement) to produce the per-leaf `Rect` list ready for compositing.
//! `cycle_focus_next/prev`, `close_focused`, and `split_focused` mutate the
//! tree in response to user keystrokes / mouse actions.
//!
//! ## Focus invariant
//!
//! Code in this module is written assuming **at most one leaf is focused at
//! a time**. The constructors don't enforce that — callers must ensure they
//! don't `.focused(true)` two siblings. The cycle / close / split mutators
//! preserve the single-focused-leaf shape; `cycle_focus_*` collapses any
//! accidental multi-focus to whichever leaf comes first in pre-order.

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type LeafId = String;

/// Direction of a binary split.
///
/// `Horizontal` = panes arranged side-by-side (the divider line is vertical,
/// the ratio applies to width). `Vertical` = panes stacked top-over-bottom
/// (the divider line is horizontal, the ratio applies to height).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDir {
    Horizontal,
    Vertical,
}

/// Recursive layout tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutNode {
    Pane {
        id: LeafId,
        #[serde(default)]
        focused: bool,
    },
    Split {
        dir: SplitDir,
        ratio: f32,
        left: Box<LayoutNode>,
        right: Box<LayoutNode>,
    },
}

/// Persistable snapshot wrapper.
///
/// Fields use `#[serde(default)]` so a future schema bump that adds new
/// optional fields can land without breaking already-on-disk snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutSnapshot {
    #[serde(default = "default_snapshot_version")]
    pub version: u32,
    pub root: LayoutNode,
    #[serde(default = "default_min_cols")]
    pub min_cols: u16,
    #[serde(default = "default_min_rows")]
    pub min_rows: u16,
}

fn default_snapshot_version() -> u32 {
    1
}
fn default_min_cols() -> u16 {
    30
}
fn default_min_rows() -> u16 {
    10
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BspError {
    #[error("tile too small: {got_w}x{got_h} < min {min_w}x{min_h}")]
    TileTooSmall {
        got_w: u16,
        got_h: u16,
        min_w: u16,
        min_h: u16,
    },
}

impl LayoutNode {
    /// Default BSP root for the home screen: a 40/60 horizontal split of
    /// `session_list` over `live_logs_stream`. Matches the legacy
    /// `LayoutComponent::render` hardcoded `Constraint::Percentage(40) /
    /// Percentage(60)` split (see `components/layout.rs:92`), so swapping
    /// the legacy path for `bsp.walk(...)` produces visually identical
    /// output at the same area.
    pub fn default_root() -> Self {
        LayoutNode::split_h(
            0.4,
            LayoutNode::pane("session_list").focused(true),
            LayoutNode::pane("live_logs_stream"),
        )
    }

    pub fn pane(id: impl Into<LeafId>) -> Self {
        LayoutNode::Pane {
            id: id.into(),
            focused: false,
        }
    }

    /// Builder: set focus state on a `Pane`. Silently returns `self`
    /// unchanged when called on a `Split` — focus only exists on leaves.
    pub fn focused(self, f: bool) -> Self {
        match self {
            LayoutNode::Pane { id, .. } => LayoutNode::Pane { id, focused: f },
            other => other,
        }
    }

    /// Side-by-side split. `ratio` is clamped to `0.0..=1.0` and applies to
    /// the left tile. Non-finite ratios (`NaN`, `±inf`) become `0.5` because
    /// JSON has no NaN encoding and a NaN ratio would break serde round-trip.
    pub fn split_h(ratio: f32, left: LayoutNode, right: LayoutNode) -> Self {
        LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: sanitize_ratio(ratio),
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    /// Stacked split. `ratio` is clamped to `0.0..=1.0` and applies to the
    /// top tile. Non-finite ratios become `0.5` (see `split_h` for why).
    pub fn split_v(ratio: f32, top: LayoutNode, bottom: LayoutNode) -> Self {
        LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratio: sanitize_ratio(ratio),
            left: Box::new(top),
            right: Box::new(bottom),
        }
    }

    pub fn ratio(&self) -> Option<f32> {
        match self {
            LayoutNode::Split { ratio, .. } => Some(*ratio),
            _ => None,
        }
    }

    /// Recursively assign a `Rect` to each leaf in pre-order.
    pub fn walk(&self, area: Rect) -> Vec<(LeafId, Rect)> {
        let mut out = Vec::new();
        walk_into(self, area, &mut out);
        out
    }

    /// Like `walk`, but rejects layouts where any leaf falls below `min_w` /
    /// `min_h`.
    pub fn walk_checked(
        &self,
        area: Rect,
        min_w: u16,
        min_h: u16,
    ) -> Result<Vec<(LeafId, Rect)>, BspError> {
        let leaves = self.walk(area);
        for (_, r) in &leaves {
            if r.width < min_w || r.height < min_h {
                return Err(BspError::TileTooSmall {
                    got_w: r.width,
                    got_h: r.height,
                    min_w,
                    min_h,
                });
            }
        }
        Ok(leaves)
    }

    /// Move focus to the next leaf in pre-order (wrapping). If no leaf is
    /// focused, focus the first one.
    pub fn cycle_focus_next(&mut self) {
        let ids = leaf_focus_state(self);
        if ids.is_empty() {
            return;
        }
        let next_idx = match ids.iter().position(|(_, f)| *f) {
            Some(i) => (i + 1) % ids.len(),
            None => 0,
        };
        let next_id = ids[next_idx].0.clone();
        set_focused_only(self, &next_id);
    }

    /// Move focus to the previous leaf in pre-order (wrapping). If no leaf
    /// is focused, focus the last one.
    pub fn cycle_focus_prev(&mut self) {
        let ids = leaf_focus_state(self);
        if ids.is_empty() {
            return;
        }
        let prev_idx = match ids.iter().position(|(_, f)| *f) {
            Some(0) => ids.len() - 1,
            Some(i) => i - 1,
            None => ids.len() - 1,
        };
        let prev_id = ids[prev_idx].0.clone();
        set_focused_only(self, &prev_id);
    }

    /// Remove the focused leaf, promoting its sibling subtree into the
    /// parent's slot. Focus migrates to the **first leaf of the promoted
    /// subtree** (pre-order). No-op (returns false) when the focused leaf
    /// is the root or no leaf is focused.
    pub fn close_focused(&mut self) -> bool {
        if let LayoutNode::Pane { focused: true, .. } = self {
            return false;
        }
        matches!(close_focused_inner(self), CloseResult::Done)
    }

    /// Replace the focused leaf with a `Split` whose left child is the
    /// original focused leaf and whose right child is `Pane(new_leaf)`.
    pub fn split_focused(&mut self, dir: SplitDir, ratio: f32, new_leaf: LeafId) {
        split_focused_inner(self, dir, sanitize_ratio(ratio), new_leaf);
    }
}

// ─── private helpers ─────────────────────────────────────────────────────────

fn sanitize_ratio(r: f32) -> f32 {
    if r.is_finite() {
        r.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

fn walk_into(node: &LayoutNode, area: Rect, out: &mut Vec<(LeafId, Rect)>) {
    match node {
        LayoutNode::Pane { id, .. } => out.push((id.clone(), area)),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => match dir {
            SplitDir::Horizontal => {
                let left_w = (area.width as f32 * *ratio).round() as u16;
                let left_rect = Rect {
                    x: area.x,
                    y: area.y,
                    width: left_w,
                    height: area.height,
                };
                let right_rect = Rect {
                    x: area.x.saturating_add(left_w),
                    y: area.y,
                    width: area.width.saturating_sub(left_w),
                    height: area.height,
                };
                walk_into(left, left_rect, out);
                walk_into(right, right_rect, out);
            }
            SplitDir::Vertical => {
                let top_h = (area.height as f32 * *ratio).round() as u16;
                let top_rect = Rect {
                    x: area.x,
                    y: area.y,
                    width: area.width,
                    height: top_h,
                };
                let bottom_rect = Rect {
                    x: area.x,
                    y: area.y.saturating_add(top_h),
                    width: area.width,
                    height: area.height.saturating_sub(top_h),
                };
                walk_into(left, top_rect, out);
                walk_into(right, bottom_rect, out);
            }
        },
    }
}

fn leaf_focus_state(node: &LayoutNode) -> Vec<(LeafId, bool)> {
    let mut out = Vec::new();
    leaf_focus_state_inner(node, &mut out);
    out
}

fn leaf_focus_state_inner(node: &LayoutNode, out: &mut Vec<(LeafId, bool)>) {
    match node {
        LayoutNode::Pane { id, focused } => out.push((id.clone(), *focused)),
        LayoutNode::Split { left, right, .. } => {
            leaf_focus_state_inner(left, out);
            leaf_focus_state_inner(right, out);
        }
    }
}

fn set_focused_only(node: &mut LayoutNode, target_id: &str) {
    match node {
        LayoutNode::Pane { id, focused } => *focused = id == target_id,
        LayoutNode::Split { left, right, .. } => {
            set_focused_only(left, target_id);
            set_focused_only(right, target_id);
        }
    }
}

enum CloseResult {
    NotFound,
    Done,
}

fn close_focused_inner(node: &mut LayoutNode) -> CloseResult {
    let (l_focused_leaf, r_focused_leaf) = match node {
        LayoutNode::Pane { .. } => return CloseResult::NotFound,
        LayoutNode::Split { left, right, .. } => (
            matches!(left.as_ref(), LayoutNode::Pane { focused: true, .. }),
            matches!(right.as_ref(), LayoutNode::Pane { focused: true, .. }),
        ),
    };

    if l_focused_leaf {
        // Promote right subtree into self's slot.
        let placeholder = LayoutNode::Pane {
            id: String::new(),
            focused: false,
        };
        if let LayoutNode::Split { right, .. } = std::mem::replace(node, placeholder) {
            *node = *right;
            focus_first_leaf(node);
            return CloseResult::Done;
        }
        unreachable!("matched as Split above");
    }
    if r_focused_leaf {
        let placeholder = LayoutNode::Pane {
            id: String::new(),
            focused: false,
        };
        if let LayoutNode::Split { left, .. } = std::mem::replace(node, placeholder) {
            *node = *left;
            focus_first_leaf(node);
            return CloseResult::Done;
        }
        unreachable!("matched as Split above");
    }

    if let LayoutNode::Split { left, right, .. } = node {
        if matches!(close_focused_inner(left.as_mut()), CloseResult::Done) {
            return CloseResult::Done;
        }
        if matches!(close_focused_inner(right.as_mut()), CloseResult::Done) {
            return CloseResult::Done;
        }
    }
    CloseResult::NotFound
}

fn focus_first_leaf(node: &mut LayoutNode) {
    match node {
        LayoutNode::Pane { focused, .. } => *focused = true,
        LayoutNode::Split { left, .. } => focus_first_leaf(left),
    }
}

fn split_focused_inner(node: &mut LayoutNode, dir: SplitDir, ratio: f32, new_leaf: LeafId) -> bool {
    match node {
        LayoutNode::Pane { focused: true, .. } => {
            let placeholder = LayoutNode::Pane {
                id: String::new(),
                focused: false,
            };
            let original = std::mem::replace(node, placeholder);
            *node = LayoutNode::Split {
                dir,
                ratio,
                left: Box::new(original),
                right: Box::new(LayoutNode::Pane {
                    id: new_leaf,
                    focused: false,
                }),
            };
            true
        }
        LayoutNode::Pane { .. } => false,
        LayoutNode::Split { left, right, .. } => {
            let cloned = new_leaf.clone();
            if split_focused_inner(left, dir, ratio, cloned) {
                return true;
            }
            split_focused_inner(right, dir, ratio, new_leaf)
        }
    }
}
