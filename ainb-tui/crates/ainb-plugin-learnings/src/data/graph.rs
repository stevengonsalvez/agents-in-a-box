//! Graph readers: nano_graphrag `*.graphml` (XML) + `kv_store_community_reports.json`.
//!
//! The graphml carries the entity nodes and the relationship edges. Node ids
//! arrive quote-wrapped (`"NAME"`) in the source; we strip the surrounding
//! quotes so ids match the bare entity names used elsewhere.
//!
//! **Edge typing:** the live nano_graphrag graphml does not emit a typed
//! relationship on edges — only a free-text `description` (key `d5`) and a
//! numeric `weight` (key `d4`). When the producer *does* carry a typed
//! relationship (our fixture, or a future schema), it lands on an edge `data`
//! key whose declared `attr.name` is `rel_type`; we resolve the key id from the
//! `<key>` table and read it. Edges without it fall back to
//! [`DEFAULT_REL_TYPE`], which is what every current real-KB edge hits.

use std::collections::HashMap;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use serde::{Deserialize, Serialize};

use super::{error::DataError, io_err};

/// Relationship type assigned to an edge that carries no typed `rel_type`
/// (the common case in the live KB, whose edges hold only a free-text
/// description).
pub const DEFAULT_REL_TYPE: &str = "relates_to";

/// A parsed graph: entity nodes + typed edges.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Graph {
    /// Entity nodes.
    pub entities: Vec<GraphEntity>,
    /// Relationship edges.
    pub edges: Vec<Edge>,
}

/// A graph entity node.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphEntity {
    /// Node id with the source quote-wrapping stripped.
    pub id: String,
    /// `entity_type` (`PATTERN` / `TOOL` / `ERROR` / ...), quotes stripped.
    pub entity_type: String,
    /// Free-text description, quotes stripped.
    pub description: String,
}

/// A typed relationship edge.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    /// Source node id (quotes stripped).
    pub source: String,
    /// Target node id (quotes stripped).
    pub target: String,
    /// Relationship type, or [`DEFAULT_REL_TYPE`] when the source is untyped.
    pub rel_type: String,
}

/// A community cluster from `kv_store_community_reports.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct Community {
    /// Community id (the JSON object key).
    pub id: String,
    /// Display title (`report_json.title`, falling back to the top-level `title`).
    pub title: String,
    /// One-line summary from `report_json.summary`.
    pub summary: String,
    /// Hierarchy level.
    pub level: i64,
    /// Member node ids (quote-wrapping preserved as stored).
    pub nodes: Vec<String>,
    /// Impact rating from `report_json.rating`.
    pub rating: f64,
}

/// Parse a nano_graphrag `*.graphml` into entities + typed edges.
///
/// Malformed XML (truncated, unbalanced tags, bad encoding) is a typed
/// [`DataError::GraphMl`] — never a panic.
pub fn parse_graphml(path: &Path) -> Result<Graph, DataError> {
    let raw = std::fs::read_to_string(path).map_err(|e| io_err(path, e))?;
    parse_graphml_str(&raw).map_err(|detail| DataError::GraphMl {
        path: path.display().to_string(),
        detail,
    })
}

