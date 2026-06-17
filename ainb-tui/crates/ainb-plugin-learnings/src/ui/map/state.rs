//! Interaction state for the radial map: centre / hop / expand / selection plus
//! the recentre animation. The map is stateless about the *records* — the shell
//! owns the data and hands [`MapState`] a pre-built [`Adjacency`] whenever it
//! needs to navigate, hit-test, or paint.
//!
//! **Per-recentre caching.** The ego subgraph + its radial layout are identical
//! across an entire recentre animation (only `scale` changes, applied AFTER
//! layout) and across every keystroke / click that doesn't change the
//! centre/hop/expand. Recomputing them every frame is pure repeated work — the
//! 6-frame grow animation alone would rebuild the same ego + layout ~6×. So
//! [`MapState`] memoizes `(ego, layout)` keyed on
//! `(center, hop, expanded, width, height)` ([`EgoCache`]); a recentre /
//! `h` / `e` / data reload invalidates it, and the animation just re-lerps the
//! cached layout per frame. `navigate`, `hit_test`, and `recentre` all reuse the
//! same cache.

use std::cell::RefCell;

use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::Rect as RRect;

use super::ego::{Adjacency, DEFAULT_NODE_CAP, EgoSubgraph};
use super::layout::{Placed, layout};
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

/// Memoized ego subgraph + its radial layout, valid for one
/// `(center, hop, expanded, width, height)` tuple. Recomputed only when one of
/// those changes (recentre / hop / expand / data reload / viewport resize).
#[derive(Debug, Clone)]
struct EgoCache {
    center: String,
    hop: u8,
    expanded: bool,
    /// Canvas width the layout was computed for (layout depends on dimensions).
    width: u16,
    /// Canvas height the layout was computed for.
    height: u16,
    /// The extracted ego subgraph (O(degree) to build off the shared adjacency).
    ego: EgoSubgraph,
    /// The settled radial layout (scale 1.0); the animation lerps a COPY of this
    /// toward the centre per frame, never mutating the cache.
    layout: Vec<Placed>,
}

impl EgoCache {
    /// `true` if this cache is valid for the given key.
    fn matches(&self, center: &str, hop: u8, expanded: bool, width: u16, height: u16) -> bool {
        self.center == center
            && self.hop == hop
            && self.expanded == expanded
            && self.width == width
            && self.height == height
    }
}

/// Map interaction state.
#[derive(Debug)]
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
    /// Memoized ego + layout for the current key (interior-mutable so the
    /// immutable render / hit-test paths can populate it). Invalidated by a
    /// centre/hop/expand change or a fresh [`Adjacency`].
    cache: RefCell<Option<EgoCache>>,
}

impl Clone for MapState {
    fn clone(&self) -> Self {
        // The cache is a pure memo of the keyed fields — drop it on clone so the
        // clone rebuilds lazily (and `Clone` doesn't have to clone a `RefCell`
        // borrow). Cheap: a clone is only taken in tests.
        Self {
            center: self.center.clone(),
            hop: self.hop,
            expanded: self.expanded,
            selected: self.selected.clone(),
            anim_left: self.anim_left,
            cache: RefCell::new(None),
        }
    }
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
            cache: RefCell::new(None),
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

    /// Invalidate the memoized ego + layout. Called whenever the centre / hop /
    /// expand changes (so the next `with_ego` rebuilds for the new key).
    fn invalidate_cache(&self) {
        *self.cache.borrow_mut() = None;
    }

    /// Build the ego subgraph for the current centre / hop / expand state from
    /// the shared adjacency. Cheap — O(degree(center)) per ring — but prefer
    /// [`Self::with_ego`] / [`Self::with_layout`] which also memoize the layout.
    #[must_use]
    pub fn subgraph(&self, adj: &Adjacency) -> EgoSubgraph {
        EgoSubgraph::build(adj, &self.center, self.hop, DEFAULT_NODE_CAP, self.expanded)
    }

    /// Ensure the cache holds the ego + settled layout for the current key
    /// (rebuilding only on a key miss), then run `f` against the cached ego.
    /// Used by navigation / recentre / hit-test so they all share one build.
    fn with_ego<R>(
        &self,
        adj: &Adjacency,
        width: u16,
        height: u16,
        f: impl FnOnce(&EgoSubgraph) -> R,
    ) -> R {
        self.ensure_cache(adj, width, height);
        let cache = self.cache.borrow();
        f(&cache.as_ref().expect("cache populated by ensure_cache").ego)
    }

