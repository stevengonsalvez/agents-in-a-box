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
use ainb_hangar_store::repo::fleet::FleetRepo;

use crate::cli::OutputFormat;

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
            println!("session_key,provider,cwd,lifecycle_state,last_observed_at");
            for row in &rows {
                println!(
                    "{},{},{},{},{}",
                    row.session_key,
                    row.provider,
                    row.cwd,
                    row.lifecycle_state,
                    row.last_observed_at
                );
            }
        }
        OutputFormat::Markdown => {
            println!("| session | provider | cwd | state | last seen |");
            println!("|---|---|---|---|---|");
            for row in &rows {
                println!(
                    "| {} | {} | {} | {} | {} |",
                    row.session_key,
                    row.provider,
                    row.cwd,
                    row.lifecycle_state,
                    row.last_observed_at
                );
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
