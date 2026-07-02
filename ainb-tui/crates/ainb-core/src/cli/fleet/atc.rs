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
    // idempotent and picks up policy/template changes on re-run. All four durable
    // artefacts are written through `write_atomic` (M-A3): a crash mid-write
    // would otherwise leave a torn file that status/list/heartbeat then error on.
    plumbing::atomic::write_atomic(&paths.claude_md, render_claude_md(&meta).as_bytes())
        .context("writing CLAUDE.md")?;
    plumbing::atomic::write_atomic(&paths.meta, meta.to_json()?.as_bytes())
        .context("writing meta.json")?;

    // Seed durable memory only if absent (never clobber accumulated state).
    if !paths.state.exists() {
        plumbing::atomic::write_atomic(&paths.state, seed_state_json().as_bytes())
            .context("seeding state.json")?;
    }
    if !paths.task_log.exists() {
        plumbing::atomic::write_atomic(&paths.task_log, seed_task_log(&name).as_bytes())
            .context("seeding task-log.md")?;
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
///
/// INVARIANT (L1): N concurrent ATC instances are SAFE precisely because each
/// instance consumes its OWN per-parent inbox (`inbox/<name>.jsonl`) and every
/// drain is exactly-once (turn-fingerprint consumed marker). There is NO shared
/// queue between ATCs. A future maintainer must NOT introduce a shared/global
/// completion queue without also adding a lock + exactly-once keying — doing so
/// would reintroduce the cross-instance lost-update / double-delivery the
/// per-parent design avoids.
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

    // Heartbeat staleness (M-A1): the heartbeat stamps `last_heartbeat_ms` every
    // firing. If `now - last_heartbeat_ms` is much larger than the interval, the
    // timer has stalled (e.g. the machine slept and the launchd StartInterval
    // could not catch up) — so a "session running" ATC can still be effectively
    // dead. We flag stale when the gap exceeds 3× the interval, and surface the
    // last-firing age so the operator isn't misled into thinking ATC is alive.
    let hb_state = read_heartbeat_state(&paths);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let interval_ms = i64::from(meta.heartbeat_interval_min.max(1)) * 60_000;
    let (last_heartbeat_ms, heartbeat_age_ms, heartbeat_stale) = if hb_state.last_heartbeat_ms > 0 {
        let age = (now_ms - hb_state.last_heartbeat_ms).max(0);
        // Only meaningful when a timer is supposed to be firing.
        let stale = meta.heartbeat_enabled && age > interval_ms.saturating_mul(3);
        (Some(hb_state.last_heartbeat_ms), Some(age), stale)
    } else {
        // Never fired yet: not "stale", just pending.
        (None, None, false)
    };

    let summary = json!({
        "name": meta.name,
        "dir": paths.dir.display().to_string(),
        "heartbeat_enabled": meta.heartbeat_enabled,
        "heartbeat_interval_min": meta.heartbeat_interval_min,
        "idle_pause_min": meta.idle_pause_min,
        "timer_installed": timer_installed,
        "session_running": session_running,
        "tmux_session": meta.tmux_session(),
        "last_heartbeat_ms": last_heartbeat_ms,
        "heartbeat_age_ms": heartbeat_age_ms,
        "heartbeat_stale": heartbeat_stale,
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
        match heartbeat_age_ms {
            Some(age) => {
                let mins = age / 60_000;
                if heartbeat_stale {
                    println!(
                        "  last beat: {mins}m ago — ⚠ STALE (timer stalled? expected every {}m)",
                        meta.heartbeat_interval_min
                    );
                } else {
                    println!("  last beat: {mins}m ago");
                }
            }
            None => println!("  last beat: never (no heartbeat recorded yet)"),
        }
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
    // reuses the exact same reader the operator sees. As of Wave 4 that reader
    // is event-sourced: `fleet needs` reads the materialized `current_state`
    // table (hook-backed sessions) and falls back to a live tmux/transcript
    // `classify()` only for sessions absent from / tmux-sourced in
    // `current_state` (non-Claude agents, transient errors). So the heartbeat's
    // coarse session state is now `current_state`-backed without any direct
    // SQLite access here — and the exactly-once inbox drain below is UNCHANGED.
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

    // EVENT-DRIVEN PATH: PEEK (do NOT drain yet) the ATC session's own durable
    // inbox. Child sessions spawned `--parent <name>` commit their completions to
    // `inbox/<name>.jsonl` the instant they finish (via their Stop hook), so ATC
    // learns about them without waiting for the poll-mode `fleet needs` scan.
    //
    // CRITICAL (C-A1): we must NOT `drain()` here. `drain()` consumes + clears +
    // records consumed fingerprints — once a fingerprint is in the consumed
    // marker it is NEVER re-delivered. If we drained before confirming the
    // session is live AND the heartbeat is actually deliverable, a dead session
    // or an idle-paused firing would consume the completions and lose them
    // permanently. So we PEEK (non-destructive), build the body, and only
    // `drain()` AFTER a fully-confirmed tmux send (C-A2). On any earlier exit the
    // completions stay in the inbox for a later firing.
    let inbox = plumbing::Inbox::open_in(
        &plumbing::paths::inbox_dir_in(&plumbing::paths::ainb_home()?),
        &meta.name,
    );
    let completions = inbox.peek();

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
            // finished children before the polled roster. (Summaries are fenced
            // as untrusted DATA inside `format_completions` — see M3.)
            // Whitespace-collapsed to keep the delivered body a SINGLE line:
            // any embedded newline turns the send into a bracketed paste that
            // can sit unsubmitted in a busy composer (see heartbeat.rs).
            let header = plumbing::format_completions(&completions);
            let header = header.split_whitespace().collect::<Vec<_>>().join(" ");
            b = format!("[HEARTBEAT {now_ms}] event-driven completions: {header} {b}");
        }
        b
    };

    // Check liveness + deliverability FIRST (C-A1). Only when delivery is
    // guaranteed do we touch the inbox.
    let session_live = tmux_session_exists(&tmux).await;
    let should_deliver = !effective_paused;
    let mut delivered = false;
    let mut skipped_pending = false;
    if session_live && should_deliver {
        // COALESCE, never stack: if the previous nudge is still sitting
        // unsubmitted in the composer (busy/stalled session), injecting
        // another would pile pastes on top — the exact failure that stacked 8
        // heartbeats in one composer. Heartbeats are idempotent fleet
        // snapshots, so skip this tick after one flush attempt (a lone Enter,
        // which submits the parked nudge if the session recovered). The next
        // tick delivers a FRESHER body anyway. Completions stay in the inbox
        // (never drained on a skip) per C-A1.
        if crate::fleet::send::pane_has_unsubmitted_input(&tmux).await {
            let _ = crate::fleet::send::tmux_press_enter(&tmux).await;
            skipped_pending = true;
            tracing::warn!(
                "atc heartbeat: prior nudge unsubmitted in {tmux} — flushed Enter, skipped this tick"
            );
        } else {
            // C-A2: send BEFORE drain. The send result decides whether we drain.
            let send_result = tmux_send(&tmux, &body).await;
            delivered = send_result.is_ok();
            // Encapsulated decision (unit-tested): drain exactly-once ONLY on a
            // confirmed send; on send failure leave the completions for a later
            // firing rather than consuming + losing them.
            commit_delivery_on_send(&inbox, &completions, &send_result);
            if let Err(e) = send_result {
                tracing::warn!("atc heartbeat: tmux send failed, completions retained: {e}");
            }
        }
    }
    // If not live / not deliverable: we never drained — the completions stay in
    // the inbox so a later, deliverable firing handles them (C-A1).

    // Persist heartbeat bookkeeping in EVERY branch (C-A2) — continue_counts were
    // mutated above, and last_heartbeat_ms/last_active_ms must advance even when
    // the send failed or the session was dead, so the idle-pause window + the
    // code-enforced cap survive across timer firings.
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
        "skipped_pending": skipped_pending,
        "now_ms": now_ms,
    });

    if matches!(format, OutputFormat::Text) {
        if effective_paused {
            println!("[atc/{name}] fleet idle-paused — heartbeat downgraded to standby");
        } else if skipped_pending {
            println!(
                "[atc/{name}] prior nudge still unsubmitted — flushed Enter, heartbeat skipped"
            );
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

/// Drain the heartbeat snapshot exactly-once IFF it was actually delivered.
///
/// This is the C-A1/C-A2 invariant in one place: peeked completions are consumed
/// ONLY after a confirmed send (`send_result.is_ok()`). Crucially, the drain is
/// limited to `sent_completions`; a child can commit another completion between
/// the heartbeat body being built and the successful tmux send, and that later
/// record must stay pending for the next heartbeat.
fn commit_delivery_on_send<E>(
    inbox: &plumbing::Inbox,
    sent_completions: &[plumbing::InboxRecord],
    send_result: &std::result::Result<(), E>,
) {
    if !sent_completions.is_empty() && send_result.is_ok() {
        let _ = inbox.drain_matching(sent_completions);
    }
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
/// Written atomically (M-A3) so a crash mid-write never leaves a torn
/// heartbeat-state.json that the next firing would fail to parse.
fn write_heartbeat_state(paths: &AtcPaths, state: &HeartbeatState) {
    if let Ok(s) = state.to_json() {
        let _ = plumbing::atomic::write_atomic(&paths.heartbeat_state, s.as_bytes());
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
///   1. Append the event to `<home>/events.jsonl` (the durable, daemon-down-safe
///      record notifyd ingests into the event log + materializes into
///      `current_state`). This REPLACED the retired per-event
///      `status/<session_id>.json` write.
///   2. On `UserPromptSubmit` (a genuine user turn) reset this session's
///      Stop-drain block budget.
///   3. On `Stop`: (a) if the session has a parent, commit a last-wins
///      completion to the parent's inbox (so the parent learns it finished);
///      (b) drain the session's OWN inbox — if it carries child completions and
///      the block budget allows, print `{"decision":"block","reason":...}` to
///      stdout so the completions become this session's next turn.
///
/// Empty inbox = no block + no writes beyond the event-log append (the leaf fast
/// path). Always exits 0 with at most the decision JSON on stdout so a failure
/// never wedges the host agent.
///
/// **Never returns Err (H-A1).** The lifecycle hooks are installed GLOBALLY into
/// `~/.claude/settings.json`, so this verb runs on EVERY Claude `Stop` on the
/// host — including hundreds of sessions unrelated to any fleet. If it returned
/// Err, `main()`'s `Result` would make the process exit NON-ZERO, and a
/// non-zero Stop-hook exit can disrupt Stop on every unrelated session. So the
/// real work runs in [`hook_inner`], whose errors are logged-and-swallowed here;
/// this function always emits valid stdout (the decision JSON or nothing) and
/// returns `Ok(())` → exit 0.
async fn hook(matches: &clap::ArgMatches) -> Result<()> {
    let (_exit_ok, emitted) = swallow_hook_result(hook_inner(matches));
    if let Some(json) = emitted {
        // The block JSON on stdout feeds the completions back into this session
        // as its next turn.
        println!("{json}");
    }
    // ALWAYS Ok → exit 0, regardless of what hook_inner did (H-A1).
    Ok(())
}

/// What the lifecycle hook emits on stdout. Two distinct JSON shapes flow to the
/// same stdout: a `Stop`-block decision (`{"decision":"block",...}`) and the
/// `PermissionRequest` `hookSpecificOutput` decision. The permission line is
/// pre-serialized (built by [`permission_emit_json`]) and emitted verbatim.
enum HookEmit {
    /// A Stop-drain block decision, serialized on emit.
    Stop(plumbing::StopDecision),
    /// A pre-serialized `hookSpecificOutput` permission-decision JSON line.
    Permission(String),
}

/// Convert a [`hook_inner`]/[`hook_core`] result into the hook's exit behaviour:
/// `(exit_ok, emitted_stdout)`. This is the H-A1 swallow contract in one place —
/// it NEVER yields a non-zero exit. A block decision serializes to the stdout
/// JSON; a permission decision is emitted verbatim; a `None` decision emits
/// nothing; an Err is logged and swallowed (no stdout). `exit_ok` is always
/// `true` (the tuple keeps the intent explicit + unit-testable).
fn swallow_hook_result(result: Result<Option<HookEmit>>) -> (bool, Option<String>) {
    match result {
        Ok(Some(HookEmit::Stop(decision))) => match serde_json::to_string(&decision) {
            Ok(s) => (true, Some(s)),
            Err(e) => {
                // Serialization of a StopDecision cannot realistically fail, but
                // if it ever did we still exit 0 with no stdout rather than
                // propagate.
                tracing::warn!("atc hook: failed to serialize decision: {e}");
                (true, None)
            }
        },
        // Already a complete JSON line — emit as-is.
        Ok(Some(HookEmit::Permission(json))) => (true, Some(json)),
        Ok(None) => (true, None),
        Err(e) => {
            // Log-and-swallow: a drain/serialize/IO failure must NOT wedge Stop
            // on this (or any other) host session.
            tracing::warn!("atc hook: swallowed error to keep Stop non-blocking: {e}");
            (true, None)
        }
    }
}

/// Build the Claude `PermissionRequest` hook output from a broker decision.
/// `approve` → `allow`; anything else (deny / timeout / superseded / dead
/// socket) → `deny`. Never auto-approves on a fallback.
fn permission_emit_json(decision: &ainb_plugin_notifyd::broker::Decision) -> String {
    use ainb_plugin_notifyd::broker::DecisionKind;
    let verdict = match decision.decision {
        DecisionKind::Approve => "allow",
        DecisionKind::Deny => "deny",
    };
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "permissionDecision": verdict,
            "permissionDecisionReason": decision.reason,
        }
    })
    .to_string()
}

/// Best-effort short context for the permission prompt: the `tool_input` JSON
/// from the hook payload (e.g. the Bash command), compacted to one line. Empty
/// when the payload has none.
fn extract_permission_context(payload: &str) -> String {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v.get("tool_input").cloned())
        .map(|ti| ti.to_string())
        .unwrap_or_default()
}

/// The fallible body of [`hook`]: resolves the process env (`AINB_HOME`,
/// `AINB_PARENT_SESSION`) + stdin payload, then delegates the I/O-on-an-explicit-
/// home work to [`hook_core`] (which is env-free so it unit-tests against a
/// tempdir without mutating process-global env — the crate forbids `unsafe`, so
/// tests can't call `set_var`). Returns the optional stdout emit ([`HookEmit`]).
fn hook_inner(matches: &clap::ArgMatches) -> Result<Option<HookEmit>> {
    let event = matches.get_one::<String>("event").cloned().unwrap_or_default();
    let session_id = matches.get_one::<String>("session-id").cloned().unwrap_or_default();
    let cwd = matches.get_one::<String>("cwd").cloned().unwrap_or_default();
    // The matcher forwarded by notify.sh (e.g. the PreToolUse tool_name or the
    // Notification notification_type). Empty → treated as absent. Authoritative
    // when present; otherwise hook_core falls back to parsing it from the payload.
    let matcher = matches
        .get_one::<String>("matcher")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let home = plumbing::paths::ainb_home()?;
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Read the raw payload from stdin (best-effort; used to extract a summary,
    // the transcript_path stamp, and the matcher fallback).
    let payload = read_stdin_to_string();
    let done_summary = extract_done_summary(&payload);

    // Resolve the live parent linkage from the env (the in-band, zero-lookup
    // signal `ainb run --parent` seeds).
    let env_parent = std::env::var(plumbing::PARENT_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    hook_core(
        &home,
        &event,
        &session_id,
        &cwd,
        env_parent.as_deref(),
        done_summary,
        now_ms,
        &payload,
        matcher.as_deref(),
    )
}

/// Env-free core of the lifecycle hook. All filesystem state is rooted at the
/// explicit `home`, so this is unit-testable against a tempdir.
#[allow(clippy::too_many_arguments)]
fn hook_core(
    home: &std::path::Path,
    event: &str,
    session_id: &str,
    cwd: &str,
    env_parent: Option<&str>,
    done_summary: Option<String>,
    now_ms: i64,
    payload: &str,
    matcher: Option<&str>,
) -> Result<Option<HookEmit>> {
    let base_event = event.split(':').next().unwrap_or(event);
    let inbox_dir = plumbing::paths::inbox_dir_in(home);

    // Resolve the parent of THIS session (env first, then durable map). Used both
    // to route our own completion up AND as one fleet-membership signal.
    let our_parent = if session_id.is_empty() {
        None
    } else {
        plumbing::resolve_parent_in(home, session_id, env_parent)
    };

    // Recognise ATC's OWN session: it was spawned `--repo <atc_root>/<name>`, so
    // its cwd is exactly that provisioned instance dir. This is the canonical
    // parent key for ATC (children are spawned `--parent <name>`, so they commit
    // to `inbox/<name>.jsonl`, NOT `inbox/<atc_session_id>.jsonl`). Resolving it
    // here lets the Stop-drain consume the SAME key the heartbeat consumes
    // (fixes the split-brain in C-A3) and is a fleet-membership signal (H1).
    // The ATC root is always `<home>/atc` (matches `atc::paths::atc_root`).
    let atc_name = if cwd.is_empty() {
        None
    } else {
        let root = home.join("atc");
        crate::fleet::atc::instance_name_for_cwd_in(&root, std::path::Path::new(cwd))
    };

    // Does this session own a non-empty inbox under its session-id key? (A parent
    // whose children committed under the session-id key — the leaf-vs-parent
    // distinction for membership.)
    let self_inbox =
        (!session_id.is_empty()).then(|| plumbing::Inbox::open_in(&inbox_dir, session_id));
    let has_self_inbox = self_inbox.as_ref().is_some_and(|ib| !ib.is_empty());

    // 1. Event log append — the durable record this session's lifecycle is
    //    event-sourced from. Replaces the per-event status/<id>.json write: notifyd
    //    ingests events.jsonl into the SQLite `events` table, and (Wave 3) folds it
    //    into current_state. The append is O(1), best-effort, and NEVER fails the
    //    hook (any error is swallowed) so a global Stop hook can't wedge any host
    //    session.
    //
    //    GATED ON FLEET MEMBERSHIP (H1) — the CONSERVATIVE default. The hooks are
    //    installed globally into ~/.claude/settings.json, so this verb runs on
    //    EVERY Claude lifecycle event host-wide. To honour the host-wide-cheap
    //    rule we append only for fleet members (a session with a resolvable
    //    parent, a non-empty inbox, or an ATC-managed cwd); a truly unrelated host
    //    session stays a true no-op. The appender below is trivially cheap, so if
    //    Stevie wants broad event-sourcing coverage this gate can be dropped (or
    //    relaxed to a lightweight lifecycle subset) by widening `is_fleet_member`
    //    — the format + tables already accept any session.
    let is_fleet_member = our_parent.is_some() || has_self_inbox || atc_name.is_some();
    if !session_id.is_empty() && is_fleet_member {
        let line = build_event_line(
            now_ms,
            session_id,
            cwd,
            base_event,
            matcher,
            payload,
            our_parent.as_deref(),
        );
        let _ = append_event_line(home, &line);
    }

    // Self-heal the durable child→parent map. `ainb run` seeds only the live
    // AINB_PARENT_SESSION env (claude mints its own session id, so a map keyed by
    // ainb's spawn-time Uuid would never match the id the hook reports). Here we
    // key the durable fallback by the id the hook ACTUALLY sees, so linkage
    // survives env loss (restart / resume) and `resolve_parent_in` can find it.
    // Idempotent (last-write-wins) and cheap; only when both ids are present.
    if !session_id.is_empty() {
        if let Some(parent) = env_parent {
            let _ = plumbing::record_parent_in(home, session_id, parent);
        }
    }

    // 2. A genuine user turn resets the consecutive-block budget. The ATC session
    //    drains its CANONICAL key (atc_name), so reset that key's budget; a plain
    //    parent uses its session-id key. The reset is under the inbox lock so it
    //    serialises against a concurrent Stop-drain's block increment.
    if base_event == "UserPromptSubmit" {
        if let Some(name) = atc_name.as_deref() {
            let _ = plumbing::Inbox::open_in(&inbox_dir, name).reset_budget();
        }
        if !session_id.is_empty() {
            let _ = plumbing::Inbox::open_in(&inbox_dir, &session_id).reset_budget();
        }
    }

    // 3. Stop: route this session's completion up to its parent, then drain for
    //    child completions.
    if base_event == "Stop" && !session_id.is_empty() {
        // (a) Route our completion ONLY when this session has a resolvable parent
        //     (zero durable writes for a truly unrelated host session — see H1).
        if let Some(parent_id) = our_parent.as_deref() {
            let summary = done_summary.clone().unwrap_or_default();
            let rec = plumbing::InboxRecord::new(session_id, parent_id, summary, event, now_ms);
            let _ = plumbing::commit_completion(home, &rec);
        }

        // (b) Drain for finished children. C-A3: ATC's children commit to the
        //     CANONICAL `atc_name` key (because they were spawned `--parent
        //     <name>`), so when this IS the ATC session we drain that key — the
        //     SAME file the heartbeat drains — so the synchronous Stop-drain and
        //     the timer never miss each other's children. A plain (non-ATC)
        //     parent drains its session-id key. We pick exactly ONE canonical key
        //     per session and drain it under the budget so exactly-once holds.
        let drain_key = atc_name.clone().unwrap_or_else(|| session_id.to_string());
        let inbox = plumbing::Inbox::open_in(&inbox_dir, &drain_key);
        if !inbox.is_empty() {
            let (_completions, decision) =
                inbox.drain_with_budget(plumbing::DEFAULT_BLOCK_BUDGET)?;
            return Ok(decision.map(HookEmit::Stop));
        }
    }

    // 4. PermissionRequest: SYNCHRONOUS approve/deny round-trip. The waiting
    //    Claude hook BLOCKS here on the approve broker socket until a human
    //    answers from the fleet UI (or `ainb fleet ... approve/deny`), then
    //    relays the verdict straight back as a `hookSpecificOutput` permission
    //    decision. Fleet members only — an unrelated host session must NOT wedge
    //    on our socket. `client_await` re-dials to its deadline, so a notifyd
    //    restart mid-wait is survived; a dead socket / no answer deny-falls-back
    //    (never auto-approves).
    if base_event == "PermissionRequest" && !session_id.is_empty() && is_fleet_member {
        let sock = ainb_plugin_notifyd::paths::Paths::under(home).approve_socket;
        let tool = matcher.unwrap_or_default();
        let context = extract_permission_context(payload);
        let decision = ainb_plugin_notifyd::broker::client_await(
            &sock,
            session_id,
            tool,
            &context,
            ainb_plugin_notifyd::broker::CLIENT_AWAIT_DEADLINE,
        );
        return Ok(Some(HookEmit::Permission(permission_emit_json(&decision))));
    }

    Ok(None)
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

/// Max payload bytes embedded in an `events.jsonl` line.
///
/// CRITICAL (PIPE_BUF atomicity): `events.jsonl` is appended concurrently by
/// every host Claude session's Stop/PreToolUse hook via a single `write()` of
/// one line. POSIX guarantees a `write()` is atomic (no interleaving / torn
/// lines) only when it is `≤ PIPE_BUF` (4096 bytes, the portable floor). A line
/// larger than that can tear under concurrent host-wide fires, corrupting the
/// log the notifyd ingest tailer parses. So the WHOLE canonical line — envelope
/// fields (ts, session_id, cwd, transcript_path, agent, event_type, matcher,
/// parent) + the embedded payload + the trailing `\n` — must stay ≤ 4096 bytes.
/// We cap the payload at 3 KiB, leaving ~1 KiB of headroom for the envelope; a
/// larger payload is replaced by a `_truncated` marker so the line stays small
/// and always-valid JSON. This also keeps the durable log bounded.
const MAX_EVENT_PAYLOAD_BYTES: usize = 3 * 1024;

/// Extract the universal `transcript_path` field from an already-parsed hook
/// payload (every Claude Code 2.1.19 hook carries it). Empty when absent. Takes
/// the parsed `Value` so the host-hot-path doesn't re-parse the payload per
/// field (it is parsed ONCE in `build_event_line`).
fn extract_transcript_path(payload: Option<&serde_json::Value>) -> String {
    payload
        .and_then(|v| v.get("transcript_path").and_then(|x| x.as_str()).map(str::to_string))
        .unwrap_or_default()
}

/// Resolve the event's `matcher` discriminator. Prefers the explicit value
/// forwarded by notify.sh (`--matcher`); otherwise reads it from the
/// already-parsed payload per event type: PreToolUse → `tool_name`,
/// Notification → `notification_type`, StopFailure → `error_type`. `None` when
/// not applicable / absent. Takes the parsed `Value` (parsed ONCE upstream).
fn resolve_matcher(
    base_event: &str,
    explicit: Option<&str>,
    payload: Option<&serde_json::Value>,
) -> Option<String> {
    if let Some(m) = explicit {
        let m = m.trim();
        if !m.is_empty() {
            return Some(m.to_string());
        }
    }
    let v = payload?;
    let key = match base_event {
        "PreToolUse" => "tool_name",
        // The managed hook registers PermissionRequest with matcher "", so the
        // tool shown in `ainb fleet approve` / the TUI detail must come from
        // the payload — without this arm the TOOL column is empty on real fires.
        "PermissionRequest" => "tool_name",
        "Notification" => "notification_type",
        "StopFailure" => "error_type",
        _ => return None,
    };
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Build the canonical `events.jsonl` line for one hook fire. This is the exact
/// shape the notifyd ingest tailer parses (`crate ainb-plugin-notifyd::ingest`)
/// and the Wave 3 materializer consumes: ts, session_id, cwd, transcript_path,
/// agent, event_type (the base event), matcher, and the bounded raw payload.
/// Parent is folded into the payload so the materializer can set current_state.parent.
///
/// The raw payload is parsed exactly ONCE here (host-hot-path: this runs on
/// every Claude lifecycle event for a fleet member) and the parsed `Value` is
/// reused for the matcher + transcript_path fields and the embedded payload.
#[allow(clippy::too_many_arguments)]
fn build_event_line(
    now_ms: i64,
    session_id: &str,
    cwd: &str,
    base_event: &str,
    matcher: Option<&str>,
    payload: &str,
    parent: Option<&str>,
) -> serde_json::Value {
    // Parse the raw payload ONCE; reuse the parsed Value for every derived
    // field. Bounded by MAX_EVENT_PAYLOAD_BYTES so the whole canonical line
    // stays ≤ PIPE_BUF (atomic cross-process append). An over-cap or
    // unparseable payload yields None here and a `_truncated`/`{}` embedded
    // payload below.
    let parsed: Option<serde_json::Value> = if payload.len() <= MAX_EVENT_PAYLOAD_BYTES {
        serde_json::from_str(payload).ok()
    } else {
        None
    };
    let matcher_val = resolve_matcher(base_event, matcher, parsed.as_ref());
    let transcript_path = extract_transcript_path(parsed.as_ref());
    // Bounded raw payload: embed the parsed value if it fit, else a truncation
    // marker so the line stays small and always valid JSON.
    let payload_val: serde_json::Value = if payload.len() <= MAX_EVENT_PAYLOAD_BYTES {
        parsed.clone().unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({ "_truncated": true, "_bytes": payload.len() })
    };
    serde_json::json!({
        "ts": now_ms,
        "session_id": session_id,
        "cwd": cwd,
        "transcript_path": transcript_path,
        "agent": "claude",
        "event_type": base_event,
        "matcher": matcher_val,
        "parent": parent,
        "payload": payload_val,
    })
}

/// Append one canonical event line to `<home>/events.jsonl`. Best-effort and
/// O(1): a single line write to an append-handle. NEVER propagates an error —
/// the lifecycle hook must always exit 0, so a full disk / permission failure
/// here is swallowed by the caller (`let _ = …`).
fn append_event_line(home: &std::path::Path, line: &serde_json::Value) -> std::io::Result<()> {
    use std::io::Write;
    let path = home.join("events.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut s = serde_json::to_string(line).unwrap_or_default();
    s.push('\n');
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    f.write_all(s.as_bytes())
}

// --- helpers ----------------------------------------------------------------

/// Extract and SANITIZE the instance `<name>` at the CLI boundary. Sanitizing
/// here (rather than only deep in `AtcPaths`) means the cleaned name is the one
/// stored in `meta.name` and threaded into the timer unit / plist / tmux session
/// names too — so `atc setup ../../foo` can neither escape the ATC root on disk
/// nor inject a traversal string into a launchd/systemd unit name. Every verb
/// (`setup`/`status`/`teardown`/`heartbeat`) sanitizes identically, so they all
/// resolve to the SAME instance. (M-A2)
fn require_name(matches: &clap::ArgMatches) -> Result<String> {
    let raw = matches.get_one::<String>("name").cloned().context("missing <name> argument")?;
    Ok(crate::fleet::atc::paths::sanitize_instance_name(&raw))
}

/// Resolve the `ainb` binary to shell out to: `$AINB_BIN` if set (tests point it
/// at a fake / the test binary), else this executable, else the literal `ainb`
/// on `$PATH`.
pub(crate) fn atc_bin() -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Provision a minimal ATC instance dir (just meta.json) under
    /// `<home>/atc/<name>` and return its path — the cwd an ATC session runs in.
    fn provision_atc(home: &std::path::Path, name: &str) -> std::path::PathBuf {
        let dir = home.join("atc").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), AtcMeta::new(name).to_json().unwrap()).unwrap();
        dir
    }

    fn inbox_for(home: &std::path::Path, key: &str) -> plumbing::Inbox {
        plumbing::Inbox::open_in(&plumbing::paths::inbox_dir_in(home), key)
    }

    // --- C-A1 / C-A2: drain only on a confirmed send -------------------------

    #[test]
    fn commit_delivery_drains_only_on_send_ok() {
        let dir = TempDir::new().unwrap();
        let inbox = inbox_for(dir.path(), "atc");
        inbox
            .commit(&plumbing::InboxRecord::new("c1", "atc", "done", "Stop", 1))
            .unwrap();
        assert!(!inbox.is_empty());

        // Confirmed send → drains exactly-once.
        let completions = inbox.peek();
        let ok: std::result::Result<(), String> = Ok(());
        commit_delivery_on_send(&inbox, &completions, &ok);
        assert!(inbox.is_empty(), "confirmed send must drain the completion");
    }

    #[test]
    fn commit_delivery_retains_on_send_failure() {
        let dir = TempDir::new().unwrap();
        let inbox = inbox_for(dir.path(), "atc");
        inbox
            .commit(&plumbing::InboxRecord::new("c1", "atc", "done", "Stop", 1))
            .unwrap();

        // Send FAILED → must NOT drain; the completion is retained for a later
        // firing (C-A2: a send failure after a would-be drain must not lose it).
        let completions = inbox.peek();
        let err: std::result::Result<(), String> = Err("tmux send failed".into());
        commit_delivery_on_send(&inbox, &completions, &err);
        assert!(
            !inbox.is_empty(),
            "send failure must retain the completion, not consume it"
        );
        assert_eq!(inbox.peek().len(), 1, "the completion is still pending");
    }

    #[test]
    fn commit_delivery_is_noop_when_no_completions() {
        let dir = TempDir::new().unwrap();
        let inbox = inbox_for(dir.path(), "atc");
        let ok: std::result::Result<(), String> = Ok(());
        // No completions → the consumed marker is never touched.
        commit_delivery_on_send(&inbox, &[], &ok);
        assert!(!dir.path().join("inbox").join("atc.consumed").exists());
    }

    #[test]
    fn commit_delivery_drains_only_the_sent_snapshot() {
        let dir = TempDir::new().unwrap();
        let inbox = inbox_for(dir.path(), "atc");
        inbox
            .commit(&plumbing::InboxRecord::new(
                "c1",
                "atc",
                "included in body",
                "Stop",
                1,
            ))
            .unwrap();
        let completions = inbox.peek();
        assert_eq!(completions.len(), 1);

        // A child finishes after the body was built but before the tmux send
        // result is committed. That late completion was not sent to ATC yet.
        inbox
            .commit(&plumbing::InboxRecord::new(
                "c2",
                "atc",
                "late completion",
                "Stop",
                2,
            ))
            .unwrap();

        let ok: std::result::Result<(), String> = Ok(());
        commit_delivery_on_send(&inbox, &completions, &ok);

        let pending = inbox.peek();
        assert_eq!(pending.len(), 1, "late completion must remain pending");
        assert_eq!(pending[0].child_id, "c2");
        assert_eq!(pending[0].summary, "late completion");
    }

    /// C-A1 end-to-end shape: a "dead session" firing never reaches the send, so
    /// `commit_delivery_on_send` is never called and the completion survives.
    /// (Models the heartbeat's `session_live && should_deliver` gate: when false
    /// we skip the deliver/drain block entirely, leaving the inbox intact.)
    #[test]
    fn dead_session_firing_retains_the_completion() {
        let dir = TempDir::new().unwrap();
        let inbox = inbox_for(dir.path(), "tower");
        inbox
            .commit(&plumbing::InboxRecord::new(
                "c1", "tower", "done", "Stop", 1,
            ))
            .unwrap();

        // Simulate the heartbeat's peek (non-destructive) then the dead-session
        // branch: session_live=false → the deliver/drain block is skipped.
        let peeked = inbox.peek();
        assert_eq!(peeked.len(), 1);
        let session_live = false;
        let should_deliver = true;
        if session_live && should_deliver {
            let ok: std::result::Result<(), String> = Ok(());
            commit_delivery_on_send(&inbox, &peeked, &ok);
        }
        // Dead session → completion is STILL pending for a later, live firing.
        assert!(
            !inbox.is_empty(),
            "a dead-session heartbeat must not consume the completion"
        );
        assert_eq!(inbox.peek().len(), 1);
    }

    // --- C-A3: ONE canonical key drained by BOTH consumers -------------------

    /// A child spawned `--parent <name>` commits to `inbox/<name>.jsonl`. The
    /// Stop-hook (when this IS the ATC session, recognised via cwd) drains THAT
    /// key — the same key the heartbeat drains (`meta.name`). So a completion
    /// committed under the parent key is consumed by the Stop-drain, and a second
    /// one by the heartbeat-key drain. Both paths agree on one key, exactly-once.
    #[test]
    fn stop_hook_and_heartbeat_share_the_canonical_atc_key() {
        let home = TempDir::new().unwrap();
        let name = "tower";
        let cwd = provision_atc(home.path(), name);

        // A child committed under the CANONICAL parent key (= the ATC name), as
        // `ainb run --parent tower` routes it.
        inbox_for(home.path(), name)
            .commit(&plumbing::InboxRecord::new(
                "child-1",
                name,
                "did the thing",
                "Stop",
                1,
            ))
            .unwrap();

        // The synchronous Stop-hook on the ATC session drains that key and blocks.
        let decision = hook_core(
            home.path(),
            "Stop",
            "atc-session-id",
            cwd.to_str().unwrap(),
            None,
            None,
            1,
            "",
            None,
        )
        .unwrap();
        let HookEmit::Stop(d) = decision.expect("Stop-hook must block on the child completion")
        else {
            panic!("Stop hook must emit a Stop decision, not a permission decision");
        };
        assert_eq!(d.decision, "block");
        assert!(
            d.reason.contains("child-1"),
            "block carries the child: {}",
            d.reason
        );

        // It is consumed exactly-once: the same key is now empty.
        assert!(inbox_for(home.path(), name).is_empty());

        // A SECOND child completion under the same key is drained by the
        // HEARTBEAT consumer (which also keys by the ATC name) — proving both
        // consumers see children under the one canonical key.
        inbox_for(home.path(), name)
            .commit(&plumbing::InboxRecord::new(
                "child-2",
                name,
                "second turn",
                "Stop",
                2,
            ))
            .unwrap();
        let hb_drained = inbox_for(home.path(), name).drain().unwrap();
        assert_eq!(hb_drained.len(), 1);
        assert_eq!(hb_drained[0].child_id, "child-2");
    }

    // --- H1: events.jsonl append GATED on fleet membership -------------------

    /// Read every canonical line from `<home>/events.jsonl` (empty when absent).
    fn read_event_lines(home: &std::path::Path) -> Vec<serde_json::Value> {
        let path = home.join("events.jsonl");
        std::fs::read_to_string(&path)
            .ok()
            .map(|s| {
                s.lines()
                    .filter(|l| !l.trim().is_empty())
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn unrelated_session_appends_no_event_line() {
        let home = TempDir::new().unwrap();
        // A session with NO parent, NO inbox, NOT an ATC cwd: a true no-op (H1
        // conservative default — the appender is gated on fleet membership).
        let decision = hook_core(
            home.path(),
            "Stop",
            "random-unrelated-session",
            "/tmp/some/unrelated/repo",
            None,
            None,
            1,
            "",
            None,
        )
        .unwrap();
        assert!(decision.is_none(), "unrelated session must not block");
        assert!(
            !home.path().join("events.jsonl").exists(),
            "unrelated session must append NO event line (H1)"
        );
    }

    #[test]
    fn atc_session_appends_a_well_formed_event_line() {
        let home = TempDir::new().unwrap();
        let cwd = provision_atc(home.path(), "tower");
        // An ATC-managed session IS a fleet member → an event line is appended.
        // Stamp a transcript_path + the PreToolUse matcher into the payload to
        // assert they land in the canonical line.
        let payload = r#"{"transcript_path":"/t/atc.jsonl","tool_name":"AskUserQuestion"}"#;
        hook_core(
            home.path(),
            "PreToolUse",
            "atc-sid",
            cwd.to_str().unwrap(),
            None,
            None,
            7,
            payload,
            Some("AskUserQuestion"),
        )
        .unwrap();
        let lines = read_event_lines(home.path());
        assert_eq!(lines.len(), 1, "ATC session must append one event line");
        let l = &lines[0];
        assert_eq!(l["session_id"], "atc-sid");
        assert_eq!(l["event_type"], "PreToolUse");
        assert_eq!(l["matcher"], "AskUserQuestion");
        assert_eq!(l["transcript_path"], "/t/atc.jsonl");
        assert_eq!(l["agent"], "claude");
        assert_eq!(l["ts"], 7);
    }

    #[test]
    fn stop_event_appends_a_line_and_still_exits_ok_on_append_error() {
        let home = TempDir::new().unwrap();
        let name = "tower";
        let cwd = provision_atc(home.path(), name);
        // Happy path: a Stop on the ATC session appends one Stop line.
        hook_core(
            home.path(),
            "Stop",
            "atc-sid",
            cwd.to_str().unwrap(),
            None,
            Some("done".into()),
            1,
            r#"{"transcript_path":"/t/atc.jsonl"}"#,
            None,
        )
        .unwrap();
        let lines = read_event_lines(home.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["event_type"], "Stop");

        // Forced append error: make events.jsonl a DIRECTORY so the open fails.
        // The append is best-effort (`let _ = …`), so hook_core still returns Ok
        // and the hook exits 0.
        std::fs::remove_file(home.path().join("events.jsonl")).unwrap();
        std::fs::create_dir_all(home.path().join("events.jsonl")).unwrap();
        let res = hook_core(
            home.path(),
            "SessionStart",
            "atc-sid",
            cwd.to_str().unwrap(),
            None,
            None,
            2,
            "{}",
            None,
        );
        assert!(res.is_ok(), "a forced append error must not fail the hook");
    }

    #[test]
    fn child_with_parent_appends_a_line_and_routes_completion() {
        let home = TempDir::new().unwrap();
        // A child with a live env parent IS a fleet member.
        hook_core(
            home.path(),
            "Stop",
            "child-sid",
            "/tmp/child/repo",
            Some("tower"),
            Some("finished".into()),
            1,
            r#"{"transcript_path":"/t/child.jsonl"}"#,
            None,
        )
        .unwrap();
        let lines = read_event_lines(home.path());
        assert_eq!(lines.len(), 1, "a parented child must append an event line");
        assert_eq!(lines[0]["session_id"], "child-sid");
        assert_eq!(lines[0]["parent"], "tower", "parent folded into the line");
        // ...and its completion routes to the parent's inbox.
        assert_eq!(inbox_for(home.path(), "tower").peek().len(), 1);
    }

    // --- PIPE_BUF: the canonical event line stays atomically appendable -------

    /// POSIX guarantees an atomic (no torn lines under concurrent host-wide
    /// appends) `write()` only when it is ≤ PIPE_BUF. Use the portable floor.
    const PIPE_BUF: usize = 4096;

    /// Serialize the canonical line exactly as `append_event_line` does (line +
    /// trailing newline) and return its byte length.
    fn line_bytes(line: &serde_json::Value) -> usize {
        let mut s = serde_json::to_string(line).unwrap();
        s.push('\n');
        s.len()
    }

    #[test]
    fn oversized_payload_is_truncated_and_line_stays_under_pipe_buf() {
        // A pathological transcript-bearing payload far over the cap.
        let big = format!(
            r#"{{"transcript_path":"/t/x.jsonl","blob":"{}"}}"#,
            "x".repeat(64 * 1024)
        );
        assert!(big.len() > MAX_EVENT_PAYLOAD_BYTES);
        let line = build_event_line(
            1,
            "session-with-a-realistically-long-claude-id-0123456789abcdef",
            "/Users/someone/very/deep/nested/work/tree/path/that/is/long",
            "Stop",
            None,
            &big,
            Some("some-parent-instance-name"),
        );
        // Over-cap payloads embed only the truncation marker (not the blob).
        assert_eq!(line["payload"]["_truncated"], true);
        assert_eq!(line["payload"]["_bytes"], big.len() as u64);
        // The WHOLE canonical line + newline must fit in one atomic write.
        assert!(
            line_bytes(&line) <= PIPE_BUF,
            "canonical line must stay ≤ PIPE_BUF for atomic host-wide append (got {})",
            line_bytes(&line)
        );
    }

    #[test]
    fn max_sized_payload_plus_envelope_fits_pipe_buf() {
        // A payload exactly at the cap, with the largest plausible envelope
        // fields, must still leave the whole line ≤ PIPE_BUF.
        let filler = "a".repeat(MAX_EVENT_PAYLOAD_BYTES - 40);
        let payload = format!(r#"{{"transcript_path":"/t","x":"{filler}"}}"#);
        assert!(payload.len() <= MAX_EVENT_PAYLOAD_BYTES);
        let line = build_event_line(
            i64::MAX,
            &"s".repeat(64),
            &"/".repeat(256),
            "PreToolUse",
            Some("AskUserQuestion"),
            &payload,
            Some(&"p".repeat(64)),
        );
        assert!(
            line_bytes(&line) <= PIPE_BUF,
            "max-cap payload + envelope must fit PIPE_BUF (got {})",
            line_bytes(&line)
        );
    }

    #[test]
    fn small_payload_round_trips_matcher_and_transcript() {
        // The parse-once refactor must still surface matcher + transcript_path.
        let payload = r#"{"transcript_path":"/t/atc.jsonl","tool_name":"AskUserQuestion"}"#;
        let line = build_event_line(7, "sid", "/cwd", "PreToolUse", None, payload, None);
        assert_eq!(line["matcher"], "AskUserQuestion");
        assert_eq!(line["transcript_path"], "/t/atc.jsonl");
        assert_eq!(line["payload"]["tool_name"], "AskUserQuestion");
    }

    // --- H-A1: the hook NEVER returns Err (always exit 0) --------------------

    #[tokio::test]
    async fn hook_returns_ok_even_on_unrelated_session() {
        // The public hook() entry point must resolve to Ok(()) → process exit 0,
        // so a global Stop hook can never wedge an unrelated host session. We
        // build the ArgMatches the registry declares for the `hook` verb.
        let m = clap::Command::new("hook")
            .arg(clap::Arg::new("event").long("event"))
            .arg(clap::Arg::new("session-id").long("session-id").default_value(""))
            .arg(clap::Arg::new("cwd").long("cwd").default_value(""))
            .arg(clap::Arg::new("matcher").long("matcher").default_value(""))
            .get_matches_from(vec!["hook", "--event", "Stop", "--session-id", "unrelated"]);
        assert!(
            hook(&m).await.is_ok(),
            "hook must always return Ok → exit 0"
        );
    }

    /// H-A1 (forced error): the drain error path is reachable, and the `hook()`
    /// wrapper swallows it. We force the inbox lock open to fail by making the
    /// `.lock` PATH a directory, so `Inbox::lock` errors inside
    /// `drain_with_budget` and the error propagates out of `hook_core`. The
    /// `hook()` swallow wrapper ([`swallow_hook_result`]) then maps that Err to
    /// `Ok(())` → exit 0, which we assert directly on the real error value.
    #[test]
    fn forced_drain_error_is_swallowed_to_ok() {
        let home = TempDir::new().unwrap();
        let name = "tower";
        let cwd = provision_atc(home.path(), name);
        inbox_for(home.path(), name)
            .commit(&plumbing::InboxRecord::new("c1", name, "x", "Stop", 1))
            .unwrap();

        // Sabotage the lock: make `inbox/<name>.lock` a directory so opening it
        // as a writable file fails inside drain_with_budget.
        let lock_path = plumbing::paths::inbox_dir_in(home.path()).join(format!("{name}.lock"));
        let _ = std::fs::remove_file(&lock_path);
        std::fs::create_dir_all(&lock_path).unwrap();

        // hook_core errors (the drain can't lock)...
        let result = hook_core(
            home.path(),
            "Stop",
            "atc-sid",
            cwd.to_str().unwrap(),
            None,
            None,
            1,
            "{}",
            None,
        );
        assert!(
            result.is_err(),
            "the sabotaged lock should make the drain error"
        );

        // ...and the swallow wrapper the real `hook()` uses maps that Err to a
        // no-op exit-0 outcome with no stdout block.
        let (exit_ok, emitted) = swallow_hook_result(result);
        assert!(exit_ok, "a forced drain error must still yield exit 0");
        assert!(emitted.is_none(), "an error must emit no decision JSON");
    }

    // The broker round-trip (live approve + dead-socket deny fallback) is proven
    // in `ainb-plugin-notifyd::broker::tests`; here we only pin the thin glue that
    // maps a broker `Decision` onto the exact Claude `PermissionRequest` output.
    #[test]
    fn permission_emit_maps_approve_and_deny() {
        use ainb_plugin_notifyd::broker::{Decision, DecisionKind};

        let allow = permission_emit_json(&Decision {
            decision: DecisionKind::Approve,
            reason: "human approved".into(),
        });
        let v: serde_json::Value = serde_json::from_str(&allow).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            "human approved"
        );

        let deny = permission_emit_json(&Decision {
            decision: DecisionKind::Deny,
            reason: "timed out".into(),
        });
        let v: serde_json::Value = serde_json::from_str(&deny).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            "timed out"
        );
    }

    #[test]
    fn extract_permission_context_pulls_tool_input() {
        let payload = r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /tmp/x"}}"#;
        let ctx = extract_permission_context(payload);
        assert!(ctx.contains("rm -rf /tmp/x"), "context was: {ctx}");
        assert!(extract_permission_context("not json").is_empty());
        assert!(extract_permission_context("{}").is_empty());
    }
}
