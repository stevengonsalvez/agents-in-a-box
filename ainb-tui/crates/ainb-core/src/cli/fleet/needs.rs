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
    AskUserQuestionData, ClassifyInput, CurrentStateIndex, NeedsContext, NeedsRow, ProbeIndex,
    Resolution, capture_pane, classify, discover_from_probes, last_ask_user_question,
    latest_transcript_for_cwd,
};
use crate::fleet::types::{Session, SessionSource};
use ainb_fleet_core::fleet::read::needs::idle_threshold_from_env;

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

    // Tier-A probes are BOTH a discovery source and a resolver. Load once here
    // so a Claude session ainb never launched (hand-started, or `kind: "bg"`)
    // can enter the fleet at all: it has no ainb record, no broker row and
    // possibly no tmux pane, so its probe file is its only trace.
    let probes = tokio::task::spawn_blocking(ProbeIndex::load)
        .await
        .unwrap_or_default();
    let known = merge_sessions(vec![ainb, peers, jobs]);
    // Probe discovery ADDS sessions, it must never duplicate one.
    //
    // `merge_sessions` merges on stable identity (id / peer_id / tmux_session /
    // bg_job_id) and deliberately never on cwd. A probe Session carries
    // Claude's own session id and none of the other keys, so it matches nothing
    // and every already-known session was emitted twice — once as `ainb`, once
    // as `probe`. Keep only probes for a cwd nothing else claims, which is
    // exactly the population this discovery exists to reach.
    let claimed: std::collections::HashSet<&str> =
        known.iter().map(|s| s.cwd.as_str()).filter(|c| !c.is_empty()).collect();
    let unclaimed: Vec<Session> = discover_from_probes(&probes)
        .into_iter()
        .filter(|s| !claimed.contains(s.cwd.as_str()))
        .collect();
    let merged = merge_sessions(vec![known, unclaimed]);
    let (rows, census) = classify_all(merged, &probes, idle_override, enrich).await;

    if matches!(format, OutputFormat::Text) {
        print_text(&rows);
        // Always shown, including when nothing needs attention: "0 need you"
        // means something very different when tier B answered for the whole
        // fleet than when it answered for none of it.
        println!("  {}", census.summary_line());
    } else {
        // The JSON stays a BARE ARRAY of rows. The ATC heartbeat parses it as
        // `Vec<NeedsRow>`, so wrapping it in an object to carry the census
        // would break the one consumer this work exists to serve.
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
/// The open `AskUserQuestion` for a session, read from its transcript.
///
/// Tier A learns THAT a session is blocked from the probe file, but the probe
/// carries only a reason string ("input needed"); the structured question, its
/// header and options live in the JSONL. Reading it here keeps
/// [`ProbeIndex::resolve`] pure and I/O-free.
fn ask_from_transcript(session: &Session) -> Option<AskUserQuestionData> {
    let path = session
        .transcript_path
        .as_deref()
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| latest_transcript_for_cwd(&session.cwd))?;
    last_ask_user_question(&path)
}

/// Whether the probe tier is the ONLY source that knows this session, i.e.
/// ainb did not launch it and no peer or tmux pane backs it.
fn is_probe_only(session: &Session) -> bool {
    session.sources.as_slice() == [SessionSource::Probe]
}

fn should_scan(session: &Session, probe_status: Option<&str>) -> bool {
    !(is_probe_only(session) && probe_status == Some("idle"))
}

/// What each evidence tier actually SAW this run, including the tiers that saw
/// nothing.
///
/// A dead tier produces no rows, which is indistinguishable from a healthy tier
/// with nothing to report — that is precisely how a broken hook pipeline ran
/// unnoticed for 65 days while every classification silently came from pane
/// scraping. Counting sessions per tier makes the difference legible: tier B at
/// zero while tier D carries the whole fleet is a broken pipeline, not a quiet
/// one.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct TierCensus {
    /// Sessions answered by the Claude probe files (tier A).
    pub probe: usize,
    /// Sessions answered by the materialized hook state (tier B).
    pub hook: usize,
    /// Sessions that fell through to the pane/transcript scan (tier D).
    pub scan: usize,
    /// Sessions affirmatively reported as WORKING rather than merely silent.
    ///
    /// The fleet has never been able to say this before: a running session and
    /// a stuck one both produced no row. Counted here rather than emitted as a
    /// needs row, because a working session does not need anything.
    pub running: usize,
    /// Live probe files found on the host, whether or not they answered.
    pub probes_seen: usize,
}

impl TierCensus {
    /// One line for the text footer.
    #[must_use]
    pub fn summary_line(&self) -> String {
        format!(
            "tiers — probe {} · hook {} · scan {} | {} working · {} live probe(s)",
            self.probe, self.hook, self.scan, self.running, self.probes_seen
        )
    }
}

