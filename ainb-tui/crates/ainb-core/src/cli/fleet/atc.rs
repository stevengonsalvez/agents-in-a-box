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
    AtcMeta, AtcPaths, DEFAULT_ERR_RETRY_CAP, HeartbeatState, build_heartbeat_enforcing_cap,
    render_claude_md, should_pause_for_idle, timer,
};
use crate::fleet::plumbing;
use crate::fleet::read::NeedsRow;
use crate::fleet::send::{tmux_send, tmux_session_exists};

pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("setup", sub)) => setup(sub, format).await,
        Some(("teardown", sub)) => teardown(sub, format).await,
        Some(("status", sub)) => status(sub, format).await,
        Some(("list", _)) => list(format).await,
        Some(("heartbeat", sub)) => heartbeat(sub, format).await,
        Some(("hook", sub)) => hook(sub).await,
        Some(("inbox", sub)) => inbox(sub, format).await,
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

    // Install the event-driven lifecycle hooks into Claude's settings.json
    // (read-preserve-modify-write: keeps reflect/notifyd/user hooks). This is
    // what upgrades managed sessions from poll-mode to event-driven — a child's
    // Stop hook commits its completion to the parent's inbox. Opt out with
    // `--no-hooks` (tests / poll-only deployments).
    let mut hooks_installed = false;
    if !matches.get_flag("no-hooks") {
        if let Some(home) = dirs::home_dir() {
            let script = ainb_plugin_notifyd::install::canonical_hook_script(
                &ainb_plugin_notifyd::paths::Paths::under(home.join(".agents-in-a-box")),
            );
            // Ensure the canonical script exists on disk before pointing hooks
            // at it (no-op if a prior notifyd install already extracted it).
            let paths_nd = ainb_plugin_notifyd::paths::Paths::under(home.join(".agents-in-a-box"));
            let _ = ainb_plugin_notifyd::install::extract_hook_script(&paths_nd);
            match plumbing::settings::install_claude_hooks(&home, &script) {
                Ok(_) => hooks_installed = true,
                Err(e) => tracing::warn!("failed to install lifecycle hooks: {e}"),
            }
        }
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
        "lifecycle_hooks_installed": hooks_installed,
    });

    if matches!(format, OutputFormat::Text) {
        println!("ATC '{name}' provisioned.");
        println!("  dir:       {}", paths.dir.display());
        println!("  policy:    {}", paths.claude_md.display());
        println!("  session:   {} (spawned: {spawned})", meta.tmux_session());
        println!("  hooks:     lifecycle hooks installed: {hooks_installed}");
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
    let bin = atc_bin();
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

    // Best-effort kill the running session. Use the same sanitization the
    // spawner applies so we target the right session for unsafe names.
    let meta_session = crate::tmux::sanitize_session_name(&name);
    let mut killed = false;
    if tmux_session_exists(&meta_session).await {
        let bin = atc_bin();
        // `--force` is mandatory: without it, `ainb kill` prints a `[y/N]`
        // prompt, reads EOF non-interactively, cancels, and leaves the session
        // running. We also check the real exit status instead of assuming
        // success, so `killed` reflects what actually happened.
        let output = tokio::process::Command::new(&bin)
            .args(["kill", "--force", &name])
            .output()
            .await
            .context("invoking `ainb kill --force`")?;
        killed = output.status.success();
    }

    // Remove the instance dir only when --purge (default keeps state/task-log).
    let paths = AtcPaths::resolve(&name)?;
    let mut purged = false;
    if purge && paths.dir.exists() {
        std::fs::remove_dir_all(&paths.dir)
            .with_context(|| format!("purging ATC dir {}", paths.dir.display()))?;
        purged = true;
    }

    // Uninstall the lifecycle hooks only when this was the LAST ATC instance —
    // another instance may still rely on them. Strips exactly the ATC-managed
    // block, leaving reflect/notifyd/user hooks intact.
    let mut hooks_uninstalled = false;
    let remaining = crate::fleet::atc::paths::list_instance_names().unwrap_or_default();
    let none_left = remaining.iter().all(|n| n == &name);
    if none_left {
        if let Some(home) = dirs::home_dir() {
            match plumbing::settings::uninstall_claude_hooks(&home) {
                Ok(()) => hooks_uninstalled = true,
                Err(e) => tracing::warn!("failed to uninstall lifecycle hooks: {e}"),
            }
        }
    }

    let summary = json!({
        "action": "teardown",
        "name": name,
        "timer_units_removed": removed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "session_killed": killed,
        "dir_purged": purged,
        "lifecycle_hooks_uninstalled": hooks_uninstalled,
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

    // Load the heartbeat process's OWN bookkeeping file. This is a single-writer
    // file (only the heartbeat writes it), separate from state.json which the
    // ATC Claude session owns — so the two writers never clobber each other.
    let mut hb_state = read_heartbeat_state(&paths);

    // Idle-pause: when the fleet has been quiet past the threshold, downgrade to
    // a cheap idle ping so ATC spends no tokens. `last_active_ms` is the last
    // time the fleet had something needing attention.
    let last_active_ms = hb_state.last_active_ms;
    let paused = should_pause_for_idle(rows.len(), last_active_ms, meta.idle_pause_min, now_ms);

    // Build the body. The cap is CODE-ENFORCED here: build_heartbeat_enforcing_cap
    // owns continue_counts (in hb_state) and presents any ERR past the cap as
    // ESCALATE-ONLY regardless of model discipline, then mutates hb_state in place.
    // EVENT-DRIVEN PATH: drain the ATC session's own durable inbox first. When
    // the plumbing is present, child sessions commit their completions here the
    // instant they finish (via their Stop hook), so ATC learns about them
    // without waiting for — or re-deriving from — the poll-mode `fleet needs`
    // scan. Completions are exactly-once: a record drained here is never
    // re-delivered. The poll-mode body below stays as the always-on fallback,
    // so ATC works identically whether or not any child has the hooks installed.
    let inbox = plumbing::Inbox::open_in(
        &plumbing::paths::inbox_dir_in(&plumbing::paths::ainb_home()?),
        &meta.name,
    );
    let completions = inbox.drain().unwrap_or_default();

    // A pending completion overrides idle-pause: a finished child wakes ATC even
    // during a quiet window. So the *effective* pause is "quiet AND nothing
    // arrived event-driven".
    let effective_paused = paused && completions.is_empty();

    let body = if effective_paused {
        format!(
            "[HEARTBEAT {now_ms}] fleet idle-paused (quiet ≥ {}m) — standing by, no token spend.",
            meta.idle_pause_min
        )
    } else {
        let mut b =
            build_heartbeat_enforcing_cap(&rows, now_ms, DEFAULT_ERR_RETRY_CAP, &mut hb_state);
        if !completions.is_empty() {
            // Prepend the event-driven completions so ATC handles the freshly
            // finished children before the polled roster.
            let header = plumbing::format_completions(&completions);
            b = format!("[HEARTBEAT {now_ms}] event-driven completions:\n{header}\n\n{b}");
        }
        b
    };

    // If the session is gone, do not send into a dead pane — report and exit 0
    // so the timer keeps firing harmlessly until teardown.
    let session_live = tmux_session_exists(&tmux).await;
    let should_deliver = !effective_paused;
    let mut delivered = false;
    if session_live && should_deliver {
        tmux_send(&tmux, &body).await.context("sending heartbeat into ATC session")?;
        delivered = true;
    }

    // Persist heartbeat bookkeeping (continue_counts already mutated above) so
    // the idle-pause window + code-enforced cap survive across timer firings.
    hb_state.last_heartbeat_ms = now_ms;
    hb_state.last_active_ms = if rows.is_empty() {
        last_active_ms.or(Some(now_ms))
    } else {
        Some(now_ms)
    };
    write_heartbeat_state(&paths, &hb_state);

    let summary = json!({
        "action": "heartbeat",
        "name": name,
        "needs_count": rows.len(),
        "completions": completions.len(),
        "session_live": session_live,
        "idle_paused": effective_paused,
        "delivered": delivered,
        "now_ms": now_ms,
    });

    if matches!(format, OutputFormat::Text) {
        if effective_paused {
            println!("[atc/{name}] fleet idle-paused — heartbeat downgraded to standby");
        } else if delivered {
            println!(
                "[atc/{name}] heartbeat delivered — {} need(s), {} event-driven completion(s)",
                rows.len(),
                completions.len()
            );
        } else {
            println!("[atc/{name}] session not live — heartbeat skipped");
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    }
    Ok(())
}

/// Read the heartbeat process's own bookkeeping (`heartbeat-state.json`).
/// Missing/corrupt → defaults (idle-pause then stays conservative; cap counters
/// start fresh). This file is single-writer: only the heartbeat touches it.
fn read_heartbeat_state(paths: &AtcPaths) -> HeartbeatState {
    std::fs::read_to_string(&paths.heartbeat_state)
        .ok()
        .map(|s| HeartbeatState::from_json_or_default(&s))
        .unwrap_or_default()
}

/// Persist the heartbeat process's own bookkeeping. Best-effort: a write failure
/// degrades the next firing to conservative defaults rather than aborting.
fn write_heartbeat_state(paths: &AtcPaths, state: &HeartbeatState) {
    if let Ok(s) = state.to_json() {
        let _ = std::fs::write(&paths.heartbeat_state, s);
    }
}

/// Shell `ainb fleet needs --no-enrich --format json` and parse the rows. The
/// `--no-enrich` keeps the read 0-token; any failure degrades to an empty
/// fleet (the heartbeat then reports "quiet").
async fn fetch_needs() -> Result<Vec<NeedsRow>> {
    let bin = atc_bin();
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

// --- hook (internal, called by the installed hook script) -------------------

/// `ainb fleet atc hook` — the Rust side of the lifecycle hook.
///
/// Invoked by `notify.sh` on every managed lifecycle event with the parsed
/// `--event` / `--session-id` / `--cwd` and the original hook payload on stdin.
/// It does the durable work the shell can't:
///
///   1. Write the session's atomic status file for this event.
///   2. On `UserPromptSubmit` (a genuine user turn) reset this session's
///      Stop-drain block budget.
///   3. On `Stop`: (a) if the session has a parent, commit a last-wins
///      completion to the parent's inbox (so the parent learns it finished);
///      (b) drain the session's OWN inbox — if it carries child completions and
///      the block budget allows, print `{"decision":"block","reason":...}` to
///      stdout so the completions become this session's next turn.
///
/// Empty inbox = no block + no writes beyond the status file (the leaf fast
/// path). Always exits 0 with at most the decision JSON on stdout so a failure
/// never wedges the host agent.
async fn hook(matches: &clap::ArgMatches) -> Result<()> {
    let event = matches.get_one::<String>("event").cloned().unwrap_or_default();
    let session_id = matches.get_one::<String>("session-id").cloned().unwrap_or_default();
    let home = plumbing::paths::ainb_home()?;
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Read the raw payload from stdin (best-effort; used to extract a summary).
    let payload = read_stdin_to_string();
    let done_summary = extract_done_summary(&payload);

    // 1. Status file (every event). Best-effort: never abort the hook on a
    //    status-write failure.
    if !session_id.is_empty() {
        let rec =
            plumbing::StatusFile::from_event(&session_id, &event, now_ms, done_summary.clone());
        let _ = plumbing::status::write_status_in(&home, &rec);
    }

    let base_event = event.split(':').next().unwrap_or(&event);

    // 2. A genuine user turn resets this session's consecutive-block budget.
    if base_event == "UserPromptSubmit" && !session_id.is_empty() {
        let inbox = plumbing::Inbox::open_in(&plumbing::paths::inbox_dir_in(&home), &session_id);
        let mut budget = inbox.read_budget();
        budget.reset();
        let _ = inbox.write_budget(&budget);
    }

    // 3. Stop: route this session's completion up to its parent, then drain its
    //    own inbox for child completions.
    if base_event == "Stop" && !session_id.is_empty() {
        // (a) Route our completion. With a resolvable parent it lands in that
        //     parent's durable inbox; without one it dead-letters (a managed
        //     session's completion is never silently dropped). A genuine leaf
        //     that is also an orphan still records, so an operator can audit it.
        let env_parent = std::env::var(plumbing::PARENT_ENV).ok();
        let parent_id = plumbing::resolve_parent_in(&home, &session_id, env_parent.as_deref())
            .unwrap_or_default();
        let summary = done_summary.clone().unwrap_or_default();
        let rec = plumbing::InboxRecord::new(&session_id, &parent_id, summary, &event, now_ms);
        let _ = plumbing::commit_completion(&home, &rec);

        // (b) Drain our own inbox — are we a parent with finished children?
        let inbox = plumbing::Inbox::open_in(&plumbing::paths::inbox_dir_in(&home), &session_id);
        if !inbox.is_empty() {
            let mut budget = inbox.read_budget();
            let completions = inbox.drain()?;
            if let Some(decision) = plumbing::decide(
                &completions,
                budget.consecutive_blocks,
                plumbing::DEFAULT_BLOCK_BUDGET,
            ) {
                budget.record_block();
                let _ = inbox.write_budget(&budget);
                // The block JSON on stdout is what feeds the completions back
                // into this session as its next turn.
                println!("{}", serde_json::to_string(&decision)?);
            }
        }
    }

    Ok(())
}

// --- inbox (operator-facing: inspect / drain / commit) ----------------------

/// `ainb fleet atc inbox {peek|drain|commit}` — operate on a parent's durable
/// inbox directly. Useful for debugging and for tests/integration to exercise
/// the event-driven path without a live session.
async fn inbox(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("peek", sub)) => inbox_peek(sub, format),
        Some(("drain", sub)) => inbox_drain(sub, format),
        Some(("commit", sub)) => inbox_commit(sub, format),
        _ => bail!("unknown `ainb fleet atc inbox` subcommand — try peek | drain | commit"),
    }
}

fn inbox_handle(matches: &clap::ArgMatches) -> Result<plumbing::Inbox> {
    let parent = matches.get_one::<String>("parent").context("missing <parent> argument")?;
    let home = plumbing::paths::ainb_home()?;
    Ok(plumbing::Inbox::open_in(
        &plumbing::paths::inbox_dir_in(&home),
        parent,
    ))
}

fn inbox_peek(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let records = inbox_handle(matches)?.peek();
    if matches!(format, OutputFormat::Text) {
        if records.is_empty() {
            println!("inbox empty");
        } else {
            for r in &records {
                println!("[{}] {} — {}", r.child_id, r.event, r.summary);
            }
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&records)?);
    }
    Ok(())
}

fn inbox_drain(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let inbox = inbox_handle(matches)?;
    let mut budget = inbox.read_budget();
    let completions = inbox.drain()?;
    let decision = plumbing::decide(
        &completions,
        budget.consecutive_blocks,
        plumbing::DEFAULT_BLOCK_BUDGET,
    );
    if decision.is_some() {
        budget.record_block();
        let _ = inbox.write_budget(&budget);
    }
    if matches!(format, OutputFormat::Text) {
        println!("drained {} completion(s)", completions.len());
        if let Some(d) = &decision {
            println!("{}", d.reason);
        }
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "drained": completions,
                "decision": decision,
            }))?
        );
    }
    Ok(())
}

