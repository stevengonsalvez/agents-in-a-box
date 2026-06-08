//! Interaction state for the radial map: centre / hop / expand / selection plus
//! the recentre animation. The map is stateless about the data — the shell owns
//! the records and asks [`MapState`] to build the [`EgoSubgraph`] for the
//! current centre/hop/expand whenever it needs to navigate, hit-test, or paint.

use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::Rect as RRect;

use crate::data::Relationship;

use super::ego::{DEFAULT_NODE_CAP, EgoSubgraph};
use super::render;

/// Frames the recentre "grow" animation runs for. At the host's ~33 ms render
/// cadence this is a ~0.2 s transition.
const ANIM_FRAMES: u8 = 6;

/// Direction for keyboard selection movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    /// `←` — previous node within the current ring (wraps).
    Left,
    /// `→` — next node within the current ring (wraps).
    Right,
    /// `↑` — inner ring (toward the centre).
    Up,
    /// `↓` — outer ring (away from the centre).
    Down,
}

/// What a mouse click resolved to, so the shell knows whether to repaint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Click hit no node box — nothing changed.
    Miss,
    /// Click selected a node (already the centre, or the overflow node) — the
    /// selection moved but the centre did not.
    Selected,
    /// Click recentred the map on a ring node — the centre token changed.
    Recentred,
}

/// Map interaction state.
#[derive(Debug, Clone)]
pub struct MapState {
    center: String,
    /// Hop depth: 1 (default) or 2.
    hop: u8,
    /// Whether the `[+N more]` overflow is expanded to show every neighbour.
    expanded: bool,
    /// Name of the selected node (centre, a ring node, or the overflow node).
    selected: String,
    /// Recentre-animation frames remaining (`0` = settled).
    anim_left: u8,
}

impl MapState {
    /// Start a map centred on `center`, with that node selected.
    #[must_use]
    pub fn new(center: impl Into<String>) -> Self {
        let center = center.into();
        Self {
            selected: center.clone(),
            center,
            hop: 1,
            expanded: false,
            anim_left: 0,
        }
    }

    /// The current centre entity name.
    #[must_use]
    pub fn center(&self) -> &str {
        &self.center
    }

    /// The selected node name.
    #[must_use]
    pub fn selected(&self) -> &str {
        &self.selected
    }

    /// Build the ego subgraph for the current centre / hop / expand state.
    #[must_use]
    pub fn subgraph(&self, rels: &[Relationship]) -> EgoSubgraph {
        EgoSubgraph::build(
            rels,
            &self.center,
            self.hop,
            DEFAULT_NODE_CAP,
            self.expanded,
        )
    }

    /// Move the selection within `ego`. `←→` orbit within the current ring
    /// (wrapping); `↑↓` step to the inner / outer ring (clamped), keeping the
    /// nearest slot index. Returns `true` if the selection changed.
    pub fn navigate(&mut self, ego: &EgoSubgraph, dir: Nav) -> bool {
        let rings = rings_of(ego);
        if rings.is_empty() {
            return false;
        }
        let (cur_ring, cur_idx) = locate(&rings, &self.selected).unwrap_or((0, 0));

        let (ring, idx) = match dir {
            Nav::Left | Nav::Right => {
                let row = &rings[cur_ring];
                let len = row.len();
                if len <= 1 {
                    return false;
                }
                let idx = if matches!(dir, Nav::Right) {
                    (cur_idx + 1) % len
                } else {
                    (cur_idx + len - 1) % len
                };
                (cur_ring, idx)
            }
            Nav::Up => {
                if cur_ring == 0 {
                    return false;
                }
                let ring = cur_ring - 1;
                (ring, cur_idx.min(rings[ring].len().saturating_sub(1)))
            }
            Nav::Down => {
                if cur_ring + 1 >= rings.len() {
                    return false;
                }
                let ring = cur_ring + 1;
                (ring, cur_idx.min(rings[ring].len().saturating_sub(1)))
            }
        };

        let next = rings[ring][idx].clone();
        if next == self.selected {
            return false;
        }
        self.selected = next;
        true
    }

    /// Recentre the map on the selected node (the `⏎` action). A real ring node
    /// becomes the new centre and the grow animation starts. Selecting the
    /// overflow node expands it instead; recentring on the current centre is a
    /// no-op. Returns `true` if anything changed.
    pub fn recentre_selected(&mut self, ego: &EgoSubgraph) -> bool {
        if let Some(node) = ego.nodes.iter().find(|n| n.name == self.selected) {
            if node.overflow {
                return self.toggle_expand();
            }
        }
        if self.selected == self.center {
            return false;
        }
        self.center = self.selected.clone();
        self.hop = 1;
        self.expanded = false;
        self.anim_left = ANIM_FRAMES;
        true
    }

