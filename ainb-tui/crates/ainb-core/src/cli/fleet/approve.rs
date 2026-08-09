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
// That listing is the only enumeration surface for the pending queue, so it
// carries everything needed to act: which worktree, which tool, the tool
// input, and how long it has been parked. `--full` prints the untruncated
// tool input plus the absolute cwd.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::cli::OutputFormat;
use ainb_plugin_notifyd::broker::{DecisionKind, client_decide, client_list};

/// Replace control characters (terminal-escape injection vector — these
/// strings are registered by whoever dialled the socket) with spaces.
fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// One waiting request, enriched with the identity the broker never sees.
///
/// The broker keys on the provider's own session uuid (Claude forwards it from
/// the hook payload), which appears in no other fleet listing, so a bare row is
/// unactionable: the operator cannot tell WHICH agent in WHICH repo is blocked.
/// `cwd` / `workspace` are joined in from notifyd's materialised state table.
#[derive(serde::Serialize)]
struct PendingRow {
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<String>,
    tool: String,
    /// Raw tool input, exactly as the broker holds it (never truncated here).
    tool_input: String,
    waiting_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    questions: Vec<serde_json::Value>,
}

/// Provider session uuid to cwd, from notifyd's materialised state table.
///
/// Read-only and best effort: a missing, locked or unmigrated db degrades to an
/// empty map, because an un-joined listing still beats no listing. When one
/// session id carries several cwd rows (the table's PK is the pair), the newest
/// `last_event_ts` wins.
fn cwd_by_session(db: &Path) -> HashMap<String, (String, i64)> {
    let mut out: HashMap<String, (String, i64)> = HashMap::new();
    let Ok(store) = ainb_plugin_notifyd::store::Store::open_readonly(db) else {
        return out;
    };
    for row in store.list_current_state().unwrap_or_default() {
        out.entry(row.session_id)
            .and_modify(|slot| {
                if row.last_event_ts > slot.1 {
                    *slot = (row.cwd.clone(), row.last_event_ts);
                }
            })
            .or_insert((row.cwd, row.last_event_ts));
    }
    out
}

/// Last path segment of a worktree: the label an operator actually recognises.
fn workspace_of(cwd: &str) -> Option<String> {
    Path::new(cwd).file_name().map(|n| n.to_string_lossy().into_owned())
}