fn inbox_commit(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let child = matches.get_one::<String>("child").context("missing --child")?;
    let parent = matches.get_one::<String>("parent").context("missing <parent> argument")?;
    let summary = matches.get_one::<String>("summary").cloned().unwrap_or_default();
    let home = plumbing::paths::ainb_home()?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let rec = plumbing::InboxRecord::new(child, parent, summary, "Stop", now_ms);
    let routed = plumbing::commit_completion(&home, &rec)?;
    if matches!(format, OutputFormat::Text) {
        println!(
            "committed completion from {child} → {} ({})",
            parent,
            if routed {
                "parent inbox"
            } else {
                "dead-letter"
            }
        );
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "routed": routed, "record": rec }))?
        );
    }
    Ok(())
}

/// Read all of stdin to a string (best-effort; empty on any error). The hook
/// payload is piped in by `notify.sh`.
fn read_stdin_to_string() -> String {
    use std::io::Read;
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    buf
}

/// Extract a one-line completion summary from a hook payload. Claude's Stop
/// payload doesn't carry the assistant text directly, so we look for the common
/// fields and otherwise return None (the status file simply omits a summary).
fn extract_done_summary(payload: &str) -> Option<String> {
    if payload.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    for key in ["last_assistant_message", "summary", "message", "reason"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.chars().take(200).collect());
            }
        }
    }
    None
}

// --- helpers ----------------------------------------------------------------

fn require_name(matches: &clap::ArgMatches) -> Result<String> {
    matches.get_one::<String>("name").cloned().context("missing <name> argument")
}

/// Resolve the `ainb` binary to shell out to: `$AINB_BIN` if set (tests point it
/// at a fake / the test binary), else this executable, else the literal `ainb`
/// on `$PATH`.
fn atc_bin() -> String {
    std::env::var("AINB_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string))
            .unwrap_or_else(|| "ainb".to_string())
    })
}

/// Seed state.json — the ATC Claude session's single-writer durable state.
/// Heartbeat bookkeeping (last_heartbeat_ms / last_active_ms / continue_counts)
/// deliberately lives in a separate heartbeat-owned file, not here.
fn seed_state_json() -> String {
    serde_json::to_string_pretty(&json!({
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
