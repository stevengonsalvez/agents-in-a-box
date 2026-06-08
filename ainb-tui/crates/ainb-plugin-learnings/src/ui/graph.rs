//! Graph tab — entity neighborhood + community clusters (P8).
//!
//! Two toggleable views over the knowledge base's graph structure:
//!
//! - **Entity neighborhood** (default): pick an entity from the left list and
//!   see its TYPED relationship edges on the right —
//!   `<entity> --<rel_type>--> <other>`. The edge types
//!   (`solves` / `caused_by` / `causes` / `relates_to`) are sourced from the
//!   AGGREGATED record relationships, NOT the graphml.
//! - **Community clusters** (`c` toggles): the community reports parsed from
//!   `kv_store_community_reports.json` — one row per cluster (`title` + member
//!   count), the nano_graphrag community view.
//!
//! **Why aggregate record relationships, not the graphml edges?** (P4 field
//! finding, load-bearing.) The live nano_graphrag graphml carries UNTYPED edges
//! — only a free-text `description` and a numeric `weight`; the relationship
//! *type* lives in the per-record `.entities.yaml` sidecars. (The committed
//! test fixture's graphml happens to add a `rel_type` key, but the neighborhood
//! deliberately does NOT read graphml edges at all — it reads the record
//! `relationships[]`. So the fixture's extra key is irrelevant to this path.)
//! The neighborhood is built from the union of every scanned record's
//! `relationships[]`, keyed by entity name. Those are typed AND connect entities
//! across learnings via shared entity names. The graphml may supplement the node
//! set in a later phase, but it is NOT the edge-type source.
//!
//! Navigation: `↑↓` / `jk` move the selection within the active view; `c`
//! toggles entity ⇄ community. Focus is entered from the shell with `g`; the
//! shell releases focus on `Backspace` (Esc is host-reserved). All width math
//! goes through `chars()` — never byte slicing — so multibyte entity / community
//! text can't panic on a char boundary (the Rust UTF-8 truncate trap).

use std::cell::Cell;
use std::collections::BTreeMap;

use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect as RRect};
use ratatui::style::{Modifier as RModifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use ainb_plugin_sdk::KeyCode;

use super::map::state::{Click, MapState, Nav};
use super::{CORNFLOWER_BLUE, GOLD, LIST_HIGHLIGHT_BG, MUTED_GRAY, SELECTION_GREEN, SOFT_WHITE};
use crate::data::{Community, DEFAULT_REL_TYPE, LearningRecord, Relationship};

/// Which graph view is active. `c` toggles Entities ⇄ Communities; `v` cycles
/// the radial Map in and out (returning to Entities).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum View {
    /// Entity neighborhood: entities + their typed edges (default).
    #[default]
    Entities,
    /// Community clusters from the community-reports json.
    Communities,
    /// Radial ego local-graph map (the `v` sub-mode).
    Map,
}

/// A typed outgoing edge from an entity to a neighbor, sourced from an
/// aggregated record relationship.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Edge {
    /// Relationship type (`solves` / `caused_by` / `causes` / `relates_to`).
    rel_type: String,
    /// Neighbor entity name the edge points at.
    target: String,
}

/// One entity node in the neighborhood view: its name plus the typed edges that
/// originate from it (aggregated across every record that mentions it as a
/// relationship `source`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    /// Entity name (the adjacency key).
    name: String,
    /// Typed outgoing edges, sorted for a deterministic render.
    edges: Vec<Edge>,
}

