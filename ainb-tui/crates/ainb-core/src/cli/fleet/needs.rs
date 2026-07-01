// ABOUTME: `ainb fleet needs` — center control panel.
//
// Classify every claude session against four signal kinds (ASK / ERR / IDLE /
// WAIT). Returns rich JSON: per-session signal + context that the calling LLM
// uses to render the Jarvis-HUD layout and route answers back.

use anyhow::Result;

use crate::cli::OutputFormat;
use crate::fleet::discover::{
    discover_from_ainb, discover_from_jobs, discover_from_peers, merge_sessions,
};
use crate::fleet::enrich_cache;
use crate::fleet::read::{
    ClassifyInput, CurrentStateIndex, NeedsRow, Resolution, capture_pane, classify,
};
use crate::fleet::types::Session;

/// Staleness window (ms) for a hook-sourced `current_state` row before the
/// reader falls back to a live `classify()` scan. `0` (the default) disables
/// the age check: ASK/ERR/WAIT/IDLE are sticky states the materializer only
/// changes on a new event, so an old row is normally still the truth. Override
/// via `AINB_FLEET_STATE_STALE_MS` only when there is an independent reason to
/// distrust an aged row (e.g. a known-flaky daemon).
fn stale_window_ms() -> i64 {
    std::env::var("AINB_FLEET_STATE_STALE_MS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|n| *n >= 0)
        .unwrap_or(0)
}

pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let idle_override: Option<i64> = matches.get_one::<i64>("idle-min").copied();
    let enrich = enrich_enabled(matches);

    let (ainb_res, jobs_res) = tokio::join!(discover_from_ainb(), async { discover_from_jobs() });
    let ainb = ainb_res.unwrap_or_default();
    let peers = discover_from_peers().unwrap_or_default();
    let jobs = jobs_res.unwrap_or_default();

    let merged = merge_sessions(vec![ainb, peers, jobs]);
    let rows = classify_all(merged, idle_override, enrich).await;

    if matches!(format, OutputFormat::Text) {
        print_text(&rows);
    } else {
        let json = serde_json::to_string_pretty(&rows)?;
        println!("{json}");
    }
    Ok(())
}

/// Enrichment is on by default. `--no-enrich` or `AINB_FLEET_ENRICH=0` turns it
/// off — the reader still attaches free cached suggestions, but no card is
/// flagged `need_enrich`, so no producer (inline or agent) runs → 0 tokens.
pub fn enrich_enabled(matches: &clap::ArgMatches) -> bool {
    if matches
        .try_get_one::<bool>("no-enrich")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
    {
        return false;
    }
    std::env::var("AINB_FLEET_ENRICH").map(|v| v != "0").unwrap_or(true)
}

