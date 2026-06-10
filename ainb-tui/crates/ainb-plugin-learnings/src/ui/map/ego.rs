//! Ego-subgraph extraction for the radial local-graph map.
//!
//! Given the union of every record's typed `relationships[]`, a centre entity,
//! and a hop depth, [`EgoSubgraph::build`] walks the **undirected** adjacency
//! (a neighbour is anything the centre points at *or* that points at the centre)
//! out to `hop` rings, applying a deterministic cap on the ring-1 neighbours so a
//! hot hub folds its overflow into a single synthetic `[+N more]` node.
//!
//! Determinism is load-bearing: the radial layout downstream assigns angular
//! slots by node order, and the tripwires assert exact tokens. Ring-1 neighbours
//! are therefore ordered by `(descending edge strength, ascending name)` and the
//! cap keeps the first `cap` of that order — never an arbitrary hash order.

use std::collections::{BTreeSet, HashMap};

use crate::data::{DEFAULT_REL_TYPE, Relationship};

/// Default ring-1 neighbour cap before overflow folds into `[+N more]`.
pub const DEFAULT_NODE_CAP: usize = 15;

/// Whether an edge of `rel_type` carries a direction (renders an arrowhead).
/// Only `relates_to` (the untyped fallback) is undirected; every other type
/// (`solves` / `caused_by` / `causes` / `requires` / …) is directed.
#[must_use]
pub fn is_directed(rel_type: &str) -> bool {
    rel_type != DEFAULT_REL_TYPE
}

/// Normalize a possibly-empty relationship type to [`DEFAULT_REL_TYPE`].
fn normalize_rel(rel_type: &str) -> String {
    if rel_type.is_empty() {
        DEFAULT_REL_TYPE.to_string()
    } else {
        rel_type.to_string()
    }
}

/// Arrowhead on an ego edge, relative to the `from → to` wire (where `from` is
/// the endpoint closer to the centre).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    /// Points `from → to` — the relationship's source is the parent (closer).
    Forward,
    /// Points `to → from` — the relationship's source is the child (farther).
    Backward,
    /// No arrowhead — an undirected (`relates_to`) edge.
    None,
}

/// One entity node in the ego subgraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgoNode {
    /// Entity name, or the synthetic `+N more` label when `overflow`.
    pub name: String,
    /// Hop distance from the centre (`0` = centre, `1` = direct neighbour, …).
    pub ring: u8,
    /// `true` for the synthetic overflow node standing in for capped neighbours.
    pub overflow: bool,
    /// How many real neighbours this overflow node represents (`0` otherwise).
    pub overflow_count: usize,
}

/// A typed edge between two ego nodes (parent closer to the centre → child).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgoEdge {
    /// Parent endpoint (closer to the centre).
    pub from: String,
    /// Child endpoint (farther from the centre).
    pub to: String,
    /// Normalized relationship type (never empty; `relates_to` when untyped).
    pub rel_type: String,
    /// Arrowhead direction relative to `from → to`.
    pub arrow: Arrow,
}

/// The extracted ego neighbourhood: centre + N-hop nodes (ring-1 capped) + the
/// typed edges connecting them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgoSubgraph {
    /// The centre entity the view is built around.
    pub center: String,
    /// Hop depth this subgraph was extracted at.
    pub hop: u8,
    /// Nodes, centre first (ring 0), then ring-1, ring-2, … in render order.
    pub nodes: Vec<EgoNode>,
    /// Typed edges between the nodes.
    pub edges: Vec<EgoEdge>,
    /// Distinct ring-1 neighbours BEFORE the cap (drives the `+N` overflow count
    /// and the `nodes:N (+M)` header).
    pub neighbor_count: usize,
}

/// One adjacency entry: a neighbour reachable from a node via one relationship,
/// remembering the original wire direction so the arrow can be resolved.
#[derive(Debug, Clone)]
struct AdjEntry {
    other: String,
    rel_type: String,
    strength: u32,
    /// `true` when the node owning this entry is the relationship's *source*
    /// (so a `node → other` arrow points forward / away from the node).
    src_is_self: bool,
}

