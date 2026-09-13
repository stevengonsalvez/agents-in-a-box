// ABOUTME: Renderer-agnostic half of the `session_tabs` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::session_tabs`, which re-exports this module.

/// One row of the `log` tab: a notification this session produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    /// Epoch-ms the hook fired.
    pub ts: i64,
    /// The raw hook event name, as the agent named it.
    pub event: String,
    /// A one-line summary, or empty.
    pub detail: String,
}

/// Keep one session's rows out of a batch the store already returned.
///
/// PURE, and deliberately not a store read: the read runs on
/// [`crate::fleet::session_log`]'s worker thread, because doing it here — which
/// is inside `terminal.draw` — is what made the `log` tab cost a second a
/// frame. This is the half that has to happen for whatever the worker fetched.
#[must_use]
pub fn log_rows(
    records: &[ainb_plugin_notifyd::NotificationRecord],
    cwd: &str,
    agent: Option<&str>,
    limit: usize,
) -> Vec<LogRow> {
    let cwd = cwd.trim_end_matches('/');
    records
        .iter()
        .filter(|row| {
            row.cwd.trim_end_matches('/') == cwd && agent.is_none_or(|agent| row.agent == agent)
        })
        .take(limit)
        .map(|row| LogRow {
            ts: row.ts,
            event: row.raw_event.clone(),
            detail: log_detail(row),
        })
        .collect()
}

/// The one-line summary for a log row: the hook's own message when it sent one,
/// else the project it fired in.
fn log_detail(row: &ainb_plugin_notifyd::NotificationRecord) -> String {
    serde_json::from_str::<serde_json::Value>(&row.payload_json)
        .ok()
        .and_then(|payload| {
            payload
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(|message| message.trim().to_string())
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| row.project.clone())
}