/// Graph-tab view state: the built adjacency + community list + selection +
/// active view + focus flag.
#[derive(Debug, Default)]
pub struct GraphState {
    /// Active view (entity neighborhood vs community clusters).
    view: View,
    /// Entity nodes with their typed edges, sorted by name (the left list).
    nodes: Vec<Node>,
    /// Community clusters from the community-reports json.
    communities: Vec<Community>,
    /// Selected row index into the active view's list.
    selected: usize,
    /// `true` once `g` has focused the graph (so `↑↓`/`c` route here). The shell
    /// releases this on `Backspace`.
    focused: bool,
    /// Flattened typed relationships across every record — the source the radial
    /// map builds its ego subgraph from (the text views use `nodes` instead).
    relationships: Vec<Relationship>,
    /// Radial map interaction state — `Some` only while the `Map` view is active.
    map: Option<MapState>,
    /// Absolute (plugin-viewport) rect the map last painted into, stashed during
    /// render so a forwarded mouse click can hit-test against the exact geometry.
    /// `Cell` so the immutable render path can update it.
    last_map_area: Cell<Option<RRect>>,
}

impl GraphState {
    /// Rebuild the typed adjacency from the union of every record's
    /// `relationships[]`, keyed by entity name. Called by the shell whenever the
    /// records change (a fresh scan).
    ///
    /// Every relationship contributes one typed outgoing edge from its `source`
    /// entity to its `target`. Entities are sorted by name so the list — and the
    /// initial selection — is deterministic across runs (a tripwire can lock an
    /// exact first-entity token). A relationship with an empty `type` falls back
    /// to [`DEFAULT_REL_TYPE`] so it still renders a typed arrow.
    pub fn set_records(&mut self, records: &[LearningRecord]) {
        let mut by_source: BTreeMap<String, Vec<Edge>> = BTreeMap::new();
        for rec in records {
            for rel in &rec.relationships {
                let rel_type = if rel.rel_type.is_empty() {
                    DEFAULT_REL_TYPE.to_string()
                } else {
                    rel.rel_type.clone()
                };
                by_source.entry(rel.source.clone()).or_default().push(Edge {
                    rel_type,
                    target: rel.target.clone(),
                });
            }
        }
        self.nodes = by_source
            .into_iter()
            .map(|(name, mut edges)| {
                // Stable edge order within a node: by (rel_type, target).
                edges.sort_by(|a, b| (&a.rel_type, &a.target).cmp(&(&b.rel_type, &b.target)));
                Node { name, edges }
            })
            .collect();
        // Keep the raw typed relationships for the radial map's ego extraction
        // (both-direction adjacency + strength + arrows; the `nodes` list above
        // is outgoing-only).
        self.relationships = records.iter().flat_map(|r| r.relationships.iter().cloned()).collect();
        self.clamp_selection();
    }

    /// Load the parsed community clusters (from `kv_store_community_reports.json`)
    /// into the community view. Called by the shell after it reads the
    /// `graph_cache`.
    pub fn set_communities(&mut self, communities: Vec<Community>) {
        self.communities = communities;
        self.clamp_selection();
    }

    /// `true` while the graph holds focus (so the shell routes `↑↓`/`c` here and
    /// the help bar reflects the active affordances).
    #[must_use]
    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    /// Enter graph focus (the `g` action). Idempotent. Resets the selection to
    /// the top so re-entering lands on a known row.
    pub fn focus(&mut self) {
        self.focused = true;
        self.selected = 0;
    }

    /// Release graph focus (the shell's `Backspace` exit). Idempotent.
    pub fn blur(&mut self) {
        self.focused = false;
    }

    /// Route a key while the graph holds focus. Returns `true` when state
    /// changed (so the shell bumps its render generation).
    ///
    /// - `c` — toggle entity ⇄ community view (resets the selection).
    /// - `↑↓` / `jk` — move the selection within the active view.
    /// - everything else — unhandled (`false`); the shell does not forward it.
    pub fn handle_key(&mut self, code: &KeyCode) -> bool {
        // `v` cycles the radial Map in and out, from any graph view.
        if matches!(code, KeyCode::Char { ch: 'v' }) {
            return self.toggle_map();
        }
        // In Map mode the radial view owns navigation; text-view keys are inert.
        if self.view == View::Map {
            return self.handle_map_key(code);
        }
        match code {
            KeyCode::Char { ch: 'c' } => {
                self.toggle_view();
                true
            }
            KeyCode::Down | KeyCode::Char { ch: 'j' } => self.move_selection(1),
            KeyCode::Up | KeyCode::Char { ch: 'k' } => self.move_selection(-1),
            _ => false,
        }
    }

