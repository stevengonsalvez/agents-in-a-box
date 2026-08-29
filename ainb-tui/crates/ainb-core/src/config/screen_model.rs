// ABOUTME: Turns CONFIG_REGISTRY into the rows and the section tree the
// settings screen renders, seeded from a serialized config.

//! The settings screen's model layer.
//!
//! [`CONFIG_REGISTRY`](super::registry::CONFIG_REGISTRY) describes the schema;
//! this module turns that description into what a user actually sees:
//!
//! - [`expand_key`] resolves a registry key's `*` segments against a real
//!   config, so `mcp_servers.*.shared` becomes one row per configured server.
//! - [`build_rows`] seeds one [`ConfigSetting`] per concrete key, grouped by
//!   [`ConfigCategory`].
//! - [`build_tree`] derives the collapsible left pane from the dotted keys, so
//!   the tree mirrors the TOML sections instead of being a second hand-written
//!   list that can drift from the first.
//!
//! Nothing here reads or writes a file. The caller supplies the seed and
//! decides what to do with the rows.

use std::collections::{BTreeMap, HashMap};

use crate::app::state::{ConfigCategory, ConfigSetting, ConfigValue};

use super::registry::{self, parse_dot_key};

/// How deep the tree may nest below a category root.
///
/// Container templates are the deepest real section
/// (`container_templates.<name>.config.image_source.build_args.<arg>`); four
/// levels reaches `image_source` and leaves the last hop as rows, which is
/// where a list beats another expandable node.
const MAX_TREE_DEPTH: usize = 4;

/// Expand a registry key into the concrete dotted paths a config actually has.
///
/// A key with no `*` is returned as-is even when the leaf is absent: an unset
/// `Option` still deserves a row, which is how you set it for the first time. A
/// key WITH a `*` can only be expanded against real data — nothing can invent
/// the name of an MCP server that isn't configured — so an absent or empty map
/// yields no rows.
#[must_use]
pub fn expand_key(key: &str, seed: &toml::Value) -> Vec<String> {
    if !key.contains('*') {
        return vec![key.to_string()];
    }
    let segments = parse_dot_key(key);
    let mut out = Vec::new();
    expand_from(seed, &segments, String::new(), &mut out);
    out
}

fn expand_from(node: &toml::Value, rest: &[String], prefix: String, out: &mut Vec<String>) {
    let Some(star) = rest.iter().position(|segment| segment == "*") else {
        out.push(join_path(&prefix, &rest.join(".")));
        return;
    };

    // Walk the literal segments in front of the star; a missing one means this
    // branch of the config simply isn't there.
    let mut current = node;
    for segment in &rest[..star] {
        match current.as_table().and_then(|table| table.get(segment)) {
            Some(child) => current = child,
            None => return,
        }
    }
    let Some(table) = current.as_table() else {
        return;
    };

    let base = join_path(&prefix, &rest[..star].join("."));
    for (name, child) in table {
        expand_from(child, &rest[star + 1..], join_path(&base, name), out);
    }
}

fn join_path(prefix: &str, tail: &str) -> String {
    match (prefix.is_empty(), tail.is_empty()) {
        (true, _) => tail.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}.{tail}"),
    }
}

/// Why a registry row cannot be edited from the settings screen, or `None`
/// when it can.
///
/// A row that accepts an edit and drops it is the failure this whole registry
/// exists to remove, so anything core cannot actually persist says so up front
/// instead of pretending.
#[must_use]
pub fn read_only_reason(key: &str) -> Option<&'static str> {
    let normalised = registry::registry_key(key);
    if normalised.starts_with("usage.") {
        // `READ_ONLY_SECTIONS` in config/mod.rs: `[usage]` is owned by the
        // burndown plugin, which does its own load-modify-save against this
        // file. Core writing it would revert a plan set from another process.
        return Some("read-only — [usage] is owned by the burndown plugin");
    }
    match registry::row(&normalised).map(|row| row.kind) {
        Some(registry::RowKind::Opaque) => {
            Some("read-only — structured value; edit it with `ainb config edit`")
        }
        _ => None,
    }
}

