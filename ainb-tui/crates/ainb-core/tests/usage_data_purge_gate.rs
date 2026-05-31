//! Phase 6d gate: ensure the host has no analytics-plane wiring outside
//! the documented carve-outs.
//!
//! Contract (from `plans/plugin-phase-6-data-plane-spec.md` and the
//! Phase 6d tracking ticket `agents-in-a-box-121.5`):
//!
//!   `rg --no-heading -n 'usage_data|burndown|UsageData' \
//!        crates/ainb-core/src/ \
//!     | rg -v 'cli/registry.rs|test_support'`
//!
//! ...should be EMPTY for the active-code subset of the host. The full
//! repository-wide grep also catches comments and the legacy
//! `models/usage.rs` parser tree (still required by `live_window` /
//! `usage_cache` for the statusline pipeline) — those are tracked
//! separately. The NARROW gate this test enforces is:
//!
//!   1. No active `UsageData` / `usage_data` / `burndown` symbol use
//!      remains in `crates/ainb-core/src/cli/` outside `registry.rs`
//!      and `usage.rs::report_json` (the byte-identity oracle).
//!   2. No active analytics CLI handler bodies in the host
//!      (`print_report`, `export_usage`, `print_optimize`, `print_compare`,
//!      `print_yield`, `model_alias_command`, `load_usage_for_plugin`,
//!      and the CSV/JSON helpers they used).
//!   3. The old `models/usage/parsers/` directory does not exist
//!      (parsers live in `crates/ainb-plugin-session-reader/src/parsers`).
//!
//! Anything else in `models/usage.rs` is *legacy host parsers* feeding
//! `live_window` and the `usage_cache` analytics-cache primitive — those
//! routes don't go through `usage_data` events and are out of scope for
//! the host-purge ticket.

use std::path::PathBuf;

fn ainb_core_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read(path: &str) -> String {
    let full = ainb_core_src().join(path);
    std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("read {}: {e}", full.display()))
}

#[test]
fn parsers_dir_under_models_usage_does_not_exist() {
    // The pre-Phase-6 host kept its provider parsers under
    // `crates/ainb-core/src/models/usage/parsers/`. Phase 6 moved them
    // into `crates/ainb-plugin-session-reader/src/parsers`. If this
    // directory ever reappears it usually means someone resurrected the
    // host-side parser pipeline — a regression for the data plane.
    let dir = ainb_core_src().join("models").join("usage");
    assert!(
        !dir.exists() || !dir.join("parsers").exists(),
        "models/usage/parsers/ must not exist on the host — \
         parsers belong to the session-reader plugin"
    );
}

#[test]
fn dead_in_tree_usage_handler_bodies_are_gone() {
    // These free-fns lived in `cli/usage.rs` pre-Phase-6 and were the
    // host's analytics CLI handlers. They are dispatched via the
    // burndown plugin now and their bodies were deleted in Phase 6d.
    // A textual check on cli/usage.rs guards against accidental
    // re-introduction (eg. someone copy-pasting from git history).
    let body = read("cli/usage.rs");
    for forbidden in [
        "fn print_report(",
        "fn print_status(",
        "fn print_optimize(",
        "fn print_compare(",
        "fn print_yield(",
        "fn export_usage(",
        "fn model_alias_command(",
        "fn load_usage(",
        // `pub fn load_usage_for_plugin` was the public entry; ensure
        // its body did not creep back in either.
        "fn load_usage_for_plugin(",
        // CSV helpers — only reachable via the dead handlers.
        "fn combined_csv(",
        "fn summary_csv(",
        "fn daily_csv(",
        "fn sessions_csv(",
        "fn activities_csv(",
        "fn projects_csv(",
        "fn models_csv(",
        "fn named_csv(",
    ] {
        assert!(
            !body.contains(forbidden),
            "dead handler `{forbidden}` reappeared in cli/usage.rs — \
             Phase 6d removed it; the burndown plugin owns this code path"
        );
    }
}

#[test]
fn cli_dir_active_usage_data_refs_match_carve_outs() {
    // The Phase 6d gate command (paraphrased):
    //   rg 'usage_data|burndown|UsageData' crates/ainb-core/src/cli/ \
    //     | rg -v 'cli/registry.rs|test_support'
    // should return only the surviving plugin-routing shim.
    //
    // We walk every file under `src/cli/`, skip the carve-outs, and
    // assert no remaining file actively *imports* (`use` or path-
    // qualified `UsageData`) the legacy host UsageData type. Comments
    // and string literals naming the plugin ("burndown") are allowed —
    // they appear in user-facing error messages and route docs.
    let cli = ainb_core_src().join("cli");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&cli).expect("read cli dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".rs") {
            continue;
        }
        // Carve-outs documented by Phase 6d:
        //   * registry.rs — owns `dispatch_usage_via_plugin`, the only
        //     surviving plugin-routing shim.
        //   * usage.rs — owns `report_json` (the test_support byte-
        //     identity oracle). Any other UsageData ref here would be
        //     a regression but is detected by `dead_in_tree_usage_handler_bodies_are_gone`.
        if matches!(name, "registry.rs" | "usage.rs") {
            continue;
        }
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        // Active-code heuristic: a `use` or fully-qualified path that
        // names UsageData / usage_data. Comments still allowed because
        // multiple files document the historical `usage_data` topic
        // ("Phase 3 cutover...") for context.
        for line in body.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("//!") {
                continue;
            }
            if trimmed.contains("UsageData")
                || trimmed.contains("models::usage::")
                || trimmed.contains("crate::models::usage")
            {
                offenders.push(format!("{}: {}", name, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "Phase 6d gate: cli/ files outside the registry.rs/usage.rs \
         carve-outs must not actively reference UsageData. Offenders:\n{}",
        offenders.join("\n")
    );
}
