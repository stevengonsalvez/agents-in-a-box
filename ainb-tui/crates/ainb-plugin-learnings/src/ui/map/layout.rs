//! Deterministic radial layout for the ego subgraph.
//!
//! No physics, no RNG: the centre sits at the viewport midpoint and each ring-k
//! node lands on an ellipse of radius proportional to `k`, with the ring's nodes
//! spread across even angular slots in [`EgoSubgraph`] order. Same subgraph +
//! same viewport → byte-identical placement, every run — which is what lets the
//! tripwires assert exact rendered tokens instead of fuzzy coordinates.
//!
//! Terminal cells are about twice as tall as they are wide, so the horizontal
//! radius is larger than the vertical one to keep rings looking round rather
//! than squashed.

use std::f64::consts::PI;

use super::ego::EgoSubgraph;

/// A node placed at a concrete cell. `(x, y)` is the node box's *anchor centre*
/// — the renderer centres the `[label]` box on it and the hit-test derives the
/// box rect from it, so both agree on geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    /// Entity name, or the `+N more` overflow label.
    pub name: String,
    /// Anchor column (centre of the node box).
    pub x: u16,
    /// Anchor row (centre of the node box).
    pub y: u16,
    /// Hop ring (0 = centre).
    pub ring: u8,
    /// `true` for the synthetic overflow node.
    pub overflow: bool,
}

/// Cells reserved around the edge of the viewport so node boxes don't clip the
/// border. Horizontal margin is larger because boxes extend sideways.
const MARGIN_X: u16 = 8;
const MARGIN_Y: u16 = 2;

/// Place every node of `ego` into the `width × height` viewport. The returned
/// vec preserves [`EgoSubgraph`] node order (centre first), so callers can zip
/// it back to the subgraph if needed.
#[must_use]
pub fn layout(ego: &EgoSubgraph, width: u16, height: u16) -> Vec<Placed> {
    let mut placed = Vec::with_capacity(ego.nodes.len());
    if width == 0 || height == 0 || ego.nodes.is_empty() {
        return placed;
    }

    let cx = f64::from(width) / 2.0;
    let cy = f64::from(height) / 2.0;
    let max_x = width.saturating_sub(1);
    let max_y = height.saturating_sub(1);

    // Outermost ring present (>=1 so a centre-only graph still divides cleanly).
    let ring_count = ego.nodes.iter().map(|n| n.ring).max().unwrap_or(0).max(1);

    // Radius budget: leave a margin so boxes stay inside the border.
    let avail_x = (cx - f64::from(MARGIN_X)).max(1.0);
    let avail_y = (cy - f64::from(MARGIN_Y)).max(1.0);

    // Group node indices by ring to compute even angular slots per ring.
    for (ring, indices) in group_by_ring(ego) {
        if ring == 0 {
            for i in indices {
                placed.push(Placed {
                    name: ego.nodes[i].name.clone(),
                    x: clamp_round(cx, max_x),
                    y: clamp_round(cy, max_y),
                    ring: 0,
                    overflow: ego.nodes[i].overflow,
                });
            }
            continue;
        }

        let radius_x = avail_x * f64::from(ring) / f64::from(ring_count);
        let radius_y = avail_y * f64::from(ring) / f64::from(ring_count);
        let count = indices.len();
        // Even angular slots. With the screen y-axis flipped below (`cy - …`),
        // the maths angle +90° maps to the top of the viewport, so the first
        // ring node sits directly above the centre — a stable anchor for the
        // deterministic order.
        for (slot, i) in indices.into_iter().enumerate() {
            let frac = slot as f64 / count as f64;
            let angle = PI / 2.0 + frac * 2.0 * PI;
            let x = cx + radius_x * angle.cos();
            let y = cy - radius_y * angle.sin();
            placed.push(Placed {
                name: ego.nodes[i].name.clone(),
                x: clamp_round(x, max_x),
                y: clamp_round(y, max_y),
                ring,
                overflow: ego.nodes[i].overflow,
            });
        }
    }

    placed
}

/// Group node indices by ring, ascending ring, preserving [`EgoSubgraph`] order
/// within each ring (the source of angular-slot determinism).
fn group_by_ring(ego: &EgoSubgraph) -> Vec<(u8, Vec<usize>)> {
    let mut rings: Vec<(u8, Vec<usize>)> = Vec::new();
    let max_ring = ego.nodes.iter().map(|n| n.ring).max().unwrap_or(0);
    for ring in 0..=max_ring {
        let indices: Vec<usize> = ego
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.ring == ring)
            .map(|(i, _)| i)
            .collect();
        if !indices.is_empty() {
            rings.push((ring, indices));
        }
    }
    rings
}