async fn classify_all(
    sessions: Vec<Session>,
    probes: &ProbeIndex,
    idle_override: Option<i64>,
    enrich: bool,
) -> (Vec<NeedsRow>, TierCensus) {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let stale_ms = stale_window_ms();
    // Snapshot the materialized read model once (read-only Store open). Empty +
    // every session falls back when the daemon is down / not installed.
    let index = CurrentStateIndex::load();
    // TIER A, ahead of the hook read: Claude's own per-session probe files.
    // One `ps` per probe, once, not once per session.
    let idle_threshold = idle_override.unwrap_or_else(idle_threshold_from_env);

    let mut census = TierCensus {
        probes_seen: probes.len(),
        ..TierCensus::default()
    };
    let mut hook_rows: Vec<NeedsRow> = Vec::new();
    let mut fallback_sessions: Vec<Session> = Vec::new();
    for session in sessions {
        // Tier A first. It is the only source that reports RUNNING
        // affirmatively and that sees a block the instant it happens, so where
        // it has a live answer it outranks the materialized hook row. It
        // abstains (`None`) on anything it cannot prove — unknown status, dead
        // pid, un-ageable idle — and the session then takes exactly the path it
        // takes today.
        //
        // The open question is read from the transcript, not the probe: the
        // probe knows THAT a session blocked, the transcript knows WHAT it
        // asked. Only consulted for a session tier A is about to call `waiting`.
        let probe_ask = probes
            .peek_status(&session)
            .filter(|status| *status == "waiting")
            .and_then(|_status| ask_from_transcript(&session));
        // Tier A ANSWERS only where it knows more than the tiers below. It does
        // NOT get to silence them.
        //
        // The probe carries no error information and `resolve_probe` can never
        // produce an ERR, but only the pane/transcript scan can see an
        // `API Error: 500 overloaded_error`. Letting tier A short-circuit every
        // probed session therefore made ERR unreachable on a Claude fleet and
        // silently killed ATC's capped auto-`continue`, which is the whole
        // reason the retry ledger exists.
        //
        // So: `waiting` short-circuits, because a structured ASK/WAIT read the
        // instant it happens beats anything the lower tiers can infer. Every
        // other status falls through and is used only to enrich the census —
        // an idle probe has no `last_assistant_text` and no error visibility,
        // so the transcript path is strictly better placed to classify it.
        let probe_status = probes.peek_status(&session);
        let probe_asserts_work = probe_status == Some("busy") || probe_status == Some("shell");
        if probe_asserts_work {
            census.running += 1;
        }
        if probe_status == Some("waiting") {
            if let Some(resolution) = probes.resolve(&session, probe_ask, idle_threshold, now_ms) {
                census.probe += 1;
                match resolution {
                    Resolution::Hook(row) => hook_rows.push(*row),
                    Resolution::Healthy => {}
                    // Tier A never returns Fallback; it abstains with None instead.
                    Resolution::Fallback => fallback_sessions.push(session),
                }
                continue;
            }
        }

        match index.resolve(&session, now_ms, stale_ms) {
            Resolution::Hook(row) => {
                census.hook += 1;
                hook_rows.push(*row);
            }
            // Healthy hook state → not a need, and authoritative enough to skip
            // the pane scan. The hooks materialize RUNNING/DONE and the
            // classifier drops both; RUNNING is genuinely working, so count it.
            Resolution::Healthy => {
                census.hook += 1;
                if !probe_asserts_work {
                    census.running += 1;
                }
            }
            Resolution::Fallback => fallback_sessions.push(session),
        }
    }

    // Run the live classifier only for the fallback sessions, in parallel.
    // A probe-only idle session has no reply path and does not need action.
    // Suppress it before the pane scan, where an idle heuristic could otherwise
    // turn infrastructure observation into a Fleet work row.
    fallback_sessions.retain(|session| should_scan(session, probes.peek_status(session)));
    census.scan = fallback_sessions.len();
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
    (out, census)
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

#[cfg(test)]
mod census_tests {
    use super::{TierCensus, should_scan};
    use crate::fleet::types::{Session, SessionSource};

    fn probe_only_session() -> Session {
        Session {
            id: "probe-session".to_string(),
            cwd: "/tmp/probe".to_string(),
            pid: None,
            git_root: None,
            tmux_session: None,
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![SessionSource::Probe],
            summary: None,
            last_seen_ms: None,
        }
    }

    #[test]
    fn probe_only_idle_does_not_become_a_fleet_work_row() {
        let session = probe_only_session();
        assert!(!should_scan(&session, Some("idle")));
        assert!(should_scan(&session, Some("busy")));
        assert!(should_scan(&session, None));
    }

    #[test]
    fn summary_line_names_every_tier_including_the_silent_ones() {
        // A tier at zero MUST still print. The whole point is telling a quiet
        // tier from a dead one, and a dead tier prints nothing by definition.
        let line = TierCensus::default().summary_line();
        assert!(line.contains("probe 0"), "{line}");
        assert!(line.contains("hook 0"), "{line}");
        assert!(line.contains("scan 0"), "{line}");
        assert!(line.contains("0 working"), "{line}");
    }

    #[test]
    fn census_reports_a_pipeline_carried_entirely_by_the_scan() {
        // The shape a broken hook pipeline actually has: hook silent, scan
        // carrying the fleet. Indistinguishable from health before this line
        // existed, and it ran that way unnoticed for 65 days.
        let line = TierCensus {
            scan: 12,
            ..TierCensus::default()
        }
        .summary_line();
        assert!(line.contains("hook 0"), "{line}");
        assert!(line.contains("scan 12"), "{line}");
    }

    #[test]
    fn working_is_counted_not_emitted_as_a_need() {
        // A working session is affirmative information, but it needs nothing,
        // so it belongs in the census and never in the rows.
        let line = TierCensus {
            probe: 4,
            hook: 1,
            scan: 0,
            running: 3,
            probes_seen: 9,
        }
        .summary_line();
        assert!(line.contains("3 working"), "{line}");
        assert!(line.contains("9 live probe(s)"), "{line}");
    }
}