    /// Route a key while the Map view is active. `o` is intentionally NOT handled
    /// here — the shell owns it (it opens the learnings-behind-the-entity picker
    /// → Detail, which needs the record set).
    fn handle_map_key(&mut self, code: &KeyCode) -> bool {
        // In-plugin "back": leave the map, returning to the neighbourhood list
        // landed on whichever entity the map ended on (so navigating the map and
        // backing out feels continuous with the list).
        if matches!(code, KeyCode::Backspace) {
            if let Some(center) = self.map.as_ref().map(|m| m.center().to_string()) {
                if let Some(idx) = self.nodes.iter().position(|n| n.name == center) {
                    self.selected = idx;
                }
            }
            self.view = View::Entities;
            self.map = None;
            return true;
        }
        let rels = self.relationships.clone();
        let Some(map) = self.map.as_mut() else {
            return false;
        };
        match code {
            KeyCode::Char { ch: 'h' } => map.toggle_hop(),
            KeyCode::Char { ch: 'e' } => map.toggle_expand(),
            KeyCode::Enter => {
                let ego = map.subgraph(&rels);
                map.recentre_selected(&ego)
            }
            KeyCode::Up => nav(map, &rels, Nav::Up),
            KeyCode::Down => nav(map, &rels, Nav::Down),
            KeyCode::Left => nav(map, &rels, Nav::Left),
            KeyCode::Right => nav(map, &rels, Nav::Right),
            _ => false,
        }
    }

    /// Toggle the radial Map view. Entering centres on the entity selected in the
    /// list; leaving returns to the neighbourhood. No-op (returns `false`) if
    /// there's no entity to centre on.
    fn toggle_map(&mut self) -> bool {
        if self.view == View::Map {
            self.view = View::Entities;
            self.map = None;
            return true;
        }
        let Some(center) = self.nodes.get(self.selected).map(|n| n.name.clone()) else {
            return false;
        };
        self.map = Some(MapState::new(center));
        self.view = View::Map;
        true
    }

    /// `true` while the radial Map view is active.
    #[must_use]
    pub fn in_map(&self) -> bool {
        self.view == View::Map
    }

    /// The entity name selected in the Map (for the shell's `o` → Detail), or
    /// `None` if not in the map / the overflow node is selected.
    #[must_use]
    pub fn map_selected_entity(&self) -> Option<String> {
        let map = self.map.as_ref()?;
        let sel = map.selected();
        // Skip the synthetic `+N more` node — it isn't a real entity.
        let ego = map.subgraph(&self.relationships);
        let is_overflow = ego.nodes.iter().any(|n| n.name == sel && n.overflow);
        (!is_overflow).then(|| sel.to_string())
    }

    /// Resolve a forwarded mouse click (plugin-viewport coords) against the map.
    /// Returns `true` if the click changed the map (selection / recentre).
    pub fn handle_map_click(&mut self, col: u16, row: u16) -> bool {
        let Some(area) = self.last_map_area.get() else {
            return false;
        };
        let rels = self.relationships.clone();
        let Some(map) = self.map.as_mut() else {
            return false;
        };
        let ego = map.subgraph(&rels);
        !matches!(map.handle_click(&ego, area, col, row), Click::Miss)
    }

    /// Advance the map recentre animation one frame (driven by the plugin's
    /// `&mut self` render). Returns `true` while still animating.
    pub fn map_tick(&mut self) -> bool {
        self.map.as_mut().is_some_and(MapState::tick)
    }

    /// Whether the map wants another render frame without input (animation).
    #[must_use]
    pub fn map_wants_redraw(&self) -> bool {
        self.map.as_ref().is_some_and(MapState::wants_redraw)
    }