/// Pre-built undirected adjacency over the WHOLE KB's typed relationships.
///
/// Building this is O(R) over every edge (two map inserts + two clones per
/// edge), so it is built ONCE — in `GraphState::set_records`, on a data reload —
/// and shared across every ego extraction. An [`EgoSubgraph::build`] then costs
/// only O(degree(center)) per ring, not O(R): the recentre animation re-extracts
/// the same ego ~6× per recentre, every keystroke re-extracts, and every mouse
/// click re-extracts — all of which previously paid the full O(R) adjacency
/// rebuild. Holding it here makes those rebuilds cheap.
#[derive(Debug, Clone, Default)]
pub struct Adjacency {
    map: HashMap<String, Vec<AdjEntry>>,
}

impl Adjacency {
    /// Build the undirected adjacency map from typed relationships, skipping
    /// self-loops and normalizing empty types to `relates_to`. O(R) over the
    /// whole edge set — call once per data reload, not per render frame.
    #[must_use]
    pub fn build(rels: &[Relationship]) -> Self {
        Self {
            map: build_adjacency(rels),
        }
    }
}

impl EgoSubgraph {
    /// Extract the ego subgraph centred on `center` out to `hop` rings from a
    /// pre-built [`Adjacency`].
    ///
    /// - `cap` bounds the ring-1 neighbours; overflow folds into one `[+N more]`
    ///   node unless `expanded` is set (the `e` action), which shows them all.
    /// - Self-loops (`source == target`) are ignored (already absent from `adj`).
    /// - An unknown or isolated `center` yields a lone centre node.
    ///
    /// Cost is O(degree(center)) per ring — the expensive O(R) adjacency build
    /// is done once in [`Adjacency::build`], NOT here, so the recentre animation
    /// and per-keystroke re-extracts are cheap.
    #[must_use]
    pub fn build(adj: &Adjacency, center: &str, hop: u8, cap: usize, expanded: bool) -> Self {
        let adj = &adj.map;

        let mut subgraph = Self {
            center: center.to_string(),
            hop,
            nodes: vec![EgoNode {
                name: center.to_string(),
                ring: 0,
                overflow: false,
                overflow_count: 0,
            }],
            edges: Vec::new(),
            neighbor_count: 0,
        };

        let mut visited: BTreeSet<String> = BTreeSet::new();
        visited.insert(center.to_string());

        // ---- Ring 1: the centre's direct neighbours (capped) ----
        let ring1 = ordered_neighbors(adj, center, &visited);
        subgraph.neighbor_count = ring1.len();

        let cap = cap.max(1);
        let (kept, dropped): (&[Neighbor], usize) = if expanded || ring1.len() <= cap {
            (&ring1, 0)
        } else {
            (&ring1[..cap], ring1.len() - cap)
        };

        for nb in kept {
            visited.insert(nb.name.clone());
            subgraph.nodes.push(EgoNode {
                name: nb.name.clone(),
                ring: 1,
                overflow: false,
                overflow_count: 0,
            });
            subgraph.edges.push(edge_for(center, nb));
        }
        if dropped > 0 {
            // One synthetic node stands in for every capped neighbour. It owns
            // no edge — it's a "dig deeper" affordance (`e` expands it), not a
            // real relationship.
            subgraph.nodes.push(EgoNode {
                name: format!("+{dropped} more"),
                ring: 1,
                overflow: true,
                overflow_count: dropped,
            });
        }

        // ---- Ring 2..=hop: neighbours of the kept frontier ----
        if hop >= 2 {
            let mut frontier: Vec<String> = kept.iter().map(|n| n.name.clone()).collect();
            for ring in 2..=hop {
                let mut next: Vec<String> = Vec::new();
                for parent in &frontier {
                    for nb in ordered_neighbors(adj, parent, &visited) {
                        if visited.contains(&nb.name) {
                            continue;
                        }
                        visited.insert(nb.name.clone());
                        subgraph.nodes.push(EgoNode {
                            name: nb.name.clone(),
                            ring,
                            overflow: false,
                            overflow_count: 0,
                        });
                        subgraph.edges.push(edge_for(parent, &nb));
                        next.push(nb.name.clone());
                    }
                }
                frontier = next;
                if frontier.is_empty() {
                    break;
                }
            }
        }

        subgraph
    }
}