/// Inner parser over a graphml string; returns a stringified XML error on
/// failure so the path-carrying wrapper can attach the file name.
fn parse_graphml_str(xml: &str) -> Result<Graph, String> {
    let mut reader = Reader::from_str(xml);
    let config = reader.config_mut();
    config.trim_text(true);

    // Resolve the `<key>` table: declared edge attr.name == "rel_type" → its id.
    let rel_type_key = find_rel_type_key(xml)?;

    let mut graph = Graph::default();
    let mut buf = Vec::new();

    // Parser state for the element currently being filled.
    let mut cur_node: Option<NodeBuilder> = None;
    let mut cur_edge: Option<EdgeBuilder> = None;
    // The `key=` of the `<data>` we are inside, so the following Text event
    // is attributed to the right field.
    let mut cur_data_key: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => return Err(e.to_string()),
            Ok(Event::Eof) => break,
            // `Start` opens an element with children; `Empty` is a
            // self-closing element (e.g. `<node id="x"/>` for an entity with
            // no `<data>`). Both carry the same attributes, so we handle the
            // tag name identically. An `Empty` node/edge has no following
            // `End`, so we finalise it here.
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"node" => {
                    cur_node = Some(NodeBuilder::new(strip_quotes(&attr_value(&e, b"id"))));
                }
                b"edge" => {
                    let source = strip_quotes(&attr_value(&e, b"source"));
                    let target = strip_quotes(&attr_value(&e, b"target"));
                    cur_edge = Some(EdgeBuilder::new(source, target));
                }
                b"data" => {
                    cur_data_key = Some(attr_value(&e, b"key"));
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"node" => {
                    graph
                        .entities
                        .push(NodeBuilder::new(strip_quotes(&attr_value(&e, b"id"))).build());
                }
                b"edge" => {
                    let source = strip_quotes(&attr_value(&e, b"source"));
                    let target = strip_quotes(&attr_value(&e, b"target"));
                    graph.edges.push(EdgeBuilder::new(source, target).build());
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                // `xml_content` decodes the bytes *and* resolves XML entities
                // (`&lt;` → `<`, `&quot;` → `"`), which the nano_graphrag text
                // payloads contain.
                let text = t.xml_content().map_err(|e| e.to_string())?.into_owned();
                if let Some(key) = cur_data_key.as_deref() {
                    if let Some(node) = cur_node.as_mut() {
                        node.apply(key, &text);
                    } else if let Some(edge) = cur_edge.as_mut() {
                        edge.apply(key, &text, rel_type_key.as_deref());
                    }
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"node" => {
                    if let Some(node) = cur_node.take() {
                        graph.entities.push(node.build());
                    }
                }
                b"edge" => {
                    if let Some(edge) = cur_edge.take() {
                        graph.edges.push(edge.build());
                    }
                }
                b"data" => {
                    cur_data_key = None;
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }

    Ok(graph)
}

/// Scan the `<key ... attr.name="rel_type" for="edge">` declaration and return
/// its `id`, if present. The producer may name the carrying key anything
/// (`d_rel`, `d8`, ...); we resolve it by its declared `attr.name` rather than
/// hard-coding the id.
fn find_rel_type_key(xml: &str) -> Result<Option<String>, String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => return Err(e.to_string()),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.name().as_ref() == b"key" => {
                let for_attr = attr_value(&e, b"for");
                let name = attr_value(&e, b"attr.name");
                if for_attr == "edge" && name == "rel_type" {
                    let id = attr_value(&e, b"id");
                    if !id.is_empty() {
                        return Ok(Some(id));
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(None)
}

/// Accumulates a node's `<data>` fields until its `</node>`.
struct NodeBuilder {
    id: String,
    entity_type: String,
    description: String,
}

impl NodeBuilder {
    fn new(id: String) -> Self {
        Self {
            id,
            entity_type: String::new(),
            description: String::new(),
        }
    }

    fn apply(&mut self, key: &str, text: &str) {
        match key {
            // d0 = entity_type, d1 = description (nano_graphrag node schema).
            "d0" => self.entity_type = strip_quotes(text),
            "d1" => self.description = strip_quotes(text),
            _ => {}
        }
    }

    fn build(self) -> GraphEntity {
        GraphEntity {
            id: self.id,
            entity_type: self.entity_type,
            description: self.description,
        }
    }
}

/// Accumulates an edge's `<data>` fields until its `</edge>`.
struct EdgeBuilder {
    source: String,
    target: String,
    rel_type: Option<String>,
}

impl EdgeBuilder {
    fn new(source: String, target: String) -> Self {
        Self {
            source,
            target,
            rel_type: None,
        }
    }

    fn apply(&mut self, key: &str, text: &str, rel_type_key: Option<&str>) {
        if Some(key) == rel_type_key {
            self.rel_type = Some(strip_quotes(text));
        }
    }

    fn build(self) -> Edge {
        Edge {
            source: self.source,
            target: self.target,
            rel_type: self
                .rel_type
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_REL_TYPE.to_string()),
        }
    }
}

/// Read an attribute's value as an unescaped `String`, or empty if absent.
fn attr_value(e: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> String {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
        .unwrap_or_default()
}

/// Strip a single layer of surrounding double quotes, if present. The
/// nano_graphrag exporter wraps node ids / type / description in literal
/// quotes inside the XML text.
fn strip_quotes(s: &str) -> String {
    let t = s.trim();
    t.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(t).to_string()
}

/// Parse `kv_store_community_reports.json` into a sorted `Vec<Community>`.
///
/// The JSON is an object keyed by community id. We sort by numeric id when
/// possible (else lexicographically) so the Graph community list is stable.
/// Malformed JSON is a typed [`DataError::Json`].
pub fn parse_community_reports(path: &Path) -> Result<Vec<Community>, DataError> {
    let raw = std::fs::read_to_string(path).map_err(|e| io_err(path, e))?;
    let map: HashMap<String, RawCommunity> =
        serde_json::from_str(&raw).map_err(|e| DataError::Json {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;

    let mut comms: Vec<Community> = map
        .into_iter()
        .map(|(id, raw)| {
            let report = raw.report_json.unwrap_or_default();
            Community {
                title: report.title.filter(|s| !s.is_empty()).unwrap_or(raw.title),
                summary: report.summary.unwrap_or_default(),
                rating: report.rating.unwrap_or_default(),
                level: raw.level,
                nodes: raw.nodes,
                id,
            }
        })
        .collect();

    comms.sort_by(|a, b| numeric_then_lex(&a.id, &b.id));
    Ok(comms)
}

/// Order two ids numerically when both parse as integers, else by string.
fn numeric_then_lex(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        _ => a.cmp(b),
    }
}

/// Raw community shape decoded straight from the JSON value.
#[derive(Debug, Deserialize)]
struct RawCommunity {
    #[serde(default)]
    report_json: Option<RawReportJson>,
    #[serde(default)]
    level: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    nodes: Vec<String>,
}

/// Raw `report_json` sub-object.
#[derive(Debug, Default, Deserialize, Serialize)]
struct RawReportJson {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    rating: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_quotes_unwraps_one_layer() {
        assert_eq!(strip_quotes("\"X\""), "X");
        assert_eq!(strip_quotes("X"), "X");
        assert_eq!(strip_quotes("\"a\"b\""), "a\"b");
    }

    #[test]
    fn numeric_ordering_beats_lexical() {
        assert_eq!(numeric_then_lex("2", "10"), std::cmp::Ordering::Less);
        assert_eq!(numeric_then_lex("a", "b"), std::cmp::Ordering::Less);
    }
}