    /// Toggle between the entity neighborhood and the community view, resetting
    /// the selection to the top of the newly-active list.
    fn toggle_view(&mut self) {
        self.view = match self.view {
            View::Entities | View::Map => View::Communities,
            View::Communities => View::Entities,
        };
        self.selected = 0;
    }

    /// The number of rows in the active view's list. The Map view has no list
    /// selection (it owns its own selection via `MapState`), so it reports the
    /// entity count — `↑↓` there is intercepted by the map before this is used.
    fn active_len(&self) -> usize {
        match self.view {
            View::Entities | View::Map => self.nodes.len(),
            View::Communities => self.communities.len(),
        }
    }

    /// Move the selection by `delta`, clamped to the active list bounds. Returns
    /// `true` when the selection actually moved.
    fn move_selection(&mut self, delta: isize) -> bool {
        let len = self.active_len();
        if len == 0 {
            return false;
        }
        let last = len - 1;
        let next = (self.selected as isize + delta).clamp(0, last as isize) as usize;
        if next == self.selected {
            return false;
        }
        self.selected = next;
        true
    }

    /// Clamp the selection into the active list (after a data reload shrinks it).
    fn clamp_selection(&mut self) {
        let len = self.active_len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }
}

/// Render the Graph tab into `area`: a left list (entities OR communities) and a
/// right detail pane (the selected entity's typed edges, or the selected
/// community's summary), then a help bar.
pub fn render(buf: &mut RBuffer, area: RRect, state: &GraphState) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    // The Map view owns the entire Graph body (its own header + canvas + help
    // bar), so it bypasses the list/detail split + shared help bar below.
    if state.view == View::Map {
        // Stash the body rect (absolute, plugin-viewport coords) so a forwarded
        // mouse click hit-tests against exactly what was painted.
        state.last_map_area.set(Some(area));
        if let Some(map) = state.map.as_ref() {
            map.render_view(buf, area, &state.relationships);
        }
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // body (list + detail)
            Constraint::Length(1), // help bar
        ])
        .split(area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(rows[0]);

    match state.view {
        View::Entities | View::Map => {
            render_entity_list(buf, cols[0], state);
            render_entity_detail(buf, cols[1], state);
        }
        View::Communities => {
            render_community_list(buf, cols[0], state);
            render_community_detail(buf, cols[1], state);
        }
    }
    render_help_bar(buf, rows[1], state);
}

/// Render the left entity list with a `▶` on the selected row.
fn render_entity_list(buf: &mut RBuffer, area: RRect, state: &GraphState) {
    let block = panel("entities", state.focused);
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if state.nodes.is_empty() {
        Paragraph::new(empty_line("  (no entities in the graph)")).render(inner, buf);
        return;
    }

    let width = inner.width as usize;
    let lines: Vec<Line> = state
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| {
            let is_sel = i == state.selected;
            let marker = if is_sel { "▶ " } else { "  " };
            let style = row_style(is_sel);
            // marker is 2 cols; reserve them so the name truncation fits.
            let name = truncate_chars(&node.name, width.saturating_sub(2));
            Line::from(vec![Span::styled(marker, style), Span::styled(name, style)])
        })
        .collect();
    Paragraph::new(lines).render(inner, buf);
}