/// Build one [`ConfigSetting`] per visible schema leaf, grouped by category and
/// kept in registry order within each.
///
/// `seed` is a serialized config: the same tree
/// [`registry::navigate_toml`] reads and [`registry::set_validated`] writes, so
/// what a row displays and what a save writes can never disagree about where a
/// value lives.
#[must_use]
pub fn build_rows(seed: &toml::Value) -> HashMap<ConfigCategory, Vec<ConfigSetting>> {
    let mut grouped: HashMap<ConfigCategory, Vec<ConfigSetting>> = HashMap::new();
    for row in registry::rows() {
        for concrete in expand_key(row.key, seed) {
            let current = registry::navigate_toml(seed, &concrete).ok();
            let mut setting = row.to_setting(current);
            // The row's identity is its CONCRETE path — `mcp_servers.ctx.shared`,
            // not `mcp_servers.*.shared` — because that is what a save writes and
            // what the search pane shows.
            setting.key = concrete;
            widen_with_detected_choices(&mut setting);
            grouped.entry(row.category).or_default().push(setting);
        }
    }
    grouped
}

/// Offer a picker where the *options* come from the machine rather than the
/// schema.
///
/// `ui_preferences.preferred_editor` is a free-form command string — the
/// registry is right to call it `Text`, because any executable is valid. But
/// making the user type `subl` from memory is a step back from the picker of
/// detected editors the old hand-written screen had, so the row keeps its Text
/// semantics (a plain string is what gets written) while presenting the editors
/// actually installed here as choices.
///
/// The current value is kept in the list even when it is not one of the known
/// editors — including the empty value of an unset preference — so opening the
/// screen can never silently retarget a custom editor nor invent a choice the
/// user never made.
fn widen_with_detected_choices(setting: &mut ConfigSetting) {
    if setting.key != "ui_preferences.preferred_editor" {
        return;
    }
    let ConfigValue::Text(current) = &setting.value else {
        return;
    };
    let current = current.clone();

    let mut options: Vec<String> = crate::editors::get_installed_editors()
        .into_iter()
        .map(|(_, command)| command)
        .collect();
    if options.is_empty() {
        return; // Nothing detected: a text field beats an empty picker.
    }
    // An unset preference gets an explicit empty first option rather than
    // silently pre-selecting whichever editor was detected first. Showing
    // `Preferred Editor : code` to someone who never chose one is a claim about
    // their config that is not true — and confirming the row would write it.
    // The empty option round-trips to "remove the key" (`OPTIONAL_KEYS`).
    if !options.contains(&current) {
        options.insert(0, current.clone());
    }
    let selected = options.iter().position(|option| *option == current).unwrap_or(0);
    setting.value = ConfigValue::Choice(options, selected);
}

/// One node of the settings tree: a category root, or a TOML sub-table under
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigTreeNode {
    pub category: ConfigCategory,
    /// Dotted path this node covers. Empty when the category's rows are
    /// top-level leaves (`default_container_template`).
    pub path: String,
    /// What the left pane prints.
    pub label: String,
    /// Indentation level; 0 is the category root.
    pub depth: usize,
    /// Whether any node below this one exists, i.e. whether it can expand.
    pub has_children: bool,
    /// Indices into this category's row vector for the WHOLE subtree, so
    /// selecting a node shows everything under it.
    pub rows: Vec<usize>,
}

impl ConfigTreeNode {
    /// Stable identity for expansion state, persisted in
    /// `ui_preferences.config_tree_expanded`. Uses the category's label rather
    /// than its `Debug` form so a rename of the enum variant does not silently
    /// collapse everyone's tree.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{}|{}", self.category.label(), self.path)
    }
}

