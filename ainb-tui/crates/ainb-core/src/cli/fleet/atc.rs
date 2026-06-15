// ABOUTME: `ainb fleet atc` — provision / manage the Air Traffic Control brain.
//
// Verbs:
//   setup <name>     provision ~/.agents-in-a-box/atc/<name>/ (CLAUDE.md + meta +
//                    seeded state/task-log), install the heartbeat timer, and
//                    spawn the ATC session via `ainb run`. Idempotent.
//   teardown <name>  remove the timer + instance dir. Safe when absent.
//   status <name>    report one instance (meta + timer + session liveness).
//   list             list all provisioned instances.
//   heartbeat <name> (internal, called by the OS timer) build the [HEARTBEAT]
//                    nudge from `fleet needs --format json` and tmux-send it
//                    into the ATC session.

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::cli::OutputFormat;
use crate::fleet::atc::{
    AtcMeta, AtcPaths, DEFAULT_ERR_RETRY_CAP, RetryLedger, build_heartbeat_with_ledger,
    render_claude_md, should_pause_for_idle, timer,
};
use crate::fleet::read::NeedsRow;
use crate::fleet::send::{tmux_send, tmux_session_exists};

pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("setup", sub)) => setup(sub, format).await,
        Some(("teardown", sub)) => teardown(sub, format).await,
        Some(("status", sub)) => status(sub, format).await,
        Some(("list", _)) => list(format).await,
        Some(("heartbeat", sub)) => heartbeat(sub, format).await,
        _ => bail!("unknown `ainb fleet atc` subcommand — try `ainb fleet atc --help`"),
    }
}

// --- setup ------------------------------------------------------------------

async fn setup(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let name = require_name(matches)?;
    let interval = matches.get_one::<u32>("interval").copied();
    let idle_pause = matches.get_one::<u32>("idle-pause").copied();
    let no_heartbeat = matches.get_flag("no-heartbeat");
    let no_spawn = matches.get_flag("no-spawn");

    let mut meta = AtcMeta::new(&name);
    if let Some(i) = interval {
        meta.heartbeat_interval_min = i;
    }
    if let Some(p) = idle_pause {
        meta.idle_pause_min = p;
    }
    meta.heartbeat_enabled = !no_heartbeat;

    let paths = AtcPaths::resolve(&name)?;
    std::fs::create_dir_all(&paths.dir)
        .with_context(|| format!("creating ATC dir {}", paths.dir.display()))?;

    // Render policy + write meta. Always overwrite CLAUDE.md/meta so setup is
    // idempotent and picks up policy/template changes on re-run.
    std::fs::write(&paths.claude_md, render_claude_md(&meta)).context("writing CLAUDE.md")?;
    std::fs::write(&paths.meta, meta.to_json()?).context("writing meta.json")?;

    // Seed durable memory only if absent (never clobber accumulated state).
    if !paths.state.exists() {
        std::fs::write(&paths.state, seed_state_json()).context("seeding state.json")?;
    }
    if !paths.task_log.exists() {
        std::fs::write(&paths.task_log, seed_task_log(&name)).context("seeding task-log.md")?;
    }

    // Install the heartbeat timer (idempotent).
    let mut timer_paths = Vec::new();
    if meta.heartbeat_enabled {
        timer_paths = timer::install(&meta).context("installing heartbeat timer")?;
    }

    // Spawn the ATC session unless suppressed (tests / dry provisioning).
    let mut spawned = false;
    if !no_spawn {
        spawned = spawn_session(&meta, &paths).await?;
    }

    let summary = json!({
        "action": "setup",
        "name": name,
        "dir": paths.dir.display().to_string(),
        "tmux_session": meta.tmux_session(),
        "heartbeat_enabled": meta.heartbeat_enabled,
        "heartbeat_interval_min": meta.heartbeat_interval_min,
        "idle_pause_min": meta.idle_pause_min,
        "timer_units": timer_paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "session_spawned": spawned,
    });

    if matches!(format, OutputFormat::Text) {
        println!("ATC '{name}' provisioned.");
        println!("  dir:       {}", paths.dir.display());
        println!("  policy:    {}", paths.claude_md.display());
        println!("  session:   {} (spawned: {spawned})", meta.tmux_session());
        if meta.heartbeat_enabled {
            println!(
                "  heartbeat: every {}m via {} unit(s)",
                meta.heartbeat_interval_min,
                timer_paths.len()
            );
        } else {
            println!("  heartbeat: disabled");
        }
        println!("  attach:    ainb attach {name}");
    } else {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    }
    Ok(())
}

