// ABOUTME: `ainb fleet approve|deny [session-id]` — CLI lever for the
// synchronous permission round-trip.
//
// The blocked `PermissionRequest` hook is parked on the notifyd approve
// socket in `client_await`; this verb delivers the human's decision via
// `client_decide`, which flows back to Claude as its `hookSpecificOutput`
// permission decision — the same broker path the TUI fleet-panel lever
// uses, so the two surfaces can never diverge.
//
// With no session-id, both verbs list the sessions currently waiting on a
// decision (discovery for scripting: `ainb fleet approve --format json`).

use anyhow::{Context, Result};

use crate::cli::OutputFormat;
use ainb_plugin_notifyd::broker::{DecisionKind, client_decide, client_list};

/// Replace control characters (terminal-escape injection vector — these
/// strings are registered by whoever dialled the socket) with spaces.
fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

pub async fn execute(
    matches: &clap::ArgMatches,
    format: OutputFormat,
    kind: DecisionKind,
) -> Result<()> {
    let session_id = matches.get_one::<String>("session-id").cloned();
    let reason = matches.get_one::<String>("reason").cloned().unwrap_or_default();
    // Keep the JSON `decision` vocabulary identical to the broker wire enum
    // (`approve`/`deny`) so scripts see one vocabulary end to end.
    let (verb, wire) = match kind {
        DecisionKind::Approve => ("approved", "approve"),
        _ => ("denied", "deny"),
    };

    // Broker clients are blocking unix I/O — keep them off the async reactor.
    let sock = ainb_plugin_notifyd::paths::Paths::from_home()?.approve_socket;
    match session_id {
        Some(session_id) => {
            let matched = tokio::task::spawn_blocking({
                let (sock, session_id) = (sock.clone(), session_id.clone());
                move || client_decide(&sock, &session_id, kind, &reason)
            })
            .await?
            .with_context(|| {
                format!(
                    "approve broker unreachable at {} — repair with `ainb notifyd restart`",
                    sock.display()
                )
            })?;
            if matches!(format, OutputFormat::Json) {
                println!(
                    "{}",
                    serde_json::json!({
                        "session_id": session_id,
                        "decision": wire,
                        "matched": matched,
                    })
                );
            } else if matched {
                // "matched" not "delivered": the broker handed the decision to a
                // parked waiter, but the hook-side write isn't acknowledged.
                println!("{verb} → {session_id}: matched the waiting hook");
            } else {
                println!("{verb} → {session_id}: no waiter (already resolved or timed out)");
            }
            // A miss is an actionable failure for scripts: exit non-zero.
            if !matched {
                std::process::exit(1);
            }
        }
        None => {
            let pending = tokio::task::spawn_blocking({
                let sock = sock.clone();
                move || client_list(&sock)
            })
            .await?
            .with_context(|| {
                format!(
                    "approve broker unreachable at {} — repair with `ainb notifyd restart`",
                    sock.display()
                )
            })?;
            if matches!(format, OutputFormat::Json) {
                println!("{}", serde_json::to_string_pretty(&pending)?);
            } else if pending.is_empty() {
                println!("no sessions waiting on a permission decision");
            } else {
                println!("{:<38} {:<18} {:<8} CONTEXT", "SESSION", "TOOL", "WAITING");
                for p in pending {
                    // AWAIT fields come from whatever dialled the socket — strip
                    // control chars so a hostile registration can't inject
                    // terminal escapes into the operator's shell.
                    println!(
                        "{:<38} {:<18} {:<8} {}",
                        sanitize(&p.session_id),
                        sanitize(&p.tool),
                        format!("{}s", p.waiting_ms / 1000),
                        sanitize(&p.context),
                    );
                }
            }
        }
    }
    Ok(())
}
