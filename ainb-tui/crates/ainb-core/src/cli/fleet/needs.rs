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
use crate::fleet::read::{ClassifyInput, NeedsRow, capture_pane, classify};
use crate::fleet::types::Session;

pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let idle_override: Option<i64> = matches.get_one::<i64>("idle-min").copied();

    let (ainb_res, jobs_res) = tokio::join!(discover_from_ainb(), async { discover_from_jobs() });
    let ainb = ainb_res.unwrap_or_default();
    let peers = discover_from_peers().unwrap_or_default();
    let jobs = jobs_res.unwrap_or_default();

    let merged = merge_sessions(vec![ainb, peers, jobs]);
    let rows = classify_all(merged, idle_override).await;

    if matches!(format, OutputFormat::Text) {
        print_text(&rows);
    } else {
        let json = serde_json::to_string_pretty(&rows)?;
        println!("{json}");
    }
    Ok(())
}

async fn classify_all(sessions: Vec<Session>, idle_override: Option<i64>) -> Vec<NeedsRow> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut handles = Vec::with_capacity(sessions.len());
    for session in sessions {
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
    let mut out = Vec::new();
    for h in handles {
        if let Ok(Some(row)) = h.await {
            out.push(row);
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