/// Classify every session, reading the event-sourced `current_state` table as
/// the PRIMARY source and falling back to the live `classify()` pane/transcript
/// scan only where the event store has nothing usable.
///
/// Per session (keyed by cwd — the fleet's cross-source dedupe key, and the
/// same cwd-correlation the Inbox uses to map ainb sessions to hook-side
/// `session_id`s):
///
/// - **hook-authoritative** (`Resolution::Hook`): an ASK/ERR/WAIT/IDLE row is
///   materialized for this session's cwd with `source = hook` — use it directly,
///   no pane scan. (Stamped `source = Some("hook")`.)
/// - **hook-healthy** (`Resolution::Healthy`): the hook says RUNNING/DONE — the
///   session is not a "need"; emit nothing AND skip the pane scan (the hooks are
///   more authoritative than a pane heuristic for a Claude session).
/// - **fallback** (`Resolution::Fallback`): no usable hook row — absent from
///   `current_state` (a non-Claude/tmux-only session like Codex/Gemini fires no
///   Claude hooks), `source = tmux`, or a stale row — run the live `classify()`
///   path, exactly as before the event store existed. This read-time merge IS
///   the tmux / non-Claude fold. (Stamped `source = Some("tmux")`.)
///
/// Each session yields at most one row, so a session is never double-listed:
/// the merged input is already cwd-deduped by `merge_sessions`, and each session
/// takes exactly one of the three branches above.
async fn classify_all(
    sessions: Vec<Session>,
    idle_override: Option<i64>,
    enrich: bool,
) -> Vec<NeedsRow> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let stale_ms = stale_window_ms();
    // Snapshot the materialized read model once (read-only Store open). Empty +
    // every session falls back when the daemon is down / not installed.
    let index = CurrentStateIndex::load();

    let mut hook_rows: Vec<NeedsRow> = Vec::new();
    let mut fallback_sessions: Vec<Session> = Vec::new();
    for session in sessions {
        match index.resolve(&session, now_ms, stale_ms) {
            Resolution::Hook(row) => hook_rows.push(*row),
            // Healthy hook state → not a need, and authoritative enough to skip
            // the pane scan.
            Resolution::Healthy => {}
            Resolution::Fallback => fallback_sessions.push(session),
        }
    }

    // Run the live classifier only for the fallback sessions, in parallel.
    let mut handles = Vec::with_capacity(fallback_sessions.len());
    for session in fallback_sessions {
        let tmux = session.tmux_session.clone();
        handles.push(tokio::spawn(async move {
            let pane = match tmux {
                Some(name) => capture_pane(&name, 80).await.ok(),
                None => None,
            };
            let mut input = ClassifyInput::from_env(session, pane, now_ms);
            if let Some(idle) = idle_override {
                input.idle_threshold_min = idle;
            }
            classify(input)
        }));
    }

    let mut out = hook_rows;
    for h in handles {
        if let Ok(Some(mut row)) = h.await {
            // Mark the provenance so a consumer can tell a tmux-folded need from
            // a hook-sourced one (the JSON field is optional + additive).
            if row.source.is_none() {
                row.source = Some(crate::fleet::read::current_state::SOURCE_TMUX.to_string());
            }
            out.push(row);
        }
    }

    // Attach enrichment uniformly across both sources: a fresh cached suggestion
    // for free, else flag the card for the producer when enrichment is enabled.
    for row in &mut out {
        if let Some(s) = enrich_cache::lookup(&row.enrich_key) {
            row.enriched = Some(s);
        } else if enrich {
            row.need_enrich = true;
        }
    }

    out.sort_by_key(|r| signal_priority(r));
    out
}

fn signal_priority(r: &NeedsRow) -> u8 {
    use crate::fleet::read::NeedsContext;
    match r.context {
        NeedsContext::Ask(_) => 0,
        NeedsContext::Err(_) => 1,
        NeedsContext::Idle(_) => 2,
        NeedsContext::Wait(_) => 3,
    }
}

fn print_text(rows: &[NeedsRow]) {
    use crate::fleet::read::NeedsContext;
    if rows.is_empty() {
        println!("ainb fleet needs — 0 sessions need you. Carry on.");
        return;
    }
    let (mut ask, mut err, mut idle, mut wait) = (0, 0, 0, 0);
    for r in rows {
        match r.context {
            NeedsContext::Ask(_) => ask += 1,
            NeedsContext::Err(_) => err += 1,
            NeedsContext::Idle(_) => idle += 1,
            NeedsContext::Wait(_) => wait += 1,
        }
    }
    println!("ainb fleet needs — {} need you", rows.len());
    println!("  🔴 {err} err · 🟡 {ask} ask · ⚪ {idle} idle · 🟢 {wait} wait");
    println!();
    for r in rows {
        let name = r
            .session
            .tmux_session
            .as_deref()
            .or(r.session.workspace_name.as_deref())
            .unwrap_or(&r.session.id);
        match &r.context {
            NeedsContext::Ask(aq) => {
                println!("▸ 🟡 {name} ─ {}", truncate_line(&aq.question, 100));
                for (i, opt) in aq.options.iter().enumerate().take(5) {
                    let glyph = ["①", "②", "③", "④", "⑤"][i.min(4)];
                    println!("    {glyph} {}", truncate_line(&opt.label, 90));
                }
            }
            NeedsContext::Err(e) => {
                println!(
                    "▸ 🔴 {name} ─ {} ({})",
                    e.pattern,
                    truncate_line(&e.snippet, 60)
                );
            }
            NeedsContext::Idle(i) => {
                println!("▸ ⚪ {name} ─ idle {}m", i.idle_minutes);
                if let Some(text) = &i.last_assistant_text {
                    println!("    '{}'", truncate_line(text, 90));
                }
            }
            NeedsContext::Wait(w) => {
                println!("▸ 🟢 {name} ─ {} {}", w.marker, truncate_line(&w.text, 80));
            }
        }
        if let Some(s) = &r.enriched {
            println!("    ↳ suggest: {}", truncate_line(s, 90));
        }
    }
}

fn truncate_line(s: &str, max: usize) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        one
    } else {
        let cut: String = one.chars().take(max).collect();
        format!("{cut}…")
    }
}