    /// Toggle hop depth 1 ↔ 2 (the `h` action). Returns `true` (always changes).
    pub fn toggle_hop(&mut self) -> bool {
        self.hop = if self.hop >= 2 { 1 } else { 2 };
        true
    }

    /// Toggle the overflow expansion (the `e` action).
    pub fn toggle_expand(&mut self) -> bool {
        self.expanded = !self.expanded;
        true
    }

    /// Resolve a click at absolute `(col, row)` within `area` against the map.
    /// A node box selects; a real ring node also recentres (animated). Returns
    /// what happened so the shell can repaint.
    pub fn handle_click(&mut self, ego: &EgoSubgraph, area: RRect, col: u16, row: u16) -> Click {
        let Some(name) = render::hit_test(area, ego, col, row) else {
            return Click::Miss;
        };
        self.selected = name;
        if self.recentre_selected(ego) {
            Click::Recentred
        } else {
            Click::Selected
        }
    }

    /// Advance the recentre animation by one frame. Returns `true` while frames
    /// remain (so the caller keeps requesting redraws).
    pub fn tick(&mut self) -> bool {
        if self.anim_left > 0 {
            self.anim_left -= 1;
        }
        self.anim_left > 0
    }

    /// Whether the map wants another render frame without input (animation).
    #[must_use]
    pub fn wants_redraw(&self) -> bool {
        self.anim_left > 0
    }

    /// The current animation scale (`1.0` settled; ramps up while animating) for
    /// [`render::render`].
    #[must_use]
    pub fn anim_scale(&self) -> f64 {
        if self.anim_left == 0 {
            return 1.0;
        }
        // frames done so far → 1..=ANIM_FRAMES mapped to a growing scale.
        let done = f64::from(ANIM_FRAMES - self.anim_left) + 1.0;
        (0.35 + 0.65 * (done / f64::from(ANIM_FRAMES))).min(1.0)
    }

    /// Paint the map for the current state into `area`, building the subgraph
    /// from `rels`. Pure paint — the animation is advanced separately via
    /// [`Self::tick`] (driven by the plugin's `&mut self` render so this stays
    /// callable from an immutable render path).
    pub fn render_view(&self, buf: &mut RBuffer, area: RRect, rels: &[Relationship]) {
        let ego = self.subgraph(rels);
        render::render(buf, area, &ego, &self.selected, self.anim_scale());
    }
}

/// Node names grouped by ring index (`rings[0]` = the centre row).
fn rings_of(ego: &EgoSubgraph) -> Vec<Vec<String>> {
    let max_ring = ego.nodes.iter().map(|n| n.ring).max().unwrap_or(0);
    let mut rings: Vec<Vec<String>> = vec![Vec::new(); usize::from(max_ring) + 1];
    for n in &ego.nodes {
        rings[usize::from(n.ring)].push(n.name.clone());
    }
    rings
}