/// A ring neighbour resolved to its representative edge (strongest, then named).
struct Neighbor {
    name: String,
    rel_type: String,
    strength: u32,
    src_is_self: bool,
}

/// Build the representative edge from `parent` to a resolved `neighbor`.
fn edge_for(parent: &str, nb: &Neighbor) -> EgoEdge {
    let arrow = if is_directed(&nb.rel_type) {
        if nb.src_is_self {
            Arrow::Forward
        } else {
            Arrow::Backward
        }
    } else {
        Arrow::None
    };
    EgoEdge {
        from: parent.to_string(),
        to: nb.name.clone(),
        rel_type: nb.rel_type.clone(),
        arrow,
    }
}

/// Build the undirected adjacency map from typed relationships, skipping
/// self-loops and normalizing empty types to `relates_to`.
fn build_adjacency(rels: &[Relationship]) -> HashMap<String, Vec<AdjEntry>> {
    let mut adj: HashMap<String, Vec<AdjEntry>> = HashMap::new();
    for rel in rels {
        if rel.source == rel.target {
            continue;
        }
        let rel_type = normalize_rel(&rel.rel_type);
        let strength = rel.strength.unwrap_or(0);
        adj.entry(rel.source.clone()).or_default().push(AdjEntry {
            other: rel.target.clone(),
            rel_type: rel_type.clone(),
            strength,
            src_is_self: true,
        });
        adj.entry(rel.target.clone()).or_default().push(AdjEntry {
            other: rel.source.clone(),
            rel_type,
            strength,
            src_is_self: false,
        });
    }
    adj
}