/// One-line preview of a field. Full JSON goes to `--format json` (and to
/// `--full`), never truncated there, so scripts keep the exact bytes.
fn preview(s: &str, width: usize) -> String {
    let s = sanitize(s);
    if s.chars().count() <= width {
        return s;
    }
    let head: String = s.chars().take(width.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Squeeze the MIDDLE out of an over-long label, keeping both ends.
///
/// Worktree names are `<repo>--<branch>--<hash>`: the repo is at the head and
/// the only thing that distinguishes two worktrees of the same repo is at the
/// tail, so a head-only truncation renders sibling worktrees identical, which
/// is the exact ambiguity this listing exists to remove.
fn elide_middle(s: &str, width: usize) -> String {
    let s = sanitize(s);
    let len = s.chars().count();
    if len <= width || width < 3 {
        return preview(&s, width);
    }
    let keep = width - 1;
    let head_len = keep.div_ceil(2);
    let tail_len = keep - head_len;
    let head: String = s.chars().take(head_len).collect();
    let tail: String = s.chars().skip(len - tail_len).collect();
    format!("{head}…{tail}")
}

/// Join the broker's raw pending list against the cwd index and order it.
///
/// Pure on purpose: this is the whole enrichment contract (which uuid gets
/// which worktree, what the JSON keys are, what order an operator reads), so it
/// is unit-tested directly rather than only through a socket-driven end-to-end.
fn rows_from(
    pending: Vec<ainb_plugin_notifyd::broker::PendingInfo>,
    cwds: &HashMap<String, (String, i64)>,
) -> Vec<PendingRow> {
    let mut rows: Vec<PendingRow> = pending
        .into_iter()
        .map(|p| {
            let cwd = cwds.get(&p.session_id).map(|(cwd, _)| cwd.clone());
            PendingRow {
                workspace: cwd.as_deref().and_then(workspace_of),
                cwd,
                session_id: p.session_id,
                tool: p.tool,
                tool_input: p.context,
                waiting_secs: p.waiting_ms / 1000,
                request_fingerprint: p.request_fingerprint,
                questions: p.questions,
            }
        })
        .collect();
    // Longest wait first: the operator's queue is by age, not by hash order.
    rows.sort_by_key(|r| std::cmp::Reverse(r.waiting_secs));
    rows
}

/// The human-readable listing, as a string.
///
/// Returns rather than prints so the exact operator-visible bytes (column
/// header, the both-ends worktree elision, the `--full` escape hatch, the
/// trailing decide hint) are assertable without a subprocess.
fn render_text(rows: &[PendingRow], full: bool) -> String {
    use std::fmt::Write as _;

    if rows.is_empty() {
        return "no sessions waiting on a permission decision\n".to_string();
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<38} {:<34} {:<18} {:<8} REQUEST",
        "SESSION", "WORKSPACE", "TOOL", "WAITING"
    );
    for r in rows {
        // Every field below came off a socket or a db written by whoever
        // dialled it, so sanitize before it reaches the operator's terminal.
        // `--full` is the escape hatch: nothing is shortened there.
        let workspace = r.workspace.as_deref().unwrap_or("(unknown worktree)");
        let _ = writeln!(
            out,
            "{:<38} {:<34} {:<18} {:<8} {}",
            preview(&r.session_id, 38),
            if full {
                sanitize(workspace)
            } else {
                elide_middle(workspace, 34)
            },
            preview(&r.tool, 18),
            format!("{}s", r.waiting_secs),
            if full {
                sanitize(&r.tool_input)
            } else {
                preview(&r.tool_input, 80)
            },
        );
        if full {
            if let Some(cwd) = &r.cwd {
                let _ = writeln!(out, "  cwd: {}", sanitize(cwd));
            }
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "decide: ainb fleet approve <session> | ainb fleet deny <session> --reason \"...\""
    );
    out
}

/// Render every request currently parked on the approve broker.
///
/// Reached by `ainb fleet approve` / `ainb fleet deny` with no session-id, and
/// pointed at by name from the `approve broker` row of `ainb fleet daemons`.
async fn list(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let full = matches.get_flag("full");
    let paths = ainb_plugin_notifyd::paths::Paths::from_home()?;
    let sock = paths.approve_socket.clone();
    let pending = tokio::task::spawn_blocking({
        let sock = sock.clone();
        move || client_list(&sock)
    })
    .await?
    .with_context(|| {
        format!(
            "approve broker unreachable at {} (repair with `ainb notifyd restart`)",
            sock.display()
        )
    })?;

    // The join is best effort and must never fail the listing.
    let db = paths.db.clone();
    let cwds = tokio::task::spawn_blocking(move || cwd_by_session(&db)).await?;

    let rows = rows_from(pending, &cwds);

    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    print!("{}", render_text(&rows, full));
    Ok(())
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
        // Both verbs share one listing so the two surfaces can never diverge.
        None => list(matches, format).await?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The pending-listing contract, asserted where CI actually runs it.
    //!
    //! These live in-source deliberately. The `Test` job builds every crate's
    //! `#[cfg(test)]` modules (`cargo nextest run --workspace --lib --tests
    //! --all-features -E 'not binary(/^tripwire_/)'`), so a module here is
    //! reachable by the command that claims to run the suite. The same
    //! assertions expressed only in a `tripwire_*` integration binary were
    //! excluded by that filter and globbed by no script, i.e. never executed.
    //!
    //! Everything below is pure: the join, the JSON shape, the column render.
    //! The socket-driven proof that a REAL parked waiter reaches this code path
    //! lives in `tests/fleet_pending_listing.rs`.

    use super::{PendingRow, elide_middle, preview, render_text, rows_from, workspace_of};
    use ainb_plugin_notifyd::broker::PendingInfo;
    use std::collections::HashMap;

    const WORKTREE: &str = "agents-in-a-box--pending-listing-probe--deadbeef";
    const TOOL_INPUT: &str =
        r#"{"command":"rm -rf build/ && cargo build --release","description":"probe"}"#;

    fn pending(session_id: &str, waiting_ms: u64) -> PendingInfo {
        PendingInfo {
            session_id: session_id.to_string(),
            tool: "Bash".to_string(),
            context: TOOL_INPUT.to_string(),
            request_fingerprint: None,
            questions: Vec::new(),
            waiting_ms,
        }
    }

    fn cwd_index(session_id: &str, cwd: &str) -> HashMap<String, (String, i64)> {
        let mut m = HashMap::new();
        m.insert(session_id.to_string(), (cwd.to_string(), 1));
        m
    }

    /// The join is the whole point: a bare provider uuid names no repo, so a
    /// row without the worktree is not actionable.
    #[test]
    fn a_row_is_joined_to_the_worktree_the_uuid_alone_never_names() {
        let cwd = format!("/tmp/worktrees/by-name/{WORKTREE}");
        let rows = rows_from(vec![pending("uuid-a", 4_200)], &cwd_index("uuid-a", &cwd));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].workspace.as_deref(), Some(WORKTREE));
        assert_eq!(rows[0].cwd.as_deref(), Some(cwd.as_str()));
        // Milliseconds are the broker's unit; the operator reads whole seconds.
        assert_eq!(rows[0].waiting_secs, 4);
    }

    /// A session with no state row still lists: an un-joined row beats no row.
    #[test]
    fn a_row_with_no_state_row_still_lists_without_a_workspace() {
        let rows = rows_from(vec![pending("uuid-orphan", 1_000)], &HashMap::new());
        assert_eq!(rows.len(), 1);
        assert!(rows[0].workspace.is_none());
        assert!(rows[0].cwd.is_none());
    }

    /// JSON is the scripting surface: stable keys, and the tool input verbatim.
    #[test]
    fn json_carries_the_documented_keys_and_never_truncates_the_tool_input() {
        let cwd = format!("/tmp/worktrees/by-name/{WORKTREE}");
        let rows = rows_from(vec![pending("uuid-a", 4_200)], &cwd_index("uuid-a", &cwd));
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&rows).expect("serialize")).expect("parse");
        let row = &json[0];
        assert_eq!(row["session_id"], "uuid-a");
        assert_eq!(row["workspace"], WORKTREE);
        assert_eq!(row["cwd"], cwd);
        assert_eq!(row["tool"], "Bash");
        assert_eq!(row["tool_input"], TOOL_INPUT, "JSON must keep exact bytes");
        assert_eq!(row["waiting_secs"], 4);
        // Absent optionals are omitted, not rendered as nulls a script must guard.
        assert!(row.get("request_fingerprint").is_none(), "row: {row}");
        assert!(row.get("questions").is_none(), "row: {row}");
    }

    /// The operator's queue is by age. Hash order would bury the oldest block.
    #[test]
    fn rows_are_ordered_longest_wait_first() {
        let rows = rows_from(
            vec![
                pending("uuid-young", 1_000),
                pending("uuid-old", 90_000),
                pending("uuid-mid", 30_000),
            ],
            &HashMap::new(),
        );
        let ids: Vec<&str> = rows.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(ids, ["uuid-old", "uuid-mid", "uuid-young"]);
    }

    /// The compact row squeezes the MIDDLE out of an over-long worktree name.
    /// A head-only truncation renders sibling worktrees of one repo identical,
    /// which is the exact ambiguity this listing exists to remove.
    #[test]
    fn the_compact_worktree_column_keeps_both_ends_not_just_the_head() {
        let cwd = format!("/tmp/worktrees/by-name/{WORKTREE}");
        let rows = rows_from(vec![pending("uuid-a", 4_200)], &cwd_index("uuid-a", &cwd));
        let text = render_text(&rows, false);
        assert!(
            text.contains("agents-in-a-box--…-probe--deadbeef"),
            "compact row must keep repo head AND disambiguating tail:\n{text}"
        );
        for needle in [
            "SESSION",
            "WORKSPACE",
            "TOOL",
            "WAITING",
            "REQUEST",
            "Bash",
            "4s",
        ] {
            assert!(
                text.contains(needle),
                "listing is missing {needle:?}:\n{text}"
            );
        }
        // Compact mode must NOT print the absolute cwd line.
        assert!(
            !text.contains("cwd: "),
            "compact row leaked the cwd:\n{text}"
        );
    }

    /// `--full` shortens nothing: whole worktree name plus the absolute cwd.
    #[test]
    fn full_elides_nothing_and_adds_the_absolute_cwd() {
        let cwd = format!("/tmp/worktrees/by-name/{WORKTREE}");
        let rows = rows_from(vec![pending("uuid-a", 4_200)], &cwd_index("uuid-a", &cwd));
        let text = render_text(&rows, true);
        assert!(
            text.contains(WORKTREE),
            "--full elided the worktree:\n{text}"
        );
        assert!(
            !text.contains('…'),
            "--full must not elide anything:\n{text}"
        );
        assert!(
            text.contains(&format!("cwd: {cwd}")),
            "--full omitted the cwd:\n{text}"
        );
        assert!(
            text.contains(TOOL_INPUT),
            "--full truncated the tool input:\n{text}"
        );
    }

    /// An empty queue says so, instead of printing a bare header.
    #[test]
    fn an_empty_queue_renders_a_sentence_not_an_empty_table() {
        let text = render_text(&[], false);
        assert_eq!(text, "no sessions waiting on a permission decision\n");
    }

    /// These strings arrive off a socket written by whoever dialled it, so a
    /// control character must never reach the operator's terminal verbatim.
    #[test]
    fn control_characters_never_reach_the_terminal() {
        let rows: Vec<PendingRow> = rows_from(
            vec![PendingInfo {
                session_id: "uuid-a".to_string(),
                tool: "Bash".to_string(),
                context: "before\u{1b}[2Jafter".to_string(),
                request_fingerprint: None,
                questions: Vec::new(),
                waiting_ms: 1_000,
            }],
            &HashMap::new(),
        );
        for full in [false, true] {
            let text = render_text(&rows, full);
            assert!(
                !text.contains('\u{1b}'),
                "escape survived (full={full}):\n{text}"
            );
            assert!(
                text.contains("before"),
                "text was dropped wholesale:\n{text}"
            );
        }
    }

    /// Boundary behaviour of the two shorteners, independent of any row.
    #[test]
    fn elide_middle_and_preview_respect_their_widths() {
        assert_eq!(elide_middle("short", 34), "short");
        let squeezed = elide_middle(WORKTREE, 20);
        assert_eq!(squeezed.chars().count(), 20);
        assert!(squeezed.starts_with("agents-in-"), "got: {squeezed}");
        assert!(squeezed.ends_with("deadbeef"), "got: {squeezed}");
        // A width too small for two ends degrades to a head preview, not a panic.
        assert_eq!(elide_middle(WORKTREE, 2).chars().count(), 2);
        assert_eq!(preview("abcdef", 3), "ab…");
        assert_eq!(preview("abc", 3), "abc");
    }

    /// Multi-byte names must be sliced on characters, not bytes.
    #[test]
    fn a_multibyte_worktree_name_is_elided_without_splitting_a_character() {
        let name = "日本語ワークツリー-テスト-用-deadbeef";
        // Width 20 keeps 10 head + 9 tail chars around the ellipsis, so the
        // disambiguating hash survives whole and the head stops on a character
        // boundary rather than mid-sequence.
        let out = elide_middle(name, 20);
        assert_eq!(out.chars().count(), 20, "got: {out}");
        assert!(out.starts_with("日本語ワーク"), "got: {out}");
        assert!(out.ends_with("deadbeef"), "got: {out}");
        assert!(!out.contains('\u{fffd}'), "a character was split: {out}");
    }

    #[test]
    fn workspace_of_takes_the_last_path_segment() {
        assert_eq!(
            workspace_of("/tmp/worktrees/by-name/wt-1").as_deref(),
            Some("wt-1")
        );
        assert_eq!(workspace_of("").as_deref(), None);
    }
}