/// Find `(ring, index)` of `name` within the grouped rings.
fn locate(rings: &[Vec<String>], name: &str) -> Option<(usize, usize)> {
    for (r, row) in rings.iter().enumerate() {
        if let Some(i) = row.iter().position(|n| n == name) {
            return Some((r, i));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(source: &str, target: &str, rel_type: &str) -> Relationship {
        Relationship {
            source: source.into(),
            target: target.into(),
            rel_type: rel_type.into(),
            description: String::new(),
            strength: Some(5),
        }
    }

    fn fixture() -> Vec<Relationship> {
        vec![
            rel("audit-after-rebase", "stale plan execution", "solves"),
            rel("stale plan execution", "git pull --rebase", "caused_by"),
            rel("audit-after-rebase", "checkpoint", "requires"),
        ]
    }

    #[test]
    fn new_centres_and_selects_the_entity() {
        let m = MapState::new("audit-after-rebase");
        assert_eq!(m.center(), "audit-after-rebase");
        assert_eq!(m.selected(), "audit-after-rebase");
    }

    #[test]
    fn left_right_orbit_within_ring() {
        let mut m = MapState::new("audit-after-rebase");
        let ego = m.subgraph(&fixture());
        // Two ring-1 neighbours (stale plan execution, checkpoint). Start at
        // centre; ↓ into the ring, then ←→ orbits between the two.
        assert!(m.navigate(&ego, Nav::Down));
        let first = m.selected().to_string();
        assert_ne!(first, "audit-after-rebase");
        assert!(m.navigate(&ego, Nav::Right));
        assert_ne!(m.selected(), first);
        assert!(m.navigate(&ego, Nav::Right)); // wraps back
        assert_eq!(m.selected(), first);
    }

    #[test]
    fn up_down_cross_rings() {
        let mut m = MapState::new("audit-after-rebase");
        let ego = m.subgraph(&fixture());
        assert_eq!(m.selected(), "audit-after-rebase"); // ring 0
        assert!(m.navigate(&ego, Nav::Down)); // → ring 1
        assert_ne!(m.selected(), "audit-after-rebase");
        assert!(m.navigate(&ego, Nav::Up)); // back to centre
        assert_eq!(m.selected(), "audit-after-rebase");
        // Up at the centre is a clamped no-op.
        assert!(!m.navigate(&ego, Nav::Up));
    }

    #[test]
    fn recentre_changes_center_and_starts_animation() {
        let mut m = MapState::new("audit-after-rebase");
        let ego = m.subgraph(&fixture());
        m.navigate(&ego, Nav::Down); // select a ring neighbour
        let target = m.selected().to_string();
        assert!(m.recentre_selected(&ego));
        assert_eq!(m.center(), target, "centre moved to the selected node");
        assert!(m.wants_redraw(), "recentre kicks the grow animation");
        assert!(m.anim_scale() < 1.0, "mid-animation scale is below settled");
    }

    #[test]
    fn recentre_on_centre_is_a_noop() {
        let mut m = MapState::new("audit-after-rebase");
        let ego = m.subgraph(&fixture());
        assert!(!m.recentre_selected(&ego));
        assert!(!m.wants_redraw());
    }

    #[test]
    fn toggle_hop_flips_between_one_and_two() {
        let mut m = MapState::new("audit-after-rebase");
        // hop 1 → only the direct ring; hop 2 → reaches git pull --rebase.
        let n1 = m.subgraph(&fixture()).nodes.len();
        m.toggle_hop();
        let n2 = m.subgraph(&fixture()).nodes.len();
        assert!(n2 > n1, "hop 2 pulls in more nodes ({n1} → {n2})");
        m.toggle_hop();
        assert_eq!(
            m.subgraph(&fixture()).nodes.len(),
            n1,
            "toggles back to hop 1"
        );
    }

    #[test]
    fn expand_reveals_capped_neighbours() {
        // A hub with more neighbours than the cap.
        let rels: Vec<Relationship> =
            (0..20).map(|i| rel("hub", &format!("n{i:02}"), "solves")).collect();
        let mut m = MapState::new("hub");
        let capped = m.subgraph(&rels);
        assert!(
            capped.nodes.iter().any(|n| n.overflow),
            "capped → overflow node"
        );
        m.toggle_expand();
        let expanded = m.subgraph(&rels);
        assert!(
            !expanded.nodes.iter().any(|n| n.overflow),
            "expand → no overflow"
        );
        assert!(expanded.nodes.len() > capped.nodes.len());
    }

    #[test]
    fn click_on_a_ring_node_recentres() {
        let mut m = MapState::new("audit-after-rebase");
        let ego = m.subgraph(&fixture());
        let area = RRect::new(0, 0, 80, 24);
        // Find a ring-1 node's painted box and click it.
        let (canvas, placed) = render::layout_in(area, &ego);
        let ring1 = placed.iter().find(|p| p.ring == 1).unwrap();
        let (bx, by, _len) = render::box_rect(canvas, ring1);
        let outcome = m.handle_click(&ego, area, bx, by);
        assert_eq!(outcome, Click::Recentred);
        assert_eq!(m.center(), ring1.name);
    }

    #[test]
    fn click_on_empty_space_is_a_miss() {
        let mut m = MapState::new("audit-after-rebase");
        let ego = m.subgraph(&fixture());
        let area = RRect::new(0, 0, 80, 24);
        assert_eq!(m.handle_click(&ego, area, 0, 0), Click::Miss);
        assert_eq!(m.center(), "audit-after-rebase");
    }

    #[test]
    fn animation_settles_after_frames() {
        let mut m = MapState::new("audit-after-rebase");
        let ego = m.subgraph(&fixture());
        m.navigate(&ego, Nav::Down);
        m.recentre_selected(&ego);
        let mut guard = 0;
        while m.wants_redraw() {
            m.tick();
            guard += 1;
            assert!(guard < 100, "animation must terminate");
        }
        assert!(
            (m.anim_scale() - 1.0).abs() < f64::EPSILON,
            "settles at scale 1.0"
        );
    }
}