/// Resolve `node`'s distinct neighbours (excluding already-`visited` nodes) to
/// one representative edge each, ordered by `(descending strength, ascending
/// name)`. When a neighbour is reachable via multiple relationships, the
/// strongest (then lexicographically-first `rel_type`) wins.
fn ordered_neighbors(
    adj: &HashMap<String, Vec<AdjEntry>>,
    node: &str,
    visited: &BTreeSet<String>,
) -> Vec<Neighbor> {
    let Some(entries) = adj.get(node) else {
        return Vec::new();
    };

    // Pick the representative edge per distinct neighbour.
    let mut best: HashMap<&str, &AdjEntry> = HashMap::new();
    for e in entries {
        if e.other == node || visited.contains(&e.other) {
            continue;
        }
        best.entry(e.other.as_str())
            .and_modify(|cur| {
                // Total tiebreak so the representative — and thus the rendered
                // arrow direction — is a pure function of the edge SET, never of
                // the order relationships happened to arrive in: strongest, then
                // lexicographically-first rel_type, then prefer the
                // centre-is-source orientation (`src_is_self`). Without the final
                // discriminator, A→B and B→A at equal strength + type would let
                // input order silently flip the arrowhead.
                let replace = e.strength > cur.strength
                    || (e.strength == cur.strength && e.rel_type < cur.rel_type)
                    || (e.strength == cur.strength
                        && e.rel_type == cur.rel_type
                        && e.src_is_self
                        && !cur.src_is_self);
                if replace {
                    *cur = e;
                }
            })
            .or_insert(e);
    }

    let mut neighbors: Vec<Neighbor> = best
        .into_values()
        .map(|e| Neighbor {
            name: e.other.clone(),
            rel_type: e.rel_type.clone(),
            strength: e.strength,
            src_is_self: e.src_is_self,
        })
        .collect();
    // Deterministic order: strongest first, ties broken by name.
    neighbors.sort_by(|a, b| b.strength.cmp(&a.strength).then_with(|| a.name.cmp(&b.name)));
    neighbors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(source: &str, target: &str, rel_type: &str, strength: Option<u32>) -> Relationship {
        Relationship {
            source: source.into(),
            target: target.into(),
            rel_type: rel_type.into(),
            description: String::new(),
            strength,
        }
    }

    fn node<'a>(g: &'a EgoSubgraph, name: &str) -> Option<&'a EgoNode> {
        g.nodes.iter().find(|n| n.name == name)
    }

    fn edge<'a>(g: &'a EgoSubgraph, to: &str) -> Option<&'a EgoEdge> {
        g.edges.iter().find(|e| e.to == to)
    }

    #[test]
    fn ego_includes_both_outgoing_and_incoming_neighbors() {
        // centre is the SOURCE of one edge and the TARGET of another.
        let rels = vec![
            rel("centre", "out-target", "solves", Some(5)),
            rel("in-source", "centre", "caused_by", Some(5)),
        ];
        let g = EgoSubgraph::build(
            &Adjacency::build(&rels),
            "centre",
            1,
            DEFAULT_NODE_CAP,
            false,
        );
        assert_eq!(g.nodes[0].name, "centre");
        assert_eq!(g.nodes[0].ring, 0);
        assert!(
            node(&g, "out-target").is_some(),
            "outgoing neighbour present"
        );
        assert!(
            node(&g, "in-source").is_some(),
            "incoming neighbour present"
        );
        assert_eq!(g.neighbor_count, 2);
    }

    #[test]
    fn arrow_direction_follows_relationship_source() {
        let rels = vec![
            rel("centre", "out-target", "solves", Some(5)),
            rel("in-source", "centre", "caused_by", Some(5)),
            rel("centre", "peer", "relates_to", Some(5)),
        ];
        let g = EgoSubgraph::build(
            &Adjacency::build(&rels),
            "centre",
            1,
            DEFAULT_NODE_CAP,
            false,
        );
        // centre is the source → arrow points away from centre.
        assert_eq!(edge(&g, "out-target").unwrap().arrow, Arrow::Forward);
        // centre is the target → arrow points back toward centre.
        assert_eq!(edge(&g, "in-source").unwrap().arrow, Arrow::Backward);
        // relates_to → no arrowhead.
        assert_eq!(edge(&g, "peer").unwrap().arrow, Arrow::None);
    }

    #[test]
    fn cap_overflows_into_plus_n_more_node() {
        // 5 neighbours, cap 3 → keep 3 + one "+2 more".
        let rels: Vec<Relationship> =
            (0..5).map(|i| rel("hub", &format!("n{i}"), "solves", Some(1))).collect();
        let g = EgoSubgraph::build(&Adjacency::build(&rels), "hub", 1, 3, false);
        assert_eq!(g.neighbor_count, 5);
        let overflow = g.nodes.iter().find(|n| n.overflow).expect("overflow node");
        assert_eq!(overflow.name, "+2 more");
        assert_eq!(overflow.overflow_count, 2);
        // 1 centre + 3 kept + 1 overflow = 5 nodes.
        assert_eq!(g.nodes.len(), 5);
    }

    #[test]
    fn expanded_shows_all_neighbors_no_overflow() {
        let rels: Vec<Relationship> =
            (0..5).map(|i| rel("hub", &format!("n{i}"), "solves", Some(1))).collect();
        let g = EgoSubgraph::build(&Adjacency::build(&rels), "hub", 1, 3, true);
        assert!(
            !g.nodes.iter().any(|n| n.overflow),
            "expanded → no overflow node"
        );
        // 1 centre + 5 neighbours.
        assert_eq!(g.nodes.len(), 6);
    }

    #[test]
    fn cap_keeps_strongest_then_alphabetical() {
        let rels = vec![
            rel("hub", "weak", "solves", Some(1)),
            rel("hub", "strong", "solves", Some(9)),
            rel("hub", "mid-b", "solves", Some(5)),
            rel("hub", "mid-a", "solves", Some(5)),
        ];
        // cap 2 → strongest (`strong`, 9) then the alphabetically-first of the
        // strength-5 pair (`mid-a`). `mid-b` and `weak` fold into +2.
        let g = EgoSubgraph::build(&Adjacency::build(&rels), "hub", 1, 2, false);
        assert!(node(&g, "strong").is_some());
        assert!(node(&g, "mid-a").is_some());
        assert!(node(&g, "mid-b").is_none());
        assert_eq!(
            g.nodes.iter().find(|n| n.overflow).unwrap().overflow_count,
            2
        );
    }

    #[test]
    fn isolated_center_is_lone_node() {
        let rels = vec![rel("other-a", "other-b", "solves", Some(5))];
        let g = EgoSubgraph::build(
            &Adjacency::build(&rels),
            "centre",
            1,
            DEFAULT_NODE_CAP,
            false,
        );
        assert_eq!(g.nodes.len(), 1);
        assert_eq!(g.nodes[0].name, "centre");
        assert!(g.edges.is_empty());
        assert_eq!(g.neighbor_count, 0);
    }

    #[test]
    fn self_loops_are_ignored() {
        let rels = vec![rel("centre", "centre", "solves", Some(5))];
        let g = EgoSubgraph::build(
            &Adjacency::build(&rels),
            "centre",
            1,
            DEFAULT_NODE_CAP,
            false,
        );
        assert_eq!(g.nodes.len(), 1);
        assert!(g.edges.is_empty());
    }

    #[test]
    fn hop_two_pulls_in_second_ring() {
        let rels = vec![
            rel("centre", "mid", "solves", Some(5)),
            rel("mid", "leaf", "requires", Some(5)),
        ];
        let h1 = EgoSubgraph::build(
            &Adjacency::build(&rels),
            "centre",
            1,
            DEFAULT_NODE_CAP,
            false,
        );
        assert!(
            node(&h1, "leaf").is_none(),
            "hop 1 stops at the direct ring"
        );

        let h2 = EgoSubgraph::build(
            &Adjacency::build(&rels),
            "centre",
            2,
            DEFAULT_NODE_CAP,
            false,
        );
        let leaf = node(&h2, "leaf").expect("hop 2 reaches the second ring");
        assert_eq!(leaf.ring, 2);
        // The ring-2 edge connects mid → leaf, not centre → leaf.
        assert_eq!(edge(&h2, "leaf").unwrap().from, "mid");
    }

    #[test]
    fn multi_relationship_neighbor_picks_strongest_representative() {
        // Same neighbour reachable via two edges of different strength → the
        // stronger one (and its type) is the representative.
        let rels = vec![
            rel("centre", "peer", "relates_to", Some(2)),
            rel("centre", "peer", "solves", Some(9)),
        ];
        let g = EgoSubgraph::build(
            &Adjacency::build(&rels),
            "centre",
            1,
            DEFAULT_NODE_CAP,
            false,
        );
        let e = edge(&g, "peer").unwrap();
        assert_eq!(
            e.rel_type, "solves",
            "stronger edge wins the representative"
        );
        assert_eq!(e.arrow, Arrow::Forward);
    }

    #[test]
    fn opposite_direction_tie_is_order_independent() {
        // A directed edge in BOTH directions at equal strength + type. The
        // representative must NOT depend on which relationship was listed first
        // — the total tiebreak prefers the centre-is-source orientation, so the
        // arrow is the same regardless of input order.
        let forward_first = vec![
            rel("centre", "peer", "causes", Some(5)),
            rel("peer", "centre", "causes", Some(5)),
        ];
        let backward_first = vec![
            rel("peer", "centre", "causes", Some(5)),
            rel("centre", "peer", "causes", Some(5)),
        ];
        let a = EgoSubgraph::build(
            &Adjacency::build(&forward_first),
            "centre",
            1,
            DEFAULT_NODE_CAP,
            false,
        );
        let b = EgoSubgraph::build(
            &Adjacency::build(&backward_first),
            "centre",
            1,
            DEFAULT_NODE_CAP,
            false,
        );
        assert_eq!(edge(&a, "peer").unwrap().arrow, Arrow::Forward);
        assert_eq!(
            edge(&a, "peer").unwrap().arrow,
            edge(&b, "peer").unwrap().arrow,
            "arrow must be input-order independent"
        );
    }

    #[test]
    fn empty_rel_type_is_undirected_relates_to() {
        let rels = vec![rel("centre", "peer", "", Some(5))];
        let g = EgoSubgraph::build(
            &Adjacency::build(&rels),
            "centre",
            1,
            DEFAULT_NODE_CAP,
            false,
        );
        let e = edge(&g, "peer").unwrap();
        assert_eq!(e.rel_type, DEFAULT_REL_TYPE);
        assert_eq!(e.arrow, Arrow::None);
    }
}