    /// Like [`Self::with_ego`] but also exposes the cached settled layout.
    fn with_layout<R>(
        &self,
        adj: &Adjacency,
        width: u16,
        height: u16,
        f: impl FnOnce(&EgoSubgraph, &[Placed]) -> R,
    ) -> R {
        self.ensure_cache(adj, width, height);
        let cache = self.cache.borrow();
        let c = cache.as_ref().expect("cache populated by ensure_cache");
        f(&c.ego, &c.layout)
    }

    /// Populate / refresh the cache for the current `(center, hop, expanded,
    /// width, height)` key if it's stale. Builds the ego off the shared
    /// adjacency (O(degree)) and the radial layout once; subsequent calls within
    /// the same animation / keystroke reuse it.
    fn ensure_cache(&self, adj: &Adjacency, width: u16, height: u16) {
        let fresh = self
            .cache
            .borrow()
            .as_ref()
            .is_some_and(|c| c.matches(&self.center, self.hop, self.expanded, width, height));
        if fresh {
            return;
        }
        let ego = EgoSubgraph::build(adj, &self.center, self.hop, DEFAULT_NODE_CAP, self.expanded);
        let layout = layout(&ego, width, height);
        *self.cache.borrow_mut() = Some(EgoCache {
            center: self.center.clone(),
            hop: self.hop,
            expanded: self.expanded,
            width,
            height,
            ego,
            layout,
        });
    }

    /// Move the selection. `←→` orbit within the current ring (wrapping); `↑↓`
    /// step to the inner / outer ring (clamped), keeping the nearest slot index.
    /// Returns `true` if the selection changed. Uses the cached ego for `width ×
    /// height` so navigation during a recentre animation re-extracts nothing.
    pub fn navigate(&mut self, adj: &Adjacency, width: u16, height: u16, dir: Nav) -> bool {
        // The selected node must exist in the current ego; if a prior toggle
        // stranded it, re-anchor to the centre BEFORE navigating so `←→` work.
        self.reconcile_selection(adj, width, height);
        let next = self.with_ego(adj, width, height, |ego| {
            let rings = rings_of(ego);
            if rings.is_empty() {
                return None;
            }
            // `selected` is guaranteed present (reconcile_selection above), so a
            // failed locate would be a logic error — fall back to the centre row.
            let (cur_ring, cur_idx) = locate(&rings, &self.selected).unwrap_or((0, 0));

            let (ring, idx) = match dir {
                Nav::Left | Nav::Right => {
                    let row = &rings[cur_ring];
                    let len = row.len();
                    if len <= 1 {
                        return None;
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
                        return None;
                    }
                    let ring = cur_ring - 1;
                    (ring, cur_idx.min(rings[ring].len().saturating_sub(1)))
                }
                Nav::Down => {
                    if cur_ring + 1 >= rings.len() {
                        return None;
                    }
                    let ring = cur_ring + 1;
                    (ring, cur_idx.min(rings[ring].len().saturating_sub(1)))
                }
            };
            Some(rings[ring][idx].clone())
        });
        match next {
            Some(name) if name != self.selected => {
                self.selected = name;
                true
            }
            _ => false,
        }
    }

    /// Recentre the map on the selected node (the `⏎` action). A real ring node
    /// becomes the new centre and the grow animation starts. Selecting the
    /// overflow node expands it instead; recentring on the current centre is a
    /// no-op. Returns `true` if anything changed.
    pub fn recentre_selected(&mut self, adj: &Adjacency, width: u16, height: u16) -> bool {
        let selected_is_overflow = self.with_ego(adj, width, height, |ego| {
            ego.nodes.iter().any(|n| n.name == self.selected && n.overflow)
        });
        if selected_is_overflow {
            return self.toggle_expand(adj);
        }
        if self.selected == self.center {
            return false;
        }
        self.center = self.selected.clone();
        self.hop = 1;
        self.expanded = false;
        self.anim_left = ANIM_FRAMES;
        // The key (center/hop/expanded) changed → drop the stale memo.
        self.invalidate_cache();
        true
    }

    /// Toggle hop depth 1 ↔ 2 (the `h` action). Re-anchors the selection if the
    /// new hop dropped the selected node out of the ego (e.g. `h` back from hop 2
    /// strands a ring-2 selection). Returns `true` (always changes the hop).
    pub fn toggle_hop(&mut self, adj: &Adjacency) -> bool {
        self.hop = if self.hop >= 2 { 1 } else { 2 };
        self.invalidate_cache();
        self.reanchor_if_selection_lost(adj);
        true
    }

