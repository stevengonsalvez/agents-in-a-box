//! multica-gap #10 — Faceted-filter panel render golden.
//!
//! Drives the REAL `render_issue_list` with the `f` panel open over the seeded
//! 5-row matrix and asserts the panel's checkbox + drill-down-count layout: the
//! `⛃ Filters` title, and the per-value counts across facets in the EXACT
//! `<value> (<n>)` form the tmux tripwire asserts (`bug (3)`, `chore (1)`,
//! `P0 (3)`, `Todo (3)`). A substring-OR would pass while broken (the
//! `tmux-ui-tripwire` trap), so each string is checked exactly.
//!
//! A second case proves the closed-panel active-facet summary chip renders once a
//! facet is applied (fail-visible: the board reads as filtered without the panel).

use ainb_hangar_core::ids::IssueId;
use ainb_hangar_proto::events::IssueRow;
use ainb_hangar_proto::lifecycle::IssueLifecycle;
use ainb_plugin_hangar::IssueListState;
use ainb_plugin_hangar::screen::issue_list::{
    FacetKind, FacetValue, IssueListEvent, reduce_issue_list, render_issue_list,
};
use ainb_plugin_sdk::WireBuffer;

/// A wire row with explicit state / priority / labels for facet rendering.
fn row(id: &str, state: &str, priority: i64, labels: &[&str]) -> IssueRow {
    IssueRow {
        subscriber_count: 0,
        subscribed: false,
        reactions: Vec::new(),
        properties: Vec::new(),
        metadata: Vec::new(),
        last_dispatch_reason: None,
        last_dispatch_detail: None,
        last_dispatch_at: None,
        origin_type: None,
        origin_id: None,
        id: IssueId::from_str(id).unwrap(),
        display_id: Some(id.to_uppercase()),
        workspace_id: "default".into(),
        title: format!("{id} title"),
        description: None,
        state: state.into(),
        assignee: Some("agent:claude".into()),
        creator: "member:alice".into(),
        created_at: 1_700_000_000_000,
        priority,
        due_date: None,
        labels: labels.iter().map(|l| (*l).to_string()).collect(),
        pr_url: None,
        branch: None,
        repo_ref: None,
        agent: None,
        source_branch: None,
        target_branch: None,
        external_ref: None,
        run_count: 0,
        last_run_status: None,
        last_run_at: None,
        parent_id: None,
        child_total: 0,
        child_done: 0,
        acceptance_criteria: Vec::new(),
        acceptance: Vec::new(),
        context_refs: Vec::new(),
        dependencies: Vec::new(),
    }
}

/// The discriminating gap-10 matrix: one `todo + P0 + bug` survivor + four decoys.
fn matrix() -> IssueListState {
    IssueListState::with_rows(vec![
        row("target", "todo", 3, &["bug"]),
        row("d_nolbl", "todo", 3, &[]),
        row("d_p2bug", "todo", 1, &["bug"]),
        row("d_progbug", "in_progress", 3, &["bug"]),
        row("d_chore", "done", 2, &["chore"]),
    ])
}

/// Flatten the buffer into a `\n`-joined glyph map (first char of each cell).
fn glyph_map(buf: &WireBuffer, cols: u16) -> String {
    let mut grid = vec![vec![' '; cols as usize]; buf.height as usize];
    for (coord, cell) in &buf.cells {
        if coord.y < buf.height && coord.x < cols {
            if let Some(ch) = cell.symbol.chars().next() {
                grid[coord.y as usize][coord.x as usize] = ch;
            }
        }
    }
    grid.into_iter()
        .map(|r| r.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The open panel shows the `⛃ Filters` title and every facet's per-value
/// drill-down counts (all facets empty = all-in-scope), in the exact `(<n>)` form.
#[test]
fn facet_panel_renders_titles_and_per_value_counts() {
    let state = reduce_issue_list(&matrix(), IssueListEvent::Key('f')).state;
    let mut buf = WireBuffer::new(120, 40);
    render_issue_list(&mut buf, 120, 1, 39, &state, 0);
    let full = glyph_map(&buf, 120);

    assert!(full.contains("Filters"), "panel title missing:\n{full}");
    // Label counts (no facet applied → over all rows).
    assert!(full.contains("bug (3)"), "bug (3) missing:\n{full}");
    assert!(full.contains("chore (1)"), "chore (1) missing:\n{full}");
    // Priority count (P0 = the 3 P0 rows).
    assert!(full.contains("P0 (3)"), "P0 (3) missing:\n{full}");
    // Status count (Todo = the 3 todo rows).
    assert!(full.contains("Todo (3)"), "Todo (3) missing:\n{full}");
    // The checkbox affordance is present (nothing selected yet).
    assert!(full.contains("[ ]"), "empty checkbox missing:\n{full}");
}

/// After toggling status=todo + priority=P0 + label=bug the panel shows the
/// checked boxes AND the drill-down label count collapses to `bug (1)` (only the
/// target remains in the todo+P0 scope; chore is zero-count → omitted).
#[test]
fn facet_panel_reflects_selection_and_drilldown() {
    let mut state = reduce_issue_list(&matrix(), IssueListEvent::Key('f')).state;
    for value in [
        FacetValue::Status(IssueLifecycle::Todo),
        FacetValue::Priority(3),
        FacetValue::Label("bug".to_string()),
    ] {
        state = reduce_issue_list(&state, IssueListEvent::ToggleFacet(value.kind(), value)).state;
    }
    let mut buf = WireBuffer::new(120, 40);
    render_issue_list(&mut buf, 120, 1, 39, &state, 0);
    let full = glyph_map(&buf, 120);

    // A checked box is now painted.
    assert!(full.contains("[x]"), "checked box missing:\n{full}");
    // Drill-down label count in the todo+P0 scope: bug (1), no chore.
    assert!(
        full.contains("bug (1)"),
        "drill-down bug (1) missing:\n{full}"
    );
    assert!(
        !full.contains("chore"),
        "chore should be zero-count/omitted:\n{full}"
    );
}

/// With a facet applied the CLOSED-panel chip row carries the gold active-facet
/// summary (fail-visible).
#[test]
fn active_facet_summary_chip_renders_when_panel_closed() {
    let state = reduce_issue_list(
        &matrix(),
        IssueListEvent::ToggleFacet(FacetKind::Status, FacetValue::Status(IssueLifecycle::Todo)),
    )
    .state;
    let mut buf = WireBuffer::new(120, 40);
    render_issue_list(&mut buf, 120, 1, 39, &state, 0);
    let full = glyph_map(&buf, 120);

    assert!(
        full.contains("status:todo"),
        "summary chip missing:\n{full}"
    );
}