/// Render the right detail pane: the selected entity's TYPED outgoing edges as
/// `<entity> --<rel_type>--> <target>` lines. The `--<type>-->` arrow is the
/// token the tests/tripwire lock.
fn render_entity_detail(buf: &mut RBuffer, area: RRect, state: &GraphState) {
    let block = panel("neighborhood", false);
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let Some(node) = state.nodes.get(state.selected) else {
        Paragraph::new(empty_line("  (nothing selected)")).render(inner, buf);
        return;
    };

    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        node.name.clone(),
        Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
    ))];

    if node.edges.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no outgoing relationships)",
            Style::default().fg(MUTED_GRAY).add_modifier(RModifier::ITALIC),
        )));
    } else {
        for edge in &node.edges {
            // `<source> --<rel_type>--> <target>`. The whole `--<type>-->` arrow
            // is one contiguous span so it survives the cell-join intact (the
            // tripwire asserts the exact `--solves-->` token).
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default().fg(MUTED_GRAY)),
                Span::styled(node.name.clone(), Style::default().fg(SOFT_WHITE)),
                Span::styled(
                    format!(" --{}--> ", edge.rel_type),
                    Style::default().fg(SELECTION_GREEN),
                ),
                Span::styled(edge.target.clone(), Style::default().fg(SOFT_WHITE)),
            ]));
        }
    }

    Paragraph::new(lines).render(inner, buf);
}

/// Render the left community list with a `▶` on the selected row.
fn render_community_list(buf: &mut RBuffer, area: RRect, state: &GraphState) {
    let block = panel("communities", state.focused);
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if state.communities.is_empty() {
        Paragraph::new(empty_line("  (no communities)")).render(inner, buf);
        return;
    }

    let width = inner.width as usize;
    let lines: Vec<Line> = state
        .communities
        .iter()
        .enumerate()
        .map(|(i, comm)| {
            let is_sel = i == state.selected;
            let marker = if is_sel { "▶ " } else { "  " };
            let style = row_style(is_sel);
            let title = truncate_chars(&comm.title, width.saturating_sub(2));
            Line::from(vec![
                Span::styled(marker, style),
                Span::styled(title, style),
            ])
        })
        .collect();
    Paragraph::new(lines).render(inner, buf);
}

/// Render the right pane for the community view: the selected cluster's title +
/// summary + member-node count.
fn render_community_detail(buf: &mut RBuffer, area: RRect, state: &GraphState) {
    let block = panel("cluster", false);
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let Some(comm) = state.communities.get(state.selected) else {
        Paragraph::new(empty_line("  (nothing selected)")).render(inner, buf);
        return;
    };

    let lines = vec![
        Line::from(Span::styled(
            comm.title.clone(),
            Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
        )),
        Line::from(Span::styled(
            comm.summary.clone(),
            Style::default().fg(SOFT_WHITE),
        )),
        Line::from(Span::styled(
            format!("  {} members · rating {:.1}", comm.nodes.len(), comm.rating),
            Style::default().fg(MUTED_GRAY),
        )),
    ];
    Paragraph::new(lines).render(inner, buf);
}

/// Bottom help bar. `↑↓ move` + `c communities`/`c entities` (the toggle) +
/// `Bksp back` are live when focused; the bar stays honest about Esc being
/// host-reserved by not advertising it as a back key.
fn render_help_bar(buf: &mut RBuffer, area: RRect, state: &GraphState) {
    let mut spans = vec![Span::raw(" ")];
    spans.extend(help_key("↑↓", "move"));
    // The toggle label names the OTHER view (where `c` takes you).
    let toggle_desc = match state.view {
        // Map renders its own help bar and returns before reaching here.
        View::Entities | View::Map => "communities",
        View::Communities => "entities",
    };
    spans.extend(help_key("c", toggle_desc));
    spans.extend(help_key("v", "map"));
    spans.extend(help_key("Bksp", "back"));
    spans.extend(help_key("Tab", "pane"));
    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// Build the ego subgraph for the map's current state and move the selection.
fn nav(map: &mut MapState, rels: &[Relationship], dir: Nav) -> bool {
    let ego = map.subgraph(rels);
    map.navigate(&ego, dir)
}

/// A rounded panel with a gold title; the border is gold when `active`
/// (focused), else cornflower-blue — mirrors the Search query-box focus cue.
fn panel(title: &str, active: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if active { GOLD } else { CORNFLOWER_BLUE }))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
        ))
}