    /// Toggle the overflow expansion (the `e` action). Re-anchors the selection
    /// if collapsing the overflow (`e` from expanded) dropped the selected
    /// neighbour out of the ego. Returns `true` (always changes the expansion).
    pub fn toggle_expand(&mut self, adj: &Adjacency) -> bool {
        self.expanded = !self.expanded;
        self.invalidate_cache();
        self.reanchor_if_selection_lost(adj);
        true
    }

    /// If `self.selected` is no longer a node in the (rebuilt) ego, re-anchor it
    /// to the centre — the centre always exists, so this matches the `new()`
    /// invariant and keeps navigation / recentre / the green highlight pointed at
    /// a visible node. Otherwise a previously-selected node that a hop/expand
    /// toggle removed would leave an off-screen "selection": dead `←→`, `⏎`
    /// recentring on an invisible node, and a vanished highlight.
    ///
    /// Uses [`Self::subgraph`] (node set only — no layout / dimensions needed) so
    /// it can run from the toggle handlers without a canvas rect.
    fn reanchor_if_selection_lost(&mut self, adj: &Adjacency) {
        let present = self.subgraph(adj).nodes.iter().any(|n| n.name == self.selected);
        if !present {
            self.selected = self.center.clone();
        }
    }

    /// Cache-aware variant of [`Self::reanchor_if_selection_lost`] used on the
    /// navigation path (it already needs the cached ego for the dimensions).
    fn reconcile_selection(&mut self, adj: &Adjacency, width: u16, height: u16) {
        let present = self.with_ego(adj, width, height, |ego| {
            ego.nodes.iter().any(|n| n.name == self.selected)
        });
        if !present {
            self.selected = self.center.clone();
        }
    }