/// Spawn the ATC Claude session via `ainb run`, rooted in the instance dir so it
/// reads the generated `CLAUDE.md`. Returns whether a new session was started
/// (false when one with the same name is already running → idempotent).
async fn spawn_session(meta: &AtcMeta, paths: &AtcPaths) -> Result<bool> {
    if tmux_session_exists(&meta.tmux_session()).await {
        return Ok(false);
    }
    let bin = std::env::var("AINB_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string))
            .unwrap_or_else(|| "ainb".to_string())
    });
    let bootstrap = format!(
        "You are ATC. Read {} as your operating policy, then read state.json and \
task-log.md. Stand by for [HEARTBEAT] messages and act per the policy.",
        paths.claude_md.display()
    );
    let status = tokio::process::Command::new(&bin)
        .args([
            "run",
            "--repo",
            &paths.dir.display().to_string(),
            "--name",
            &meta.name,
            "--dangerously-skip-permissions",
            "-p",
            &bootstrap,
        ])
        .status()
        .await
        .context("spawning ATC session via `ainb run`")?;
    if !status.success() {
        bail!("`ainb run` exited {status} — ATC session not spawned");
    }
    Ok(true)
}

// --- teardown ---------------------------------------------------------------

async fn teardown(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let name = require_name(matches)?;
    let purge = matches.get_flag("purge");

    // Remove the timer first (idempotent, safe when absent).
    let removed = timer::teardown(&name).context("removing heartbeat timer")?;

    // Best-effort kill the running session.
    let meta_session = format!("tmux_{name}");
    let mut killed = false;
    if tmux_session_exists(&meta_session).await {
        let bin = std::env::var("AINB_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().map(str::to_string))
                .unwrap_or_else(|| "ainb".to_string())
        });
        let _ = tokio::process::Command::new(&bin).args(["kill", &name]).status().await;
        killed = true;
    }

    // Remove the instance dir only when --purge (default keeps state/task-log).
    let paths = AtcPaths::resolve(&name)?;
    let mut purged = false;
    if purge && paths.dir.exists() {
        std::fs::remove_dir_all(&paths.dir)
            .with_context(|| format!("purging ATC dir {}", paths.dir.display()))?;
        purged = true;
    }

    let summary = json!({
        "action": "teardown",
        "name": name,
        "timer_units_removed": removed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "session_killed": killed,
        "dir_purged": purged,
    });

    if matches!(format, OutputFormat::Text) {
        println!("ATC '{name}' torn down.");
        println!("  timer units removed: {}", removed.len());
        println!("  session killed:      {killed}");
        println!("  dir purged:          {purged}");
        if !purge {
            println!("  (state.json + task-log.md kept; re-run with --purge to delete)");
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    }
    Ok(())
}

// --- status -----------------------------------------------------------------

async fn status(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let name = require_name(matches)?;
    let paths = AtcPaths::resolve(&name)?;
    if !paths.meta.exists() {
        bail!(
            "no ATC instance named '{name}' (expected {})",
            paths.meta.display()
        );
    }
    let meta = AtcMeta::from_json(&std::fs::read_to_string(&paths.meta)?)?;
    let timer_installed = timer::is_installed(&name);
    let session_running = tmux_session_exists(&meta.tmux_session()).await;

    let summary = json!({
        "name": meta.name,
        "dir": paths.dir.display().to_string(),
        "heartbeat_enabled": meta.heartbeat_enabled,
        "heartbeat_interval_min": meta.heartbeat_interval_min,
        "idle_pause_min": meta.idle_pause_min,
        "timer_installed": timer_installed,
        "session_running": session_running,
        "tmux_session": meta.tmux_session(),
    });

    if matches!(format, OutputFormat::Text) {
        println!("ATC '{}' status", meta.name);
        println!("  dir:       {}", paths.dir.display());
        println!(
            "  session:   {} (running: {session_running})",
            meta.tmux_session()
        );
        println!(
            "  heartbeat: {} (timer installed: {timer_installed}, every {}m)",
            if meta.heartbeat_enabled {
                "enabled"
            } else {
                "disabled"
            },
            meta.heartbeat_interval_min
        );
        println!("  idle-pause: {}m", meta.idle_pause_min);
    } else {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    }
    Ok(())
}

