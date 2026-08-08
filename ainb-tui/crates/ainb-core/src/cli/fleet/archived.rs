// ABOUTME: `ainb fleet archived` — the retired half of the Fleet roster.
//
// The daemon demotes long-dead sessions to `visible = 0` so the 3s reconciler
// stops scanning them (1,440 of 1,472 visible rows on a measured profile were
// dead). Demoted is NOT deleted: the row keeps its key, its history, and its
// direct-lookup path. This is the browse path that makes that true in practice.
//
// Reads the Hangar database directly rather than going through the daemon RPC,
// matching `ainb hangar inbox list`. The archived roster is cold, operator-
// facing data with no live-stream semantics, so a read-only open is the whole
// requirement — and it keeps working when the daemon is down, which is exactly
// when someone goes looking for a session that vanished.

use anyhow::{Context as _, Result};

use ainb_hangar_store::Store;
use ainb_hangar_store::repo::fleet::{FleetRepo, FleetSessionRow};

use crate::cli::OutputFormat;
// The repo's own RFC-4180 / markdown escapers, shared rather than re-derived:
// a `cwd` with a comma or a `display_name` with a pipe is ordinary, not exotic,
// and hand-rolled interpolation corrupts the row silently.
use crate::cli::hangar::{csv_field, md_cell};

const CSV_HEADER: &str = "session_key,provider,cwd,display_name,lifecycle_state,last_observed_at";

/// One RFC-4180 row. Split out from the printing so the escaping is testable —
/// a `cwd` with a comma is ordinary, and a corrupted row is silent otherwise.
fn csv_row(row: &FleetSessionRow) -> String {
    format!(
        "{},{},{},{},{},{}",
        csv_field(&row.session_key),
        csv_field(&row.provider),
        csv_field(&row.cwd),
        csv_field(row.display_name.as_deref().unwrap_or_default()),
        csv_field(&row.lifecycle_state),
        row.last_observed_at
    )
}

/// One markdown table row, pipes escaped.
fn md_row(row: &FleetSessionRow) -> String {
    format!(
        "| {} | {} | {} | {} | {} |",
        md_cell(&row.session_key),
        md_cell(&row.provider),
        md_cell(&row.cwd),
        md_cell(&row.lifecycle_state),
        row.last_observed_at
    )
}

pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let limit = matches.get_one::<i64>("limit").copied().unwrap_or(50).max(1);
    let store = Store::open_default().await.context("open hangar database")?;
    let rows = FleetRepo::list_archived(store.pool(), limit)
        .await
        .context("read archived fleet sessions")?;

    match format {
        OutputFormat::Json => {
            let wire: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "session_key": row.session_key,
                        "provider": row.provider,
                        "cwd": row.cwd,
                        "display_name": row.display_name,
                        "lifecycle_state": row.lifecycle_state,
                        "last_observed_at": row.last_observed_at,
                        "version": row.version,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string(&wire).unwrap_or_else(|_| "[]".to_string())
            );
        }
        OutputFormat::Csv => {
            println!("{CSV_HEADER}");
            for row in &rows {
                println!("{}", csv_row(row));
            }
        }
        OutputFormat::Markdown => {
            println!("| session | provider | cwd | state | last seen |");
            println!("|---|---|---|---|---|");
            for row in &rows {
                println!("{}", md_row(row));
            }
        }
        OutputFormat::Text => {
            if rows.is_empty() {
                println!("ainb fleet archived — nothing retired yet.");
                return Ok(());
            }
            println!("ainb fleet archived — {} retired session(s)", rows.len());
            for row in &rows {
                let label = row.display_name.as_deref().unwrap_or(&row.session_key);
                println!(
                    "▸ {label} ─ {} · {} · {}",
                    row.provider, row.lifecycle_state, row.cwd
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session whose paths and label carry the delimiters of both formats.
    /// None of these are exotic: a worktree name with a comma, a branch label
    /// with a pipe, a quoted directory. Interpolating them raw silently shifts
    /// every later column.
    fn hostile_row() -> FleetSessionRow {
        FleetSessionRow {
            session_key: "claude:a,b".to_string(),
            provider: "claude".to_string(),
            provider_session_id: None,
            tmux_target: None,
            process_start_fingerprint: None,
            cwd: "/repo/feat,\"x\"|y".to_string(),
            display_name: Some("fix: a, b | c".to_string()),
            lifecycle_state: "EXITED".to_string(),
            active_work_count: 0,
            workload_updated_at: 0,
            workload_authority: "inferred".to_string(),
            attention_state: "NONE".to_string(),
            current_request_fingerprint: None,
            management_state: "DEGRADED".to_string(),
            transport_health: "UNAVAILABLE".to_string(),
            capabilities: "{}".to_string(),
            provenance: "authoritative".to_string(),
            confidence: "LOW".to_string(),
            discovered_at: 1,
            last_observed_at: 42,
            metadata_updated_at: 0,
            metadata_authority: "inferred".to_string(),
            lifecycle_updated_at: 0,
            lifecycle_authority: "inferred".to_string(),
            attention_updated_at: 0,
            attention_authority: "inferred".to_string(),
            transport_updated_at: 0,
            transport_authority: "inferred".to_string(),
            version: 1,
            updated_revision: 0,
        }
    }

    #[test]
    fn csv_quotes_every_field_that_carries_a_delimiter() {
        let line = csv_row(&hostile_row());

        assert_eq!(
            line, "\"claude:a,b\",claude,\"/repo/feat,\"\"x\"\"|y\",\"fix: a, b | c\",EXITED,42",
            "commas and quotes must be RFC-4180 escaped, not interpolated raw"
        );
        assert!(
            line.split(',').count() > CSV_HEADER.split(',').count(),
            "sanity: the raw text really does contain embedded commas"
        );
    }

    #[test]
    fn markdown_escapes_the_pipe_that_would_split_the_row() {
        let line = md_row(&hostile_row());

        assert!(
            !line.replace("\\|", "").contains("|y"),
            "an unescaped pipe in a cwd would open a phantom column: {line}"
        );
        assert!(
            line.contains("/repo/feat,\"x\"\\|y"),
            "the pipe is escaped in place: {line}"
        );
    }
}
