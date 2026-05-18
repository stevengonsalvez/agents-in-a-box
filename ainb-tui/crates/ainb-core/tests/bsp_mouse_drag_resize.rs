// P6 drag-resize math: dx cells of drag on a split with parent_width W
// produces a new ratio current + dx/W, clamped to [0.05, 0.95] so the
// tile never fully collapses.

use ainb::ui::bsp_mouse::{apply_drag_horizontal, apply_drag_vertical};

#[test]
fn dragging_right_increases_ratio_proportionally() {
    // start at 0.5 of width 100 → 50px. drag +10 → ratio = 0.5 + 0.1 = 0.6
    let new = apply_drag_horizontal(0.5, 10, 100);
    assert!((new - 0.6).abs() < 1e-5);
}

#[test]
fn dragging_left_decreases_ratio_proportionally() {
    let new = apply_drag_horizontal(0.5, -20, 100);
    assert!((new - 0.3).abs() < 1e-5);
}

#[test]
fn drag_clamps_at_upper_bound() {
    let new = apply_drag_horizontal(0.9, 50, 100);
    assert!((new - 0.95).abs() < 1e-5);
}

#[test]
fn drag_clamps_at_lower_bound() {
    let new = apply_drag_horizontal(0.1, -50, 100);
    assert!((new - 0.05).abs() < 1e-5);
}

#[test]
fn drag_with_zero_parent_width_just_clamps() {
    // degenerate but shouldn't divide-by-zero or panic
    let new = apply_drag_horizontal(0.5, 10, 0);
    assert!((0.05..=0.95).contains(&new));
}

#[test]
fn vertical_drag_uses_dy_and_parent_height() {
    let new = apply_drag_vertical(0.5, 8, 40);
    assert!((new - 0.7).abs() < 1e-5);
}

#[test]
fn vertical_drag_clamps_same_as_horizontal() {
    assert!((apply_drag_vertical(0.95, 100, 50) - 0.95).abs() < 1e-5);
    assert!((apply_drag_vertical(0.05, -100, 50) - 0.05).abs() < 1e-5);
}