// --- list -------------------------------------------------------------------

async fn list(format: OutputFormat) -> Result<()> {
    let names = crate::fleet::atc::paths::list_instance_names()?;
    let mut rows = Vec::new();
    for name in &names {
        let paths = AtcPaths::resolve(name)?;
        let meta = match std::fs::read_to_string(&paths.meta)
            .ok()
            .and_then(|s| AtcMeta::from_json(&s).ok())
        {
            Some(m) => m,
            None => continue,
        };
        let running = tmux_session_exists(&meta.tmux_session()).await;
        rows.push(json!({
            "name": meta.name,
            "heartbeat_enabled": meta.heartbeat_enabled,
            "heartbeat_interval_min": meta.heartbeat_interval_min,
            "timer_installed": timer::is_installed(name),
            "session_running": running,
        }));
    }

    if matches!(format, OutputFormat::Text) {
        if rows.is_empty() {
            println!("No ATC instances provisioned. Run `ainb fleet atc setup <name>`.");
            return Ok(());
        }
        println!("ATC instances ({}):", rows.len());
        for r in &rows {
            println!(
                "  {} — heartbeat {} ({}m) · timer {} · session {}",
                r["name"].as_str().unwrap_or("?"),
                if r["heartbeat_enabled"].as_bool().unwrap_or(false) {
                    "on"
                } else {
                    "off"
                },
                r["heartbeat_interval_min"].as_u64().unwrap_or(0),
                if r["timer_installed"].as_bool().unwrap_or(false) {
                    "installed"
                } else {
                    "absent"
                },
                if r["session_running"].as_bool().unwrap_or(false) {
                    "running"
                } else {
                    "stopped"
                },
            );
        }
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "instances": rows }))?
        );
    }
    Ok(())
}

// --- heartbeat (internal, timer-driven) -------------------------------------