/// Derive the whole tree, in pre-order, for the given categories.
///
/// `categories` fixes the top-level order; `rows` is the output of
/// [`build_rows`]. A category with no rows contributes nothing.
#[must_use]
pub fn build_tree(
    categories: &[ConfigCategory],
    rows: &HashMap<ConfigCategory, Vec<ConfigSetting>>,
) -> Vec<ConfigTreeNode> {
    let mut nodes = Vec::new();
    for category in categories {
        let Some(category_rows) = rows.get(category) else {
            continue;
        };
        if category_rows.is_empty() {
            continue;
        }
        push_category(*category, category_rows, &mut nodes);
    }
    nodes
}

/// Emit a category root plus its subtree.
///
/// The root's path is the longest prefix every row in the category shares, so
/// `[mcp_pool]` (whose rows are all siblings) is one flat node while `[fleet]`
/// (cost / interview / bridge) grows children.
fn push_category(category: ConfigCategory, rows: &[ConfigSetting], out: &mut Vec<ConfigTreeNode>) {
    let parents: Vec<Vec<String>> = rows.iter().map(|row| parent_segments(&row.key)).collect();
    let root = common_prefix(&parents);
    let all: Vec<usize> = (0..rows.len()).collect();

    let root_index = out.len();
    out.push(ConfigTreeNode {
        category,
        path: root.join("."),
        label: category.label().to_string(),
        depth: 0,
        has_children: false,
        rows: all.clone(),
    });

    push_children(category, rows, &parents, &root, &all, 1, out);
    out[root_index].has_children = out.len() > root_index + 1;
}

/// Group the rows that live BELOW `prefix` by their next path segment, emitting
/// one node per group and recursing.
#[allow(clippy::too_many_arguments)]
fn push_children(
    category: ConfigCategory,
    rows: &[ConfigSetting],
    parents: &[Vec<String>],
    prefix: &[String],
    candidates: &[usize],
    depth: usize,
    out: &mut Vec<ConfigTreeNode>,
) {
    if depth > MAX_TREE_DEPTH {
        return;
    }

    // First-seen order, not alphabetical: registry order is the order a reader
    // of config.toml expects, and `build_rows` preserves it.
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &index in candidates {
        let parent = &parents[index];
        if parent.len() <= prefix.len() {
            continue; // A row that sits directly on `prefix`, not below it.
        }
        let segment = parent[prefix.len()].clone();
        if !groups.contains_key(&segment) {
            order.push(segment.clone());
        }
        groups.entry(segment).or_default().push(index);
    }

    for segment in order {
        let members = groups.remove(&segment).unwrap_or_default();
        let mut child_prefix = prefix.to_vec();
        child_prefix.push(segment.clone());

        let index = out.len();
        out.push(ConfigTreeNode {
            category,
            path: child_prefix.join("."),
            label: prettify(&segment),
            depth,
            has_children: false,
            rows: members.clone(),
        });
        push_children(
            category,
            rows,
            parents,
            &child_prefix,
            &members,
            depth + 1,
            out,
        );
        out[index].has_children = out.len() > index + 1;
    }
}

/// The dotted path of the table a key lives in: `fleet.cost.session_usd` →
/// `["fleet", "cost"]`.
fn parent_segments(key: &str) -> Vec<String> {
    let mut segments = parse_dot_key(key);
    segments.pop();
    segments
}

fn common_prefix(paths: &[Vec<String>]) -> Vec<String> {
    let Some((first, rest)) = paths.split_first() else {
        return Vec::new();
    };
    let mut prefix = first.clone();
    for path in rest {
        let shared = prefix.iter().zip(path.iter()).take_while(|(a, b)| a == b).count();
        prefix.truncate(shared);
    }
    prefix
}