    /// Resolve a click at absolute `(col, row)` within `area` against the map.
    /// A node box selects; a real ring node also recentres (animated). Returns
    /// what happened so the shell can repaint. Hit-tests against the cached
    /// layout for `area`'s canvas.
    pub fn handle_click(&mut self, adj: &Adjacency, area: RRect, col: u16, row: u16) -> Click {
        let [_, canvas, _] = render::split_area(area);
        let Some(name) = self.with_layout(adj, canvas.width, canvas.height, |_, placed| {
            render::hit_test_in(canvas, placed, col, row)
        }) else {
            return Click::Miss;
        };
        self.selected = name;
        if self.recentre_selected(adj, canvas.width, canvas.height) {
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

    /// Paint the map for the current state into `area` from the shared
    /// adjacency. Reuses the cached ego + settled layout (only `scale` varies per
    /// frame), so the 6-frame recentre animation re-extracts NOTHING — it just
    /// re-lerps the cached layout. Pure paint — the animation is advanced
    /// separately via [`Self::tick`] (driven by the plugin's `&mut self` render
    /// so this stays callable from an immutable render path).
    pub fn render_view(&self, buf: &mut RBuffer, area: RRect, adj: &Adjacency) {
        let [_, canvas, _] = render::split_area(area);
        let scale = self.anim_scale();
        let selected = self.selected.clone();
        self.with_layout(adj, canvas.width, canvas.height, |ego, placed| {
            render::render_cached(buf, area, ego, placed, &selected, scale);
        });
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
    use crate::data::Relationship;

    /// Canvas dims the tests key the cache on — matches the historical baseline.
    const W: u16 = 80;
    const H: u16 = 24;

    fn rel(source: &str, target: &str, rel_type: &str) -> Relationship {
        Relationship {
            source: source.into(),
            target: target.into(),
            rel_type: rel_type.into(),
            description: String::new(),
            strength: Some(5),
        }
    }

    fn fixture() -> Adjacency {
        Adjacency::build(&[
            rel("audit-after-rebase", "stale plan execution", "solves"),
            rel("stale plan execution", "git pull --rebase", "caused_by"),
            rel("audit-after-rebase", "checkpoint", "requires"),
        ])
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
        let adj = fixture();
        // Two ring-1 neighbours (stale plan execution, checkpoint). Start at
        // centre; ↓ into the ring, then ←→ orbits between the two.
        assert!(m.navigate(&adj, W, H, Nav::Down));
        let first = m.selected().to_string();
        assert_ne!(first, "audit-after-rebase");
        assert!(m.navigate(&adj, W, H, Nav::Right));
        assert_ne!(m.selected(), first);
        assert!(m.navigate(&adj, W, H, Nav::Right)); // wraps back
        assert_eq!(m.selected(), first);
    }

    #[test]
    fn up_down_cross_rings() {
        let mut m = MapState::new("audit-after-rebase");
        let adj = fixture();
        assert_eq!(m.selected(), "audit-after-rebase"); // ring 0
        assert!(m.navigate(&adj, W, H, Nav::Down)); // → ring 1
        assert_ne!(m.selected(), "audit-after-rebase");
        assert!(m.navigate(&adj, W, H, Nav::Up)); // back to centre
        assert_eq!(m.selected(), "audit-after-rebase");
        // Up at the centre is a clamped no-op.
        assert!(!m.navigate(&adj, W, H, Nav::Up));
    }

    #[test]
    fn recentre_changes_center_and_starts_animation() {
        let mut m = MapState::new("audit-after-rebase");
        let adj = fixture();
        m.navigate(&adj, W, H, Nav::Down); // select a ring neighbour
        let target = m.selected().to_string();
        assert!(m.recentre_selected(&adj, W, H));
        assert_eq!(m.center(), target, "centre moved to the selected node");
        assert!(m.wants_redraw(), "recentre kicks the grow animation");
        assert!(m.anim_scale() < 1.0, "mid-animation scale is below settled");
    }

    #[test]
    fn recentre_on_centre_is_a_noop() {
        let mut m = MapState::new("audit-after-rebase");
        let adj = fixture();
        assert!(!m.recentre_selected(&adj, W, H));
        assert!(!m.wants_redraw());
    }

    #[test]
    fn toggle_hop_flips_between_one_and_two() {
        let mut m = MapState::new("audit-after-rebase");
        let adj = fixture();
        // hop 1 → only the direct ring; hop 2 → reaches git pull --rebase.
        let n1 = m.subgraph(&adj).nodes.len();
        m.toggle_hop(&adj);
        let n2 = m.subgraph(&adj).nodes.len();
        assert!(n2 > n1, "hop 2 pulls in more nodes ({n1} → {n2})");
        m.toggle_hop(&adj);
        assert_eq!(m.subgraph(&adj).nodes.len(), n1, "toggles back to hop 1");
    }

    #[test]
    fn expand_reveals_capped_neighbours() {
        // A hub with more neighbours than the cap.
        let rels: Vec<Relationship> =
            (0..20).map(|i| rel("hub", &format!("n{i:02}"), "solves")).collect();
        let adj = Adjacency::build(&rels);
        let mut m = MapState::new("hub");
        let capped = m.subgraph(&adj);
        assert!(
            capped.nodes.iter().any(|n| n.overflow),
            "capped → overflow node"
        );
        m.toggle_expand(&adj);
        let expanded = m.subgraph(&adj);
        assert!(
            !expanded.nodes.iter().any(|n| n.overflow),
            "expand → no overflow"
        );
        assert!(expanded.nodes.len() > capped.nodes.len());
    }

    #[test]
    fn click_on_a_ring_node_recentres() {
        let mut m = MapState::new("audit-after-rebase");
        let adj = fixture();
        let area = RRect::new(0, 0, 80, 24);
        // Find a ring-1 node's painted box and click it.
        let ego = m.subgraph(&adj);
        let (canvas, placed) = render::layout_in(area, &ego);
        let ring1 = placed.iter().find(|p| p.ring == 1).unwrap();
        let (bx, by, _len) = render::box_rect(canvas, ring1);
        let outcome = m.handle_click(&adj, area, bx, by);
        assert_eq!(outcome, Click::Recentred);
        assert_eq!(m.center(), ring1.name);
    }

    #[test]
    fn click_on_empty_space_is_a_miss() {
        let mut m = MapState::new("audit-after-rebase");
        let adj = fixture();
        let area = RRect::new(0, 0, 80, 24);
        assert_eq!(m.handle_click(&adj, area, 0, 0), Click::Miss);
        assert_eq!(m.center(), "audit-after-rebase");
    }

    #[test]
    fn animation_settles_after_frames() {
        let mut m = MapState::new("audit-after-rebase");
        let adj = fixture();
        m.navigate(&adj, W, H, Nav::Down);
        m.recentre_selected(&adj, W, H);
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

    // ---- Task 2: re-clamp the selection after `e` / `h` toggles ----

    #[test]
    fn e_collapse_reanchors_a_stranded_selection_to_centre() {
        // A hub with overflow: expand it (`e`), select a neighbour that only
        // exists while expanded, then collapse (`e`). The collapse drops that
        // neighbour from the ego — the selection must re-anchor to the centre,
        // not strand off-screen.
        let rels: Vec<Relationship> =
            (0..20).map(|i| rel("hub", &format!("n{i:02}"), "solves")).collect();
        let adj = Adjacency::build(&rels);
        let mut m = MapState::new("hub");

        // Expand so every neighbour is a real node.
        m.toggle_expand(&adj);
        // Select a neighbour that the cap would have folded into overflow
        // (`n19` sorts last; with cap 15 it's beyond the kept frontier).
        m.navigate(&adj, W, H, Nav::Down);
        // Walk to a late neighbour by orbiting; just confirm we left the centre.
        assert_ne!(m.selected(), "hub", "selection moved off the centre");
        // Force-select a neighbour only present while expanded.
        m.selected = "n19".to_string();
        assert!(
            m.subgraph(&adj).nodes.iter().any(|n| n.name == "n19"),
            "precondition: n19 is present while expanded"
        );

        // Collapse: n19 is now folded into the overflow and gone from the ego.
        m.toggle_expand(&adj);
        assert!(
            !m.subgraph(&adj).nodes.iter().any(|n| n.name == "n19"),
            "precondition: n19 dropped out of the collapsed ego"
        );
        // The selection must have re-anchored to the centre (the safe anchor).
        assert_eq!(
            m.selected(),
            "hub",
            "collapsing must re-anchor a stranded selection to the centre"
        );
        // …and navigation works again from the re-anchored selection.
        assert!(
            m.navigate(&adj, W, H, Nav::Down),
            "←→/↓ must work after re-anchor (selection is on a real node)"
        );
    }

    #[test]
    fn h_back_to_hop_one_reanchors_a_ring_two_selection() {
        // hop 2 reaches `git pull --rebase` (a ring-2 node). Select it, then
        // `h` back to hop 1 — the ring-2 node is gone, so the selection must
        // re-anchor to the centre and arrows must work again.
        let mut m = MapState::new("audit-after-rebase");
        let adj = fixture();
        m.toggle_hop(&adj); // hop 2
        m.selected = "git pull --rebase".to_string();
        assert!(
            m.subgraph(&adj).nodes.iter().any(|n| n.name == "git pull --rebase"),
            "precondition: ring-2 node present at hop 2"
        );

        m.toggle_hop(&adj); // back to hop 1
        assert!(
            !m.subgraph(&adj).nodes.iter().any(|n| n.name == "git pull --rebase"),
            "precondition: ring-2 node gone at hop 1"
        );
        assert_eq!(
            m.selected(),
            "audit-after-rebase",
            "hopping back must re-anchor the stranded ring-2 selection to the centre"
        );
        // ⏎ on the re-anchored centre is a clean no-op (centre == selected),
        // proving the selection points at a real, visible node — not a ghost.
        assert!(!m.recentre_selected(&adj, W, H));
        // And ↓ navigates into the ring again.
        assert!(m.navigate(&adj, W, H, Nav::Down));
        assert_ne!(m.selected(), "audit-after-rebase");
    }

    // ---- Task 3: the cached layout is byte-identical across animation frames ----

    #[test]
    fn cached_layout_is_byte_identical_across_animation_frames() {
        // The recentre animation must reuse ONE cached ego + settled layout and
        // only vary `scale`; the cache must not be rebuilt per frame. Render the
        // map across every animation frame and assert the cached settled layout
        // never changes (byte-identical), proving the per-frame work is just the
        // scale lerp, not a re-layout.
        let mut m = MapState::new("audit-after-rebase");
        let adj = fixture();
        m.navigate(&adj, W, H, Nav::Down);
        m.recentre_selected(&adj, W, H); // kicks the animation; invalidates cache

        let area = RRect::new(0, 0, W, H);
        let mut buf = RBuffer::empty(area);
        // First paint populates the cache.
        m.render_view(&mut buf, area, &adj);
        let baseline = m.cache.borrow().as_ref().unwrap().layout.clone();

        let mut frames = 0;
        while m.wants_redraw() {
            m.tick();
            let mut buf = RBuffer::empty(area);
            m.render_view(&mut buf, area, &adj);
            let now = m.cache.borrow().as_ref().unwrap().layout.clone();
            assert_eq!(
                now, baseline,
                "the cached settled layout must be byte-identical every frame \
                 (only scale varies)"
            );
            frames += 1;
            assert!(frames < 100, "animation must terminate");
        }
        assert!(
            frames > 0,
            "the recentre animation must run at least one frame"
        );
    }
}