async fn heartbeat(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let name = require_name(matches)?;
    let paths = AtcPaths::resolve(&name)?;
    if !paths.meta.exists() {
        bail!("no ATC instance named '{name}' — cannot fire heartbeat");
    }
    let meta = AtcMeta::from_json(&std::fs::read_to_string(&paths.meta)?)?;
    let tmux = meta.tmux_session();

    // Pull the LLM-free needs read. Shelling the verb keeps ATC poll-mode and
    // reuses the exact same classifier the operator sees.
    let rows = fetch_needs().await.unwrap_or_default();
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Idle-pause: when the fleet has been quiet past the threshold, downgrade to
    // a cheap idle ping so ATC spends no tokens. `last_active_ms` is the last
    // time the fleet had something needing attention, tracked in state.json.
    let last_active_ms = read_last_active_ms(&paths);
    let paused = should_pause_for_idle(rows.len(), last_active_ms, meta.idle_pause_min, now_ms);
    // Reconstruct the ERR retry ledger from state.json so the cap is enforced
    // across timer firings: any ERR session already at the cap is flagged
    // ESCALATE in the body instead of being auto-continued again.
    let ledger = read_retry_ledger(&paths);
    let body = if paused {
        format!(
            "[HEARTBEAT {now_ms}] fleet idle-paused (quiet ≥ {}m) — standing by, no token spend.",
            meta.idle_pause_min
        )
    } else {
        build_heartbeat_with_ledger(&rows, now_ms, Some(&ledger))
    };

    // If the session is gone, do not send into a dead pane — report and exit 0
    // so the timer keeps firing harmlessly until teardown.
    let session_live = tmux_session_exists(&tmux).await;
    let mut delivered = false;
    if session_live && !paused {
        tmux_send(&tmux, &body).await.context("sending heartbeat into ATC session")?;
        delivered = true;
    }

    // Persist heartbeat bookkeeping so the idle-pause window + retry cap survive
    // across timer firings and context compaction.
    update_state(&paths, now_ms, rows.len(), last_active_ms);

    let summary = json!({
        "action": "heartbeat",
        "name": name,
        "needs_count": rows.len(),
        "session_live": session_live,
        "idle_paused": paused,
        "delivered": delivered,
        "now_ms": now_ms,
    });

    if matches!(format, OutputFormat::Text) {
        if paused {
            println!("[atc/{name}] fleet idle-paused — heartbeat downgraded to standby");
        } else if delivered {
            println!(
                "[atc/{name}] heartbeat delivered — {} session(s) need attention",
                rows.len()
            );
        } else {
            println!("[atc/{name}] session not live — heartbeat skipped");
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    }
    Ok(())
}

/// Read `last_active_ms` from state.json (the last heartbeat that saw needs).
/// Missing/corrupt → None (the idle-pause logic then stays conservative).
fn read_last_active_ms(paths: &AtcPaths) -> Option<i64> {
    let raw = std::fs::read_to_string(&paths.state).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("last_active_ms").and_then(serde_json::Value::as_i64)
}

/// Rebuild the ERR retry ledger from state.json's `retry_counts` map. The ATC
/// session owns the counts (it increments on each `continue` it sends, per the
/// CLAUDE.md policy); the heartbeat reads them back to flag cap-exhausted ERR
/// sessions for escalation. Missing/corrupt → an empty ledger at the default cap.
fn read_retry_ledger(paths: &AtcPaths) -> RetryLedger {
    let mut ledger = RetryLedger::new(DEFAULT_ERR_RETRY_CAP);
    let Some(raw) = std::fs::read_to_string(&paths.state).ok() else {
        return ledger;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return ledger;
    };
    if let Some(map) = v.get("retry_counts").and_then(serde_json::Value::as_object) {
        for (session_id, count) in map {
            let n = count.as_u64().unwrap_or(0);
            for _ in 0..n {
                ledger.record(session_id);
            }
        }
    }
    ledger
}

/// Update state.json bookkeeping: stamp `last_heartbeat_ms` always, and refresh
/// `last_active_ms` to now whenever the fleet currently has needs (so the
/// idle-pause window is measured from the last genuinely-busy moment).
fn update_state(paths: &AtcPaths, now_ms: i64, needs_count: usize, prev_active: Option<i64>) {
    let mut v: serde_json::Value = std::fs::read_to_string(&paths.state)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("last_heartbeat_ms".into(), json!(now_ms));
        let active = if needs_count > 0 {
            now_ms
        } else {
            prev_active.unwrap_or(now_ms)
        };
        obj.insert("last_active_ms".into(), json!(active));
    }
    if let Ok(s) = serde_json::to_string_pretty(&v) {
        let _ = std::fs::write(&paths.state, s);
    }
}

/// Shell `ainb fleet needs --no-enrich --format json` and parse the rows. The
/// `--no-enrich` keeps the read 0-token; any failure degrades to an empty
/// fleet (the heartbeat then reports "quiet").
async fn fetch_needs() -> Result<Vec<NeedsRow>> {
    let bin = std::env::var("AINB_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string))
            .unwrap_or_else(|| "ainb".to_string())
    });
    let out = tokio::process::Command::new(&bin)
        .args(["--format", "json", "fleet", "needs", "--no-enrich"])
        .output()
        .await
        .context("invoking `ainb fleet needs`")?;
    if !out.status.success() {
        bail!("`ainb fleet needs` exited {}", out.status);
    }
    let rows: Vec<NeedsRow> =
        serde_json::from_slice(&out.stdout).context("parsing `fleet needs` JSON")?;
    Ok(rows)
}

// --- helpers ----------------------------------------------------------------

fn require_name(matches: &clap::ArgMatches) -> Result<String> {
    matches.get_one::<String>("name").cloned().context("missing <name> argument")
}

fn seed_state_json() -> String {
    serde_json::to_string_pretty(&json!({
        "last_heartbeat_ms": 0,
        "retry_counts": {},
        "escalated": {},
        "notes": {}
    }))
    .unwrap_or_else(|_| "{}".into())
}

fn seed_task_log(name: &str) -> String {
    format!(
        "# ATC task log — {name}\n\n\
This is ATC's durable, human-readable memory. Append one dated line per action\n\
(cleared / escalated / skipped) so decisions survive context compaction.\n\n\
- provisioned\n"
    )
}
