// ABOUTME: Shared test fixtures. The model and git fixtures live in
// `ainb_app::test_support`; this module re-exports them and adds the one
// fixture that needs the CLI's usage report, which stays in this crate.

pub use ainb_app::test_support::*;

use crate::models::UsageData;

/// Public wrapper around the `cli::usage::report_json` helper. Exposed
/// here (rather than making the CLI fn `pub`) so integration tests in
/// `tests/` can capture the canonical JSON shape without leaking module
/// internals from `cli::usage`.
// TODO(post-6e): wired by cli_burndown fixture tests; once those
// re-enable post-6e the lib-build warning evaporates on its own. The
// function survives Phase 6d because `tripwire.rs` still uses it as the
// in-tree byte-identity oracle for the plugin's CLI output.
#[allow(dead_code)]
pub fn cli_usage_report_json(data: &UsageData) -> serde_json::Value {
    crate::cli::usage::report_json(data)
}