/// Round a float coordinate to the nearest cell and clamp into `0..=max`.
fn clamp_round(v: f64, max: u16) -> u16 {
    let r = v.round();
    if r < 0.0 {
        0
    } else if r > f64::from(max) {
        max
    } else {
        r as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Relationship;
    use crate::ui::map::ego::{Adjacency, DEFAULT_NODE_CAP};

    fn rel(source: &str, target: &str, strength: u32) -> Relationship {
        Relationship {
            source: source.into(),
            target: target.into(),
            rel_type: "solves".into(),
            description: String::new(),
            strength: Some(strength),
        }
    }

    fn find<'a>(placed: &'a [Placed], name: &str) -> &'a Placed {
        placed.iter().find(|p| p.name == name).expect("node placed")
    }

    fn dist2(a: &Placed, b: &Placed) -> f64 {
        let dx = f64::from(a.x) - f64::from(b.x);
        let dy = f64::from(a.y) - f64::from(b.y);
        dx * dx + dy * dy
    }

    #[test]
    fn centre_sits_at_viewport_midpoint() {
        let ego = EgoSubgraph::build(
            &Adjacency::build(&[rel("c", "n", 5)]),
            "c",
            1,
            DEFAULT_NODE_CAP,
            false,
        );
        let placed = layout(&ego, 80, 24);
        let centre = find(&placed, "c");
        assert_eq!(centre.x, 40);
        assert_eq!(centre.y, 12);
        assert_eq!(centre.ring, 0);
    }

    #[test]
    fn all_nodes_stay_in_bounds() {
        let rels: Vec<Relationship> = (0..8).map(|i| rel("hub", &format!("n{i}"), 5)).collect();
        let ego = EgoSubgraph::build(&Adjacency::build(&rels), "hub", 1, DEFAULT_NODE_CAP, false);
        let placed = layout(&ego, 60, 20);
        assert_eq!(placed.len(), ego.nodes.len());
        for p in &placed {
            assert!(p.x < 60, "{} x={} out of bounds", p.name, p.x);
            assert!(p.y < 20, "{} y={} out of bounds", p.name, p.y);
        }
    }

    #[test]
    fn ring_two_is_farther_from_centre_than_ring_one() {
        let rels = vec![rel("c", "mid", 5), rel("mid", "leaf", 5)];
        let ego = EgoSubgraph::build(&Adjacency::build(&rels), "c", 2, DEFAULT_NODE_CAP, false);
        let placed = layout(&ego, 80, 24);
        let centre = find(&placed, "c");
        let mid = find(&placed, "mid");
        let leaf = find(&placed, "leaf");
        assert_eq!(mid.ring, 1);
        assert_eq!(leaf.ring, 2);
        assert!(
            dist2(leaf, centre) > dist2(mid, centre),
            "ring-2 leaf must be farther from centre than ring-1 mid"
        );
    }

    #[test]
    fn layout_is_deterministic() {
        let rels: Vec<Relationship> = (0..6).map(|i| rel("hub", &format!("n{i}"), 5)).collect();
        let ego = EgoSubgraph::build(&Adjacency::build(&rels), "hub", 1, DEFAULT_NODE_CAP, false);
        let a = layout(&ego, 80, 24);
        let b = layout(&ego, 80, 24);
        assert_eq!(a, b, "same subgraph + viewport → identical placement");
    }

    #[test]
    fn first_ring_node_sits_above_centre() {
        // Single neighbour starts at the top (−90°), directly above the centre.
        let ego = EgoSubgraph::build(
            &Adjacency::build(&[rel("c", "n", 5)]),
            "c",
            1,
            DEFAULT_NODE_CAP,
            false,
        );
        let placed = layout(&ego, 80, 24);
        let centre = find(&placed, "c");
        let n = find(&placed, "n");
        assert_eq!(n.x, centre.x, "single ring node is horizontally aligned");
        assert!(n.y < centre.y, "and sits above the centre");
    }

    #[test]
    fn zero_viewport_yields_no_placement() {
        let ego = EgoSubgraph::build(
            &Adjacency::build(&[rel("c", "n", 5)]),
            "c",
            1,
            DEFAULT_NODE_CAP,
            false,
        );
        assert!(layout(&ego, 0, 24).is_empty());
        assert!(layout(&ego, 80, 0).is_empty());
    }
}