/// The selected-row vs normal-row style.
fn row_style(is_sel: bool) -> Style {
    if is_sel {
        Style::default()
            .fg(SELECTION_GREEN)
            .bg(LIST_HIGHLIGHT_BG)
            .add_modifier(RModifier::BOLD)
    } else {
        Style::default().fg(SOFT_WHITE)
    }
}

/// A muted-italic empty-state line.
fn empty_line(text: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        text,
        Style::default().fg(MUTED_GRAY).add_modifier(RModifier::ITALIC),
    ))
}

/// A live help-bar entry: gold key glyph + muted description.
fn help_key(key: &str, desc: &str) -> [Span<'static>; 3] {
    [
        Span::styled(
            key.to_string(),
            Style::default().fg(GOLD).add_modifier(RModifier::BOLD),
        ),
        Span::styled(format!(" {desc}"), Style::default().fg(MUTED_GRAY)),
        Span::styled("  ", Style::default().fg(MUTED_GRAY)),
    ]
}

/// Truncate a string to at most `max` characters, appending `…` when clipped.
/// Char-based — never byte-slices — so a multibyte entity / community name
/// can't panic on a char boundary (the Rust UTF-8 truncate trap).
fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Provenance, Relationship};

    fn rel(source: &str, target: &str, rel_type: &str) -> Relationship {
        Relationship {
            source: source.into(),
            target: target.into(),
            rel_type: rel_type.into(),
            description: String::new(),
            strength: None,
        }
    }

    fn record(id: &str, rels: Vec<Relationship>) -> LearningRecord {
        LearningRecord {
            id: id.into(),
            title: id.into(),
            scope: "universal".into(),
            confidence: 0.9,
            category: "process".into(),
            tags: vec![],
            source_tool: None,
            project: None,
            key_insight: String::new(),
            body_md: String::new(),
            entities: vec![],
            relationships: rels,
            provenance: Provenance::default(),
        }
    }

    fn community(id: &str, title: &str) -> Community {
        Community {
            id: id.into(),
            title: title.into(),
            summary: "s".into(),
            level: 0,
            nodes: vec!["a".into(), "b".into()],
            rating: 7.0,
        }
    }

    #[test]
    fn adjacency_aggregates_typed_edges_keyed_by_entity_name() {
        let mut st = GraphState::default();
        st.set_records(&[
            record(
                "r1",
                vec![rel("audit-after-rebase", "stale plan execution", "solves")],
            ),
            record(
                "r2",
                vec![rel("tokio Runtime", "runtime drop panic", "causes")],
            ),
        ]);
        // Entities sorted by name: `audit-after-rebase` first.
        assert_eq!(st.nodes.len(), 2);
        assert_eq!(st.nodes[0].name, "audit-after-rebase");
        assert_eq!(st.nodes[0].edges.len(), 1);
        assert_eq!(st.nodes[0].edges[0].rel_type, "solves");
        assert_eq!(st.nodes[0].edges[0].target, "stale plan execution");
    }

    #[test]
    fn empty_rel_type_falls_back_to_default() {
        let mut st = GraphState::default();
        st.set_records(&[record("r1", vec![rel("a", "b", "")])]);
        assert_eq!(st.nodes[0].edges[0].rel_type, DEFAULT_REL_TYPE);
    }

    #[test]
    fn c_toggles_view_and_resets_selection() {
        let mut st = GraphState::default();
        st.set_records(&[record(
            "r1",
            vec![rel("a", "b", "solves"), rel("c", "d", "solves")],
        )]);
        st.set_communities(vec![community("1", "Cluster 1")]);
        st.focus();
        assert_eq!(st.view, View::Entities);
        // Move the selection, then toggle — selection resets to the top.
        st.handle_key(&KeyCode::Down);
        assert_eq!(st.selected, 1);
        assert!(st.handle_key(&KeyCode::Char { ch: 'c' }));
        assert_eq!(st.view, View::Communities);
        assert_eq!(st.selected, 0);
        // Toggle back.
        assert!(st.handle_key(&KeyCode::Char { ch: 'c' }));
        assert_eq!(st.view, View::Entities);
    }

    #[test]
    fn down_up_clamp_within_active_list() {
        let mut st = GraphState::default();
        st.set_records(&[record(
            "r1",
            vec![rel("a", "x", "solves"), rel("b", "y", "solves")],
        )]);
        st.focus();
        assert_eq!(st.selected, 0);
        assert!(st.handle_key(&KeyCode::Down));
        assert_eq!(st.selected, 1);
        // Down at the bottom is a clamped no-op.
        assert!(!st.handle_key(&KeyCode::Down));
        assert!(st.handle_key(&KeyCode::Up));
        assert_eq!(st.selected, 0);
        // Up at the top is a clamped no-op.
        assert!(!st.handle_key(&KeyCode::Up));
    }

    #[test]
    fn focus_blur_toggles_focused_flag() {
        let mut st = GraphState::default();
        assert!(!st.is_focused());
        st.focus();
        assert!(st.is_focused());
        st.blur();
        assert!(!st.is_focused());
    }

    fn graph_with_fixture() -> GraphState {
        let mut st = GraphState::default();
        st.set_records(&[
            record(
                "r1",
                vec![
                    rel("audit-after-rebase", "stale plan execution", "solves"),
                    rel("stale plan execution", "git pull --rebase", "caused_by"),
                ],
            ),
            record(
                "r2",
                vec![rel("tokio Runtime", "runtime drop panic", "causes")],
            ),
        ]);
        st.focus();
        st
    }

    #[test]
    fn v_toggles_into_and_out_of_the_map() {
        let mut st = graph_with_fixture();
        assert!(!st.in_map());
        // Enter the map centred on the selected entity (sorted-first).
        assert!(st.handle_key(&KeyCode::Char { ch: 'v' }));
        assert!(st.in_map());
        assert_eq!(st.map.as_ref().unwrap().center(), "audit-after-rebase");
        // `v` again exits back to the neighbourhood.
        assert!(st.handle_key(&KeyCode::Char { ch: 'v' }));
        assert!(!st.in_map());
    }

    #[test]
    fn map_routes_hop_expand_and_recentre() {
        let mut st = graph_with_fixture();
        st.handle_key(&KeyCode::Char { ch: 'v' });
        // h toggles hop, e toggles expand, ⏎ recentres on a moved selection.
        assert!(st.handle_key(&KeyCode::Char { ch: 'h' }));
        assert!(st.handle_key(&KeyCode::Char { ch: 'e' }));
        assert!(st.handle_key(&KeyCode::Down)); // select a ring node
        let moved = st.map_selected_entity().unwrap();
        assert_ne!(moved, "audit-after-rebase");
        assert!(st.handle_key(&KeyCode::Enter)); // recentre
        assert_eq!(st.map.as_ref().unwrap().center(), moved);
    }

    #[test]
    fn backspace_exits_map_and_lands_list_on_the_centre() {
        let mut st = graph_with_fixture();
        st.handle_key(&KeyCode::Char { ch: 'v' });
        st.handle_key(&KeyCode::Down); // move to a ring node
        st.handle_key(&KeyCode::Enter); // recentre there
        let landed = st.map.as_ref().unwrap().center().to_string();
        assert!(st.handle_key(&KeyCode::Backspace)); // exit map
        assert!(!st.in_map());
        // The list selection now points at the entity the map ended on.
        assert_eq!(st.nodes[st.selected].name, landed);
    }

    #[test]
    fn truncate_chars_is_char_safe_on_multibyte() {
        let s = "🧠".repeat(10);
        let out = truncate_chars(&s, 4);
        assert_eq!(out.chars().count(), 4);
        assert!(out.ends_with('…'));
        assert_eq!(truncate_chars("abc", 0), "");
    }
}