/// `image_source` → `Image Source`. Words split on `_` only: a user-chosen map
/// key such as a template named `claude-docker` keeps its own spelling.
fn prettify(segment: &str) -> String {
    segment
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Case-insensitive subsequence match, used by the `/` filter.
///
/// Subsequence rather than substring so `dblclk` finds `double_click_ms`; the
/// caller ranks exact substring hits first, so the loose matches sort below the
/// obvious ones instead of burying them.
#[must_use]
pub fn fuzzy_matches(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut wanted = needle.chars().flat_map(char::to_lowercase).peekable();
    for candidate in haystack.chars().flat_map(char::to_lowercase) {
        match wanted.peek() {
            Some(&next) if next == candidate => {
                wanted.next();
            }
            Some(_) => {}
            None => return true,
        }
    }
    wanted.peek().is_none()
}

/// The keychain service a secret row stores its literal under.
///
/// Derived from the dotted key so it is unique per row, stable across restarts
/// and self-describing in Keychain Access — `ainb-fleet-bridge-telegram-token`
/// says exactly which setting it backs. The `keychain:` resolver
/// ([`crate::fleet::bridge::secrets`]) looks an item up by service name alone,
/// so the service IS the whole address; a shorter scheme would risk two rows
/// colliding on one item.
#[must_use]
pub fn keychain_service(key: &str) -> String {
    format!("ainb-{}", key.replace('.', "-"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    fn seed() -> toml::Value {
        toml::Value::try_from(AppConfig::default()).expect("config serializes")
    }

    #[test]
    fn a_key_without_a_star_survives_even_when_the_leaf_is_absent() {
        let empty = toml::Value::Table(toml::map::Map::new());
        assert_eq!(expand_key("docker.host", &empty), vec!["docker.host"]);
    }

    #[test]
    fn a_star_expands_to_one_path_per_configured_instance() {
        let seed: toml::Value = toml::from_str(
            r#"
            [mcp_servers.context7]
            shared = true
            [mcp_servers.playwright]
            shared = false
            "#,
        )
        .unwrap();
        let mut keys = expand_key("mcp_servers.*.shared", &seed);
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "mcp_servers.context7.shared",
                "mcp_servers.playwright.shared"
            ]
        );
    }

    #[test]
    fn a_star_over_an_absent_map_yields_no_rows() {
        let empty = toml::Value::Table(toml::map::Map::new());
        assert!(expand_key("mcp_servers.*.shared", &empty).is_empty());
    }

    #[test]
    fn nested_stars_expand_together() {
        let seed: toml::Value = toml::from_str(
            r#"
            [container_templates.dev.config.environment]
            NODE_ENV = "development"
            TZ = "UTC"
            "#,
        )
        .unwrap();
        let mut keys = expand_key("container_templates.*.config.environment.*", &seed);
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "container_templates.dev.config.environment.NODE_ENV",
                "container_templates.dev.config.environment.TZ",
            ]
        );
    }

    #[test]
    fn rows_are_seeded_from_the_config_and_keyed_by_concrete_path() {
        let rows = build_rows(&seed());
        let pool = rows.get(&ConfigCategory::McpPool).expect("MCP pool rows");
        let enabled =
            pool.iter().find(|row| row.key == "mcp_pool.enabled").expect("pool enabled row");
        assert!(matches!(enabled.value, ConfigValue::Bool(true)));
        assert!(!enabled.description.is_empty());
    }

    #[test]
    fn the_opaque_plugin_value_table_never_becomes_a_row() {
        let seed: toml::Value = toml::from_str(
            r#"
            [plugins.learnings]
            learnings_dir = "/tmp/kb"
            "#,
        )
        .unwrap();
        let rows = build_rows(&seed);
        let plugin_rows = rows.get(&ConfigCategory::Plugins).cloned().unwrap_or_default();
        assert!(
            plugin_rows.iter().all(|row| !row.key.starts_with("plugins.learnings")),
            "plugin value tables must come from manifests, not the registry: {:?}",
            plugin_rows.iter().map(|r| &r.key).collect::<Vec<_>>()
        );
    }

    #[test]
    fn hidden_leaves_never_become_rows() {
        let rows = build_rows(&seed());
        let keys: Vec<&str> = rows.values().flatten().map(|row| row.key.as_str()).collect();
        for hidden in ["version", "ui_preferences.home_sidebar_width"] {
            assert!(
                !keys.contains(&hidden),
                "hidden leaf '{hidden}' was rendered"
            );
        }
    }

    #[test]
    fn a_flat_section_is_a_single_tree_node() {
        let rows = build_rows(&seed());
        let tree = build_tree(&[ConfigCategory::McpPool], &rows);
        assert_eq!(tree.len(), 1, "{tree:#?}");
        assert_eq!(tree[0].path, "mcp_pool");
        assert!(!tree[0].has_children);
    }

    #[test]
    fn a_nested_section_grows_children_that_mirror_the_toml() {
        let seed: toml::Value = toml::from_str(
            r#"
            [fleet.cost]
            session_usd = 5.0
            [fleet.interview]
            surface = "fleet"
            [fleet.bridge.telegram]
            token = "$TG"
            "#,
        )
        .unwrap();
        let rows = build_rows(&seed);
        let tree = build_tree(&[ConfigCategory::Fleet], &rows);

        let paths: Vec<&str> = tree.iter().map(|node| node.path.as_str()).collect();
        assert_eq!(paths[0], "fleet", "{paths:?}");
        assert!(paths.contains(&"fleet.cost"), "{paths:?}");
        assert!(paths.contains(&"fleet.interview"), "{paths:?}");
        assert!(paths.contains(&"fleet.bridge"), "{paths:?}");
        assert!(paths.contains(&"fleet.bridge.telegram"), "{paths:?}");
        assert!(tree[0].has_children);

        // The root carries every row in the category, so selecting it is
        // "show me the whole section".
        let fleet_rows = rows.get(&ConfigCategory::Fleet).unwrap();
        assert_eq!(tree[0].rows.len(), fleet_rows.len());

        // A child carries only its own subtree.
        let bridge = tree.iter().find(|node| node.path == "fleet.bridge").unwrap();
        assert!(bridge.rows.iter().all(|&i| fleet_rows[i].key.starts_with("fleet.bridge.")));
    }

    /// #9. An unset `preferred_editor` must render as unset, not as whichever
    /// editor happens to be installed first.
    #[test]
    fn an_unset_editor_preference_does_not_preselect_one() {
        let rows = build_rows(&seed());
        let editor = rows
            .get(&ConfigCategory::Editor)
            .and_then(|rows| rows.iter().find(|row| row.key == "ui_preferences.preferred_editor"))
            .expect("editor row");
        assert_eq!(
            editor.value.display(),
            "",
            "an unset editor preference was rendered as a real choice"
        );
        assert_eq!(
            editor.value.raw(),
            "",
            "confirming the row would write that choice"
        );
    }

    #[test]
    fn usage_rows_declare_themselves_read_only() {
        assert!(read_only_reason("usage.plan.id").is_some());
        assert!(read_only_reason("usage.currency.code").is_some());
        assert!(read_only_reason("mcp_pool.enabled").is_none());
    }

    #[test]
    fn structured_rows_declare_themselves_read_only() {
        assert!(read_only_reason("mcp_servers.ctx.definition.config").is_some());
    }

    #[test]
    fn fuzzy_matches_a_subsequence_and_rejects_a_miss() {
        assert!(fuzzy_matches("mcp_pool.idle_grace_secs", "idlegrace"));
        assert!(fuzzy_matches("Shared MCP Pool", "mcp"));
        assert!(!fuzzy_matches("docker.timeout", "zzz"));
    }

    #[test]
    fn the_keychain_service_is_derived_from_the_key() {
        assert_eq!(
            keychain_service("fleet.bridge.telegram.token"),
            "ainb-fleet-bridge-telegram-token"
        );
    }
}
