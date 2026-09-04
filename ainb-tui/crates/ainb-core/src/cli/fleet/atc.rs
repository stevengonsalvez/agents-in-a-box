// ABOUTME: `ainb fleet atc` — provision / manage the Air Traffic Control brain.
//
// Verbs:
//   setup <name>     provision ~/.agents-in-a-box/atc/<name>/ (CLAUDE.md + meta +
//                    seeded state/task-log), install the heartbeat timer, and
//                    spawn the ATC session via `ainb run`. Idempotent.
//   teardown <name>  remove the timer + instance dir. Safe when absent.
//   status <name>    report one instance (meta + timer + session liveness).
//   list             list all provisioned instances.
//   mode <name>      report the supervisor mode (lite | full), or switch it with
//                    --set. Exactly one controller owns the fleet at a time.
//   supervise <name> (internal) run the LITE controller: the LLM-free scan loop
//                    that owns the fleet while mode is `lite`.
//   repair <name>    re-assert an existing instance's heartbeat scheduler from
//                    its meta.json, rebuilt against the CURRENT $PATH/$AINB_BIN.
//                    Reads meta, never writes it, so a stale timer costs one
//                    file instead of a full re-provision.
//   heartbeat <name> (internal, called by the OS timer) build the [HEARTBEAT]
//                    nudge from `fleet needs --format json` and tmux-send it
//                    into the ATC session.

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::cli::OutputFormat;
use crate::fleet::atc::supervisor::{
    Controller, SupervisorMode, may_act, mode_help, resolve_full_provider, stand_down_reason,
};
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
        Some(("repair", sub)) => repair(sub, format).await,
        Some(("mode", sub)) => super::atc_supervisor::mode(sub, format).await,
        Some(("supervise", sub)) => super::atc_supervisor::supervise(sub, format).await,
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

    let paths = AtcPaths::resolve(&name)?;

    // Carry the supervisor mode + provider of an EXISTING instance forward.
    // `setup` is idempotent and is what `daemon atc start` re-runs to respawn a
    // dead session; rebuilding meta from `AtcMeta::new` would silently flip a
    // deliberately-lite fleet back to a token-spending full brain.
    let existing = std::fs::read_to_string(&paths.meta)
        .ok()
        .and_then(|s| AtcMeta::from_json(&s).ok());

    let mut meta = AtcMeta::new(&name);
    if let Some(prior) = &existing {
        meta.mode = prior.mode;
        meta.provider.clone_from(&prior.provider);
    }
    if let Some(raw) = matches.get_one::<String>("mode") {
        meta.mode = SupervisorMode::from_id(raw)
            .with_context(|| format!("unknown mode '{raw}' — expected `lite` or `full`"))?;
    }
    if let Some(p) = matches.get_one::<String>("provider") {
        meta.provider.clone_from(p);
    }
    // Refuse BEFORE writing anything: a full-mode instance on a provider ainb
    // cannot nudge provisions cleanly and is then never woken, which looks
    // healthy on every surface. Lite mode runs no brain, so it is unconstrained.
    if meta.mode == SupervisorMode::Full {
        resolve_full_provider(&meta.provider)?;
    }

    if let Some(i) = interval {
        meta.heartbeat_interval_min = i;
    }
    if let Some(p) = idle_pause {
        meta.idle_pause_min = p;
    }
    meta.heartbeat_enabled = !no_heartbeat;

    std::fs::create_dir_all(&paths.dir)
        .with_context(|| format!("creating ATC dir {}", paths.dir.display()))?;

    // Render policy + write meta. Always overwrite CLAUDE.md/meta so setup is
    // idempotent and picks up policy/template changes on re-run. All four durable
    // artefacts are written through `write_atomic` (M-A3): a crash mid-write
    // would otherwise leave a torn file that status/list/heartbeat then error on.
    let policy = render_claude_md(&meta);
    plumbing::atomic::write_atomic(&paths.claude_md, policy.as_bytes())
        .context("writing CLAUDE.md")?;
    // A provider that reads a DIFFERENT file (Codex reads AGENTS.md) gets the
    // same policy under the name it actually opens. Writing only CLAUDE.md
    // would give a brain that boots and then ignores its playbook. CLAUDE.md is
    // still written unconditionally so switching providers never leaves the
    // instance without the file the previous one read.
    let policy_file = provider_policy_file(&meta);
    if policy_file != "CLAUDE.md" {
        plumbing::atomic::write_atomic(&paths.dir.join(policy_file), policy.as_bytes())
            .with_context(|| format!("writing {policy_file}"))?;
    }
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

    // D12: provision the heartbeat as a DAEMON cron by registering the instance
    // in the daemon store — the daemon-native replacement for the launchd/systemd
    // timer. We register FIRST so the local timer can be a true FALLBACK, never a
    // concurrent second scheduler: with the daemon up (the P9 world) two active
    // schedulers would fire two nudges per interval into the same ATC session and
    // keep two split-brain retry-cap ledgers (the CLI heartbeat's JSON cap vs the
    // daemon cron's `atc_retry` cap) — the exact one-send-path / daemon-owns-state
    // violation. Best-effort: a down daemon warns and returns false (external-dep
    // rule — never hard-fail on a missing daemon). The UX is unchanged.
    // A LITE fleet has no LLM heartbeat to schedule, and registering one would
    // put a second controller on the fleet the moment the daemon came up.
    let schedules_heartbeat = meta.heartbeat_enabled && meta.mode == SupervisorMode::Full;
    let daemon_registered = if schedules_heartbeat {
        register_heartbeat_with_daemon(&meta, &paths).await
    } else {
        false
    };

    // Install the launchd/systemd timer ONLY as the fallback the daemon cron
    // supersedes: when the daemon owns the heartbeat we skip the local timer (and
    // tear down any timer a prior daemon-down `setup` left behind) so exactly one
    // mechanism fires. When the daemon is unreachable the local timer keeps the
    // heartbeat alive. `timer::install` is idempotent.
    // A LITE instance must not leave a full-mode scheduler behind. Skipping the
    // INSTALL is not enough when the instance was previously full: both
    // schedulers stay registered and keep firing `atc heartbeat` every interval
    // forever. Each beat now stands down, so nothing is sent — but it also
    // reports `ledger_owner: "none"`, which trips the daemon's handoff gate and
    // logs a warning on every beat, indefinitely. `mode --set lite` tears both
    // down; this path has to agree with it.
    if meta.mode == SupervisorMode::Lite {
        if let Err(e) = timer::teardown(&meta.name) {
            eprintln!("warning: lite mode but the local heartbeat timer was not removed: {e}");
        }
        if !unregister_heartbeat_with_daemon(&meta.name).await {
            tracing::debug!(
                "lite mode: no daemon heartbeat cron to unregister (or the daemon is down)"
            );
        }
    }

    let mut timer_paths = Vec::new();
    if schedules_heartbeat {
        if daemon_registered {
            // A prior daemon-down run may have installed a local timer; remove it
            // now that the daemon cron is the single active scheduler.
            //
            // Reported, not swallowed: the presence of this unit is what the
            // ledger-handover predicate reads as "the local timer was the
            // scheduler". A teardown that failed silently leaves it looking
            // installed while the daemon owns the schedule, and a later switch
            // to lite then skips the handover it needed.
            if let Err(e) = timer::teardown(&meta.name) {
                eprintln!(
                    "warning: the daemon owns the heartbeat but the local timer unit could not \
be removed ({e}); `ainb fleet atc repair {}` re-asserts a single scheduler",
                    meta.name
                );
            }
        } else {
            timer_paths = timer::install(&meta).context("installing heartbeat timer")?;
            // Pinning `current_exe()` used to guarantee the unit pointed at a
            // real binary. Resolving at firing time does not, so say so now
            // rather than reporting a successful setup that can never fire.
            if let Some(warning) = timer::install_would_be_unrunnable(&meta) {
                eprintln!("warning: heartbeat timer installed but {warning}");
            }
        }
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
            let _ = ainb_plugin_notifyd::install::extract_hook_bin(&paths_nd);
            match plumbing::settings::install_claude_hooks(&home, &script) {
                Ok(_) => hooks_installed = true,
                Err(e) => tracing::warn!("failed to install lifecycle hooks: {e}"),
            }
        }
    }

    // Start the controller the mode names — never both. In full mode that is the
    // brain session; in lite mode it is the LLM-free scanner, and no brain is
    // spawned at all (spawning one that is never nudged would burn a session
    // slot and mislead every liveness surface).
    let mut spawned = false;
    let mut lite_started = false;
    if !no_spawn {
        match meta.mode {
            SupervisorMode::Full => spawned = spawn_session(&meta, &paths).await?,
            SupervisorMode::Lite => {
                lite_started = super::atc_supervisor::ensure_lite_running(&meta.name).is_ok();
            }
        }
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
        "daemon_registered": daemon_registered,
        "mode": meta.mode.id(),
        "provider": meta.provider,
        "owner": meta.mode.owner().label(),
        "lite_supervisor_started": lite_started,
    });

    if matches!(format, OutputFormat::Text) {
        println!("ATC '{name}' provisioned.");
        println!("  dir:       {}", paths.dir.display());
        println!("  policy:    {}", paths.dir.join(policy_file).display());
        println!(
            "  mode:      {} — owned by the {}",
            meta.mode.id(),
            meta.mode.owner().label()
        );
        match meta.mode {
            SupervisorMode::Full => {
                println!("  session:   {} (spawned: {spawned})", meta.tmux_session());
                println!("  brain:     {}", meta.provider);
            }
            SupervisorMode::Lite => {
                println!("  scanner:   lite (started: {lite_started}) — no LLM, no token spend");
            }
        }
        println!("  hooks:     lifecycle hooks installed: {hooks_installed}");
        if schedules_heartbeat {
            let via = if daemon_registered {
                "daemon cron".to_string()
            } else {
                format!("{} local timer unit(s)", timer_paths.len())
            };
            println!(
                "  heartbeat: every {}m via {via}",
                meta.heartbeat_interval_min
            );
        } else if meta.mode == SupervisorMode::Lite {
            println!("  heartbeat: n/a in lite mode (the scanner is the controller)");
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
    // Refuse rather than spawn a brain we cannot wake. Reached only in full
    // mode; `setup` and `mode --set` both validate earlier, so this is the last
    // guard for a hand-edited meta.json.
    resolve_full_provider(&meta.provider)?;
    let bin = atc_bin();
    let bootstrap = format!(
        "You are ATC. Read {} as your operating policy, then read state.json and \
task-log.md. Stand by for [HEARTBEAT] messages and act per the policy.",
        paths.dir.join(provider_policy_file(meta)).display()
    );
    let status = tokio::process::Command::new(&bin)
        .args([
            "run",
            "--tool",
            &meta.provider,
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

/// The policy filename the instance's provider actually reads, falling back to
/// `CLAUDE.md` for a provider that declares no ATC control (a lite instance can
/// legitimately name any provider, since it runs no brain).
fn provider_policy_file(meta: &AtcMeta) -> &'static str {
    crate::fleet::atc::supervisor::provider_control(&meta.provider)
        .filter(|c| c.is_supported())
        .map_or("CLAUDE.md", |c| c.policy_file)
}

/// Register the ATC instance's heartbeat as a daemon cron (D12), returning
/// whether the daemon accepted the registration.
///
/// Best-effort by design: a missing / unreachable daemon (no token file, dial
/// refused, timeout) is warned and returns `false`, and the caller keeps the
/// local launchd/systemd timer as the fallback. This is the external-dep rule —
/// `atc setup` must never hard-fail just because the daemon is not up.
async fn register_heartbeat_with_daemon(meta: &AtcMeta, paths: &AtcPaths) -> bool {
    use crate::fleet::bridge::daemon::DaemonClient;
    use ainb_hangar_proto::snapshots::AtcRegisterParams;

    let client = match DaemonClient::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "ATC daemon registration skipped (daemon not reachable: {e}); \
heartbeat falls back to the local timer"
            );
            return false;
        }
    };
    let params = AtcRegisterParams {
        name: meta.name.clone(),
        cwd: paths.dir.display().to_string(),
        tmux_session: Some(meta.tmux_session()),
        heartbeat_cron: Some(heartbeat_cron_for_interval(meta.heartbeat_interval_min)),
        err_retry_cap: None,
        idle_pause_min: Some(i64::from(meta.idle_pause_min)),
        expected_generation: None,
    };
    match client.atc_register(params).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(
                "ATC daemon registration failed ({e}); heartbeat falls back to the local timer"
            );
            false
        }
    }
}

/// Unregister the ATC instance's heartbeat from the daemon (D12), returning
/// whether the daemon reported it disabled a registered instance.
///
/// Best-effort by design (mirrors [`register_heartbeat_with_daemon`]): a missing
/// / unreachable daemon is warned and returns `false`. This clears `enabled` +
/// `next_tick_at` in daemon-owned state so a torn-down instance's `atc_instance`
/// row is no longer schedulable — the daemon-native counterpart to removing the
/// local launchd/systemd timer.
async fn unregister_heartbeat_with_daemon(name: &str) -> bool {
    use crate::fleet::bridge::daemon::DaemonClient;
    use ainb_hangar_proto::snapshots::AtcUnregisterParams;

    let client = match DaemonClient::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "ATC daemon unregister skipped (daemon not reachable: {e}); \
the instance row is left as-is"
            );
            return false;
        }
    };
    match client
        .atc_unregister(AtcUnregisterParams {
            name: name.to_string(),
            expected_generation: None,
        })
        .await
    {
        Ok(res) => res.disabled,
        Err(e) => {
            tracing::warn!("ATC daemon unregister failed ({e}); the instance row is left as-is");
            false
        }
    }
}

/// Convert a heartbeat interval in minutes to a UTC 5-field cron expression the
/// daemon heartbeat scheduler fires on.
///
/// An interval under an hour maps to `*/N * * * *` (every N minutes); an interval
/// of 60+ minutes maps to hourly `0 * * * *` (a `*/N` minute field only spans
/// 0–59). `0` is clamped to 1 so a degenerate config still yields a valid cron.
#[must_use]
fn heartbeat_cron_for_interval(interval_min: u32) -> String {
    let n = interval_min.max(1);
    if n >= 60 {
        "0 * * * *".to_string()
    } else {
        format!("*/{n} * * * *")
    }
}

// --- teardown ---------------------------------------------------------------

async fn teardown(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let name = require_name(matches)?;
    let purge = matches.get_flag("purge");

    // Remove the timer first (idempotent, safe when absent).
    let removed = timer::teardown(&name).context("removing heartbeat timer")?;

    // Unregister from the daemon so its heartbeat cron stops scheduling this
    // instance (clears enabled + next_tick_at in daemon-owned state). Best-effort:
    // a down daemon is warned, not fatal. Mirrors setup's register call — teardown
    // must tear down BOTH schedulers, not just the local timer.
    let daemon_unregistered = unregister_heartbeat_with_daemon(&name).await;

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
        "daemon_unregistered": daemon_unregistered,
        "session_killed": killed,
        "dir_purged": purged,
        "lifecycle_hooks_uninstalled": hooks_uninstalled,
    });

    if matches!(format, OutputFormat::Text) {
        println!("ATC '{name}' torn down.");
        println!("  timer units removed: {}", removed.len());
        println!("  daemon unregistered: {daemon_unregistered}");
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
    // A unit file existing is NOT the same as a working timer: if the binary it
    // names cannot be found, launchd exits 78 EX_CONFIG and parks the job
    // forever while `installed: true` keeps claiming health.
    let timer_health = timer::installed_program_health(&name);
    let timer_program = timer_health.program().map(str::to_string);
    let timer_problem = timer_health.problem();
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
        "timer_program": timer_program,
        "timer_program_problem": timer_problem,
        "session_running": session_running,
        "tmux_session": meta.tmux_session(),
        "last_heartbeat_ms": last_heartbeat_ms,
        "heartbeat_age_ms": heartbeat_age_ms,
        "heartbeat_stale": heartbeat_stale,
        "mode": meta.mode.id(),
        "provider": meta.provider,
        "owner": meta.mode.owner().label(),
    });

    if matches!(format, OutputFormat::Text) {
        println!("ATC '{}' status", meta.name);
        println!("  dir:       {}", paths.dir.display());
        for line in mode_help(meta.mode, &meta.provider) {
            println!("  {line}");
        }
        println!(
            "  session:   {} (running: {session_running})",
            meta.tmux_session()
        );
        let program_note = timer_problem
            .as_ref()
            .map_or_else(String::new, |problem| format!(", {problem}"));
        println!(
            "  heartbeat: {} (timer installed: {timer_installed}{program_note}, every {}m)",
            if meta.heartbeat_enabled {
                "enabled"
            } else {
                "disabled"
            },
            meta.heartbeat_interval_min
        );
        if timer_problem.is_some() {
            // Deliberately NOT `setup`: setup rebuilds meta.json from
            // `AtcMeta::new` defaults, re-enables a disabled heartbeat,
            // rewrites CLAUDE.md + settings.json hooks and spawns a session.
            // The blast radius of a stale PATH should be one file.
            println!(
                "             the timer cannot fire. Run `ainb fleet atc repair {}` to rebuild it against the current PATH.",
                meta.name
            );
        }
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

// --- repair -----------------------------------------------------------------

/// Which scheduler owns an instance's heartbeat after a repair. Exactly one, by
/// construction: two live schedulers firing into one ATC session is the D12
/// violation `setup` exists to prevent, and `repair` must not reintroduce it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scheduler {
    /// The daemon cron owns it; the local timer is torn down.
    Daemon,
    /// No daemon, so the launchd/systemd unit is the scheduler.
    LocalTimer,
    /// The heartbeat is disabled in meta.json; nothing is scheduled.
    None,
}

impl Scheduler {
    fn as_str(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::LocalTimer => "local_timer",
            Self::None => "none",
        }
    }
}

/// Whether the daemon actually answers, used by `repair --dry-run` where
/// registering would mutate daemon state.
///
/// Deliberately a real RPC: `DaemonClient::from_env()` succeeds whenever the
/// token file exists, and a stopped daemon leaves that file behind, so it
/// reports "up" for a daemon that is scheduling nothing.
async fn daemon_is_reachable() -> bool {
    use crate::fleet::bridge::daemon::DaemonClient;

    match DaemonClient::from_env() {
        Ok(client) => client.attention_list_fleet().await.is_ok(),
        Err(_) => false,
    }
}

/// The mutual-exclusion rule, extracted so it can be pinned by a unit test
/// (the daemon branch is not reachable without a live daemon).
fn repair_scheduler(heartbeat_enabled: bool, daemon_registered: bool) -> Scheduler {
    if !heartbeat_enabled {
        Scheduler::None
    } else if daemon_registered {
        Scheduler::Daemon
    } else {
        Scheduler::LocalTimer
    }
}

/// Re-assert an existing instance's heartbeat scheduler.
///
/// INVARIANT: repair READS meta.json and NEVER writes it. That is the entire
/// reason this verb exists instead of "re-run setup": setup rebuilds meta from
/// `AtcMeta::new` defaults and would silently reset a customised interval /
/// idle-pause, re-enable a disabled heartbeat, rewrite CLAUDE.md and the
/// settings.json hooks, and spawn a Claude session.
///
/// The single repair primitive is `timer::install`, which rebuilds the unit from
/// the CURRENT process env (`$AINB_BIN`, `$PATH`). So repair is literally
/// "rewrite the unit against this shell's PATH", which is exactly the failure
/// the heartbeat cannot recover from on its own.
async fn repair(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let name = require_name(matches)?;
    let dry_run = matches.get_flag("dry-run");
    let paths = AtcPaths::resolve(&name)?;
    if !paths.meta.exists() {
        bail!(
            "no ATC instance named '{name}' (expected {})",
            paths.meta.display()
        );
    }
    let meta = AtcMeta::from_json(&std::fs::read_to_string(&paths.meta)?)?;

    let before = timer::installed_program_health(&name);
    let program_before = before.program().map(str::to_string);
    let problem_before = before.problem();

    // Daemon arbitration, byte-for-byte the same call `setup` makes. In dry-run
    // we must NOT register (that mutates daemon state), so we probe instead:
    // `daemon_registered` then means "the daemon answered and would take this
    // heartbeat", not "registration happened".
    //
    // The probe is a real read-only RPC, NOT `DaemonClient::from_env()`.
    // `from_env` only resolves the socket path and reads the token file, and
    // nothing deletes that token when the daemon stops, so a stale token would
    // make dry-run claim the daemon owns a heartbeat it is not scheduling and
    // invert the preview against what a real run does.
    // In dry-run the daemon question is UNANSWERABLE, not merely unprobed.
    // Whether a real run takes the daemon branch depends on `atc_register`
    // SUCCEEDING, and registration fails for reasons a read-only probe cannot
    // see (generation conflict, unknown instance, proto skew, store error);
    // `register_heartbeat_with_daemon` returns false for every one of them. So
    // dry-run does not guess: it reports `daemon_registered: null` and previews
    // the LOCAL TIMER path, which is the conservative branch.
    //
    // The property that matters for a health gate is one-directional: dry-run
    // must never be GREENER than a real run. Previewing the local path (and
    // running its refusal gate) guarantees that. The reverse, dry-run failing
    // where a real run would have handed the heartbeat to the daemon, is the
    // safe direction to be wrong in, and the output says so explicitly.
    let daemon_registered = if !meta.heartbeat_enabled || dry_run {
        false
    } else {
        register_heartbeat_with_daemon(&meta, &paths).await
    };

    let scheduler = repair_scheduler(meta.heartbeat_enabled, daemon_registered);

    let mut timer_units: Vec<std::path::PathBuf> = Vec::new();
    let mut removed: Vec<std::path::PathBuf> = Vec::new();
    let mut daemon_unregistered = false;
    let changed;
    let mut program_after: Option<String> = None;
    let result;

    match scheduler {
        // The heartbeat was turned off by the operator. Removing a unit that
        // survived (`setup --no-heartbeat` on a provisioned instance leaves the
        // old unit installed and firing) is repair. Installing one here would
        // resurrect a scheduler someone deliberately disabled, which is a worse
        // bug than the one this verb fixes.
        Scheduler::None => {
            removed = remove_or_preview_units(&name, dry_run)?;
            // The daemon cron must go too, not just the local unit. Nothing in
            // the daemon reads `meta.heartbeat_enabled`: its scheduler selects
            // on its own `enabled` row, and `setup --no-heartbeat` flips the
            // meta WITHOUT unregistering. Removing only the local unit here
            // would print "nothing to schedule" while the daemon kept sending
            // [HEARTBEAT] every interval, which is the same confident-and-wrong
            // claim this whole line of work exists to remove.
            //
            // A failed unregister is NOT reportable as "nothing to schedule":
            // if the daemon still holds the registration it keeps firing, and
            // exiting 0 there is precisely the confident-and-wrong claim this
            // verb exists to remove. In dry-run nothing is attempted, so the
            // field stays null rather than previewing an unregister that may
            // not even be needed.
            if !dry_run {
                let unregistered = unregister_heartbeat_with_daemon(&meta.name).await;
                if !unregistered && daemon_is_reachable().await {
                    bail!(
                        "ATC '{}' has its heartbeat disabled and the local timer was removed, but \
the daemon is reachable and did NOT accept the unregister, so its cron may still be firing. \
Re-run once the daemon is healthy, or check `ainb fleet atc list`.",
                        meta.name
                    );
                }
                daemon_unregistered = unregistered;
            }
            changed = !removed.is_empty() || daemon_unregistered;
            result = "disabled";
        }
        // Exactly one scheduler: the daemon cron owns it, so the local timer goes.
        Scheduler::Daemon => {
            removed = remove_or_preview_units(&name, dry_run)?;
            changed = !removed.is_empty();
            result = "daemon";
        }
        Scheduler::LocalTimer => {
            // Refuse BEFORE writing anything. A repair that overwrites a unit
            // with an equally dead one has degraded the instance while claiming
            // success, and `setup`'s install-then-warn behaviour is precisely
            // what makes the current advice useless here.
            if let timer::ProgramHealth::Missing(program) = timer::fresh_program_health(&meta) {
                let pinned = std::env::current_exe()
                    .ok()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "/full/path/to/ainb".to_string());
                bail!(
                    "cannot repair ATC '{name}': the rebuilt timer would name '{program}', which \
does not resolve on the PATH the unit will carry, so it still could not fire. Nothing was written \
(the existing unit is unchanged). Install ainb somewhere on PATH, or re-run pinned to a full path:\
\n  AINB_BIN={pinned} ainb fleet atc repair {name}"
                );
            }
            changed = timer::install_would_change(&meta);
            if dry_run {
                timer_units = timer::unit_paths(&meta.name)?;
            } else {
                // Exactly one scheduler, enforced on THIS arm too. Registration
                // returning false does not mean the daemon holds nothing: it
                // also covers a reachable daemon that declined, and an instance
                // registered by an earlier `setup` keeps its enabled row. Left
                // alone, the daemon cron and this local unit would both fire
                // into one session.
                //
                // A daemon that is DOWN cannot be told anything and is not
                // scheduling either, so a failed unregister there is harmless.
                // A daemon that is UP and refuses is the dangerous case: it may
                // still hold the cron, and installing the local unit on top
                // would produce exactly the double-scheduling this verb claims
                // to prevent. Refuse BEFORE writing, so the instance is left
                // exactly as it was, matching the `Scheduler::None` arm which
                // already treats this state as fatal.
                daemon_unregistered = unregister_heartbeat_with_daemon(&meta.name).await;
                if !daemon_unregistered && daemon_is_reachable().await {
                    bail!(
                        "cannot repair ATC '{}': the daemon is reachable and did NOT accept the \
unregister, so it may still hold the heartbeat cron. Installing a local timer on top would leave \
two schedulers firing into one session. Nothing was written. Re-run once the daemon is healthy.",
                        meta.name
                    );
                }
                // Always install, even when `changed` is false: install also
                // re-loads the job with launchctl/systemctl, and a byte-perfect
                // unit whose job was unloaded (OS upgrade, manual unload) is
                // still a dead heartbeat that a byte-compare no-op would leave
                // dead.
                timer_units = timer::install(&meta).context("installing heartbeat timer")?;
                // Verify the unit that was WRITTEN, which install addressed by
                // `meta.name`. Reading back by the raw CLI arg would inspect a
                // different file whenever the two differ.
                let after = timer::installed_program_health(&meta.name);
                match &after {
                    timer::ProgramHealth::Resolves(p) => program_after = Some(p.clone()),
                    other => bail!(
                        "repaired ATC '{}' but the installed timer still cannot fire: {}",
                        meta.name,
                        other
                            .problem()
                            .unwrap_or_else(|| "no unit on disk after install".to_string())
                    ),
                }
            }
            result = "repaired";
        }
    }

    let summary = json!({
        "action": "repair",
        "name": meta.name,
        "dry_run": dry_run,
        "result": result,
        "scheduler": scheduler.as_str(),
        // Null in dry-run: nothing was attempted, so neither is KNOWN. A bool
        // here would be a guess presented as a fact.
        "daemon_registered": if dry_run { serde_json::Value::Null } else { json!(daemon_registered) },
        "daemon_unregistered": if dry_run { serde_json::Value::Null } else { json!(daemon_unregistered) },
        // The JSON surface is what agents and CI consume, so it must not hide
        // that the unit was written and never handed to launchd/systemd.
        "activation_skipped": timer::activation_is_skipped(),
        "changed": changed,
        "timer_units": timer_units.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "timer_units_removed": removed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "program_before": program_before,
        "problem_before": problem_before,
        "program_after": program_after,
    });

    if matches!(format, OutputFormat::Text) {
        print_repair_text(
            &meta.name,
            result,
            dry_run,
            changed,
            &timer_units,
            &removed,
            daemon_unregistered,
            program_before.as_deref(),
            problem_before.as_deref(),
            program_after.as_deref(),
        );
        if dry_run {
            println!("Re-run without --dry-run to apply.");
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    }
    Ok(())
}

/// Tear the local timer down, or (dry-run) report which units a teardown would
/// remove without touching them.
fn remove_or_preview_units(name: &str, dry_run: bool) -> Result<Vec<std::path::PathBuf>> {
    if dry_run {
        Ok(timer::unit_paths(name)?.into_iter().filter(|p| p.exists()).collect())
    } else {
        timer::teardown(name).context("removing heartbeat timer")
    }
}

#[allow(clippy::too_many_arguments)]
fn print_repair_text(
    name: &str,
    result: &str,
    dry_run: bool,
    changed: bool,
    timer_units: &[std::path::PathBuf],
    removed: &[std::path::PathBuf],
    daemon_unregistered: bool,
    program_before: Option<&str>,
    problem_before: Option<&str>,
    program_after: Option<&str>,
) {
    match result {
        "disabled" => {
            println!("ATC '{name}' heartbeat is disabled in meta.json, nothing to schedule.");
            println!(
                "  local timer units {}: {}",
                if dry_run {
                    "that would be removed"
                } else {
                    "removed"
                },
                removed.len()
            );
            // Dry-run attempts no daemon call, so "no" would state as fact the
            // very guess the JSON surface refuses to make (it emits null here).
            // Text is the DEFAULT format, so it is the one most operators read;
            // it must not be more confident than the machine-readable output.
            println!(
                "  daemon cron unregistered: {}",
                if dry_run {
                    "unknown (dry-run attempts no daemon call)"
                } else if daemon_unregistered {
                    "yes"
                } else {
                    "no"
                }
            );
            println!(
                "  (enable it with `ainb fleet atc setup {name} --interval <n>`; note setup rewrites meta.json)"
            );
        }
        "daemon" => {
            println!(
                "ATC '{name}' heartbeat {}.",
                if dry_run {
                    "would be repaired"
                } else {
                    "repaired"
                }
            );
            println!("  scheduler: daemon cron (the daemon owns this heartbeat)");
            println!(
                "  local timer units {}: {}",
                if dry_run {
                    "that would be removed"
                } else {
                    "removed"
                },
                removed.len()
            );
        }
        _ => {
            println!(
                "ATC '{name}' heartbeat {}.",
                if dry_run {
                    "would be repaired"
                } else {
                    "repaired"
                }
            );
            // Not "daemon not reachable": this branch is taken whenever
            // registration returned false, which also covers a daemon that
            // answered and declined. Naming a cause we did not establish sends
            // an operator to debug the wrong thing.
            println!("  scheduler: local timer (the daemon did not take this heartbeat)");
            let units = timer_units
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            println!("  units:     {units}");
            let before_note = problem_before.map_or_else(
                || {
                    program_before.map_or_else(
                        || "no unit installed".to_string(),
                        |p| format!("ok ({p} resolved)"),
                    )
                },
                str::to_string,
            );
            println!("  before:    {before_note}");
            match program_after {
                Some(p) => {
                    println!("  after:     ok ({p} resolves on the unit PATH)");
                    // Do not let "repaired" imply a loaded job when the load
                    // step was deliberately skipped: the unit is on disk but
                    // nothing asked launchd/systemd to pick it up.
                    if timer::activation_is_skipped() {
                        println!(
                            "  note:      unit written but NOT loaded (AINB_TIMER_SKIP_ACTIVATION=1)"
                        );
                    }
                }
                None => println!("  after:     not verified (nothing was written)"),
            }
            let changed_note = match (changed, dry_run) {
                (true, true) => "yes, the unit would be rewritten against the current PATH",
                (true, false) => "yes, unit rewritten against the current PATH",
                (false, true) => "no, the unit already matches the current PATH",
                (false, false) => "no, the unit already matched the current PATH (job reloaded)",
            };
            println!("  changed:   {changed_note}");
        }
    }
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
        // Same program check as `status`, so the fleet-wide view cannot claim a
        // timer is fine while the per-instance view calls it dead.
        let timer_problem = timer::installed_program_health(name).problem();
        rows.push(json!({
            "name": meta.name,
            "heartbeat_enabled": meta.heartbeat_enabled,
            "heartbeat_interval_min": meta.heartbeat_interval_min,
            "timer_installed": timer::is_installed(name),
            "timer_program_problem": timer_problem,
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
                if r["timer_program_problem"].is_string() {
                    "BROKEN"
                } else if r["timer_installed"].as_bool().unwrap_or(false) {
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

    // THE EXCLUSIVITY GATE. Both schedulers — the local launchd/systemd timer
    // and the daemon cron — reach the full controller through this one verb, so
    // gating here gates every full-mode action path. A timer left behind by a
    // switch to lite therefore fires into a refusal instead of sending a second
    // stream of nudges alongside the lite scanner.
    //
    // Reports `roster_valid: false` and `delivered: false`, which are the
    // daemon's own fail-closed gates: it leaves the retry ledger untouched
    // rather than reading a stood-down beat as "the whole fleet recovered".
    if !may_act(meta.mode, Controller::FullHeartbeat) {
        let reason = stand_down_reason(meta.mode, Controller::FullHeartbeat);
        if matches!(format, OutputFormat::Text) {
            println!("[atc/{name}] {reason}");
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "action": "heartbeat",
                    "name": name,
                    "stood_down": true,
                    "mode": meta.mode.id(),
                    "owner": meta.mode.owner().label(),
                    "reason": reason,
                    "needs_count": 0,
                    "err_sessions": [],
                    "roster_valid": false,
                    "ledger_owner": "none",
                    "completions": 0,
                    "session_live": false,
                    "idle_paused": false,
                    "delivered": false,
                    "skipped_pending": false,
                }))?
            );
        }
        return Ok(());
    }

    let tmux = meta.tmux_session();

    // Pull the LLM-free needs read. Shelling the verb keeps ATC poll-mode and
    // reuses the exact same reader the operator sees. As of Wave 4 that reader
    // is event-sourced: `fleet needs` reads the materialized `current_state`
    // table (hook-backed sessions) and falls back to a live tmux/transcript
    // `classify()` only for sessions absent from / tmux-sourced in
    // `current_state` (non-Claude agents, transient errors). So the heartbeat's
    // coarse session state is now `current_state`-backed without any direct
    // SQLite access here — and the exactly-once inbox drain below is UNCHANGED.
    // Whether the scan actually WORKED, kept separate from its result. A failed
    // `fleet needs` degrades to an empty roster here, which is byte-identical to a
    // healthy quiet fleet — and whoever owns the retry ledger would read that as
    // "every session recovered" and hand each one a fresh budget. The summary
    // carries this so the failure is visible across the process boundary instead
    // of being laundered into a cheerful empty list.
    let scan = fetch_needs().await;
    let roster_valid = scan.is_ok();
    if let Err(e) = &scan {
        tracing::warn!(error = %e, "atc heartbeat: fleet scan failed; roster reported as unusable");
    }
    let rows = scan.unwrap_or_default();

    // The ERR roster is taken BEFORE the ATC-channel filter below. Channel rules
    // decide who gets NUDGED; they must not decide who gets ESCALATED. A rule that
    // routes a session's cards away from ATC would otherwise also silence its
    // phone push, which is the opposite of what suppressing a nudge means.
    let err_sessions: Vec<serde_json::Value> = rows
        .iter()
        .filter_map(|r| match &r.context {
            crate::fleet::read::NeedsContext::Err(e) => Some(json!({
                "session_id": r.session.id,
                "cwd": r.session.cwd,
                "pattern": e.pattern,
            })),
            _ => None,
        })
        .collect();
    // ATC channel gate (agents-in-a-box-cdd): drop needs rows the notify rules kept
    // off the Atc channel, so a board-only `waiting` (or a kind routed away from
    // ATC) stops nudging the ATC brain. Fail-open when the daemon inbox is
    // unreachable — a transient socket fault must never silence ATC.
    let rows = match fetch_fleet_attention().await {
        Ok(attention) => filter_atc_channel(rows, &attention),
        Err(e) => {
            tracing::debug!(
                error = %e,
                "atc heartbeat: attention inbox unavailable; nudging the full needs snapshot"
            );
            rows
        }
    };
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Load the heartbeat process's OWN bookkeeping file. This is a single-writer
    // file (only the heartbeat writes it), separate from state.json which the
    // ATC Claude session owns — so the two writers never clobber each other.
    let mut hb_state = read_heartbeat_state(&paths);

    // WHO OWNS THE ERR RETRY LEDGER, decided by the PRESENCE of `--exhausted`
    // (an empty value still counts as present — "the daemon is up and nothing is
    // spent" is a real answer, distinct from "no daemon").
    //
    // Daemon up: it reads the durable `atc_retry` ledger before this beat and
    // hands us the spent sessions. We render the cap from that set and count
    // NOTHING locally, so the two ledgers are never both live in one beat.
    // Daemon down: nobody else is counting, so we fall back to our own
    // `heartbeat-state.json` tally — an unattended ATC still stops auto-continuing
    // a permanently-broken session.
    let daemon_exhausted: Option<std::collections::HashSet<String>> =
        matches.get_one::<String>("exhausted").map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        });

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
            "[HEARTBEAT {}] fleet idle-paused (quiet ≥ {}m) — standing by, no token spend.",
            crate::fleet::atc::heartbeat::stamp(now_ms),
            meta.idle_pause_min
        )
    } else {
        let mut b = match &daemon_exhausted {
            // Daemon-owned: seed a SCRATCH tally at the cap for the sessions it
            // reported spent, render from that, and throw it away. The durable
            // count stays in `atc_retry` where the daemon can escalate off it.
            // Cost of the scratch: a session mid-budget renders plain rather than
            // carrying the "final auto-continue" hint, because we deliberately do
            // not ship the exact counts over the CLI boundary. The hard stop is
            // unaffected — it comes from the set.
            Some(ids) => {
                let mut scratch = HeartbeatState::default();
                for id in ids {
                    scratch.continue_counts.insert(id.clone(), DEFAULT_ERR_RETRY_CAP);
                }
                build_heartbeat_enforcing_cap(&rows, now_ms, DEFAULT_ERR_RETRY_CAP, &mut scratch)
            }
            None => {
                build_heartbeat_enforcing_cap(&rows, now_ms, DEFAULT_ERR_RETRY_CAP, &mut hb_state)
            }
        };
        if !completions.is_empty() {
            // Prepend the event-driven completions so ATC handles the freshly
            // finished children before the polled roster. (Summaries are fenced
            // as untrusted DATA inside `format_completions` — see M3.)
            // Whitespace-collapsed to keep the delivered body a SINGLE line:
            // any embedded newline turns the send into a bracketed paste that
            // can sit unsubmitted in a busy composer (see heartbeat.rs).
            let header = plumbing::format_completions(&completions);
            let header = header.split_whitespace().collect::<Vec<_>>().join(" ");
            b = format!(
                "[HEARTBEAT {}] event-driven completions: {header} {b}",
                crate::fleet::atc::heartbeat::stamp(now_ms)
            );
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
        // The ERR roster this beat saw, for whoever owns the retry ledger, plus
        // the two facts that say whether it is safe to act on: did the scan work,
        // and did the nudge land.
        "err_sessions": err_sessions,
        "roster_valid": roster_valid,
        "ledger_owner": if daemon_exhausted.is_some() { "daemon" } else { "local" },
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
pub(super) async fn fetch_needs() -> Result<Vec<NeedsRow>> {
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

/// Snapshot the daemon's fleet-wide OPEN attention inbox, whose rows carry the
/// resolved routing channels (tcp T5, agents-in-a-box-cdd). Best-effort: a daemon
/// that is down / unreachable errors, and the caller then nudges the FULL needs
/// snapshot (fail-open — the Atc gate must never silence ATC on a transient socket
/// fault). The wire rows already carry channels resolved-at-read by the daemon
/// (`attention_row_to_wire`), so a legacy pre-channel row resolves to its rule's
/// set without any re-resolution at this seam.
async fn fetch_fleet_attention() -> Result<Vec<ainb_hangar_proto::events::AttentionRow>> {
    let client = crate::fleet::bridge::daemon::DaemonClient::from_env()?;
    Ok(client.attention_list_fleet().await?)
}

/// Drop the needs rows the notify rules kept OFF the ATC channel (agents-in-a-box-cdd).
///
/// A session the attention inbox KNOWS but whose resolved `ChannelSet` excludes
/// [`Channel::Atc`] — a board-only `waiting`, or a kind the rules routed away from
/// ATC — stops nudging the ATC brain. A session ABSENT from the inbox (a tmux-only
/// / non-Claude row the daemon never raised an attention for) is KEPT: the Atc
/// gate only applies to rows the daemon actually routed, so wiring the channel in
/// never silently drops an un-routed session. A session with SEVERAL open rows
/// nudges ATC when ANY of them is Atc-routed.
fn filter_atc_channel(
    rows: Vec<NeedsRow>,
    attention: &[ainb_hangar_proto::events::AttentionRow],
) -> Vec<NeedsRow> {
    use ainb_hangar_proto::Channel;
    use std::collections::HashSet;
    let mut known: HashSet<&str> = HashSet::new();
    let mut atc_on: HashSet<&str> = HashSet::new();
    for a in attention {
        known.insert(a.session_id.as_str());
        if a.channels.contains(Channel::Atc) {
            atc_on.insert(a.session_id.as_str());
        }
    }
    rows.into_iter()
        .filter(|r| {
            let sid = r.session.id.as_str();
            !known.contains(sid) || atc_on.contains(sid)
        })
        .collect()
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

/// What the lifecycle hook emits on stdout. Stop, PermissionRequest, and
/// structured PreToolUse outputs share stdout. Hook-specific lines are
/// pre-serialized and emitted verbatim.
enum HookEmit {
    /// A Stop-drain block decision, serialized on emit.
    Stop(plumbing::StopDecision),
    /// A pre-serialized `hookSpecificOutput` permission-decision JSON line.
    Permission(String),
    /// A pre-serialized structured AskUserQuestion hook output.
    Structured(String),
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
        Ok(Some(HookEmit::Permission(json) | HookEmit::Structured(json))) => (true, Some(json)),
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
    // `input` tolerated as an alias, mirroring `approve_context` in the
    // notifyd transition fold — the two surfaces must read the same payloads.
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v.get("tool_input").or_else(|| v.get("input")).cloned())
        .map(|ti| ti.to_string())
        .unwrap_or_default()
}

/// Where the saved tmux window name is parked while an interview banner is up.
/// Which surface owns the next interview for a session.
///
/// DEFAULT IS NATIVE, deliberately. Holding the tool call is the powerful mode
/// but it suppresses Claude's own picker, so a session whose operator is at the
/// terminal would stare at a banner instead of a question. Native costs nothing
/// and never strands anyone; Fleet-hold is opted into per session (or globally)
/// by whoever actually wants to answer from another surface.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InterviewSurface {
    /// Claude draws its own picker immediately; Fleet gets a mirrored card.
    Native,
    /// The hook holds the tool call so Fleet/macOS can answer as exact JSON.
    Fleet,
}

impl InterviewSurface {
    /// Parse a stored token. Anything unrecognised is NATIVE: a corrupt or
    /// half-written file must never silently start holding tool calls.
    fn parse(raw: &str) -> Self {
        match raw.trim() {
            "fleet" => Self::Fleet,
            _ => Self::Native,
        }
    }

    /// The token written to disk.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Fleet => "fleet",
        }
    }
}

/// Directory holding the surface preference: one file per session id, plus a
/// `default` file for the global setting.
#[must_use]
pub fn interview_surface_dir(home: &std::path::Path) -> std::path::PathBuf {
    home.join("fleet").join("interview-surface")
}

/// Resolve the surface for `session_id`: per-session override, else the global
/// default, else [`InterviewSurface::Native`].
///
/// Two small file reads, no TOML parse — this runs on EVERY `AskUserQuestion`
/// and must not become a cost on the hook path.
#[must_use]
pub fn interview_surface(home: &std::path::Path, session_id: &str) -> InterviewSurface {
    let dir = interview_surface_dir(home);
    if !session_id.is_empty() {
        if let Ok(raw) = std::fs::read_to_string(dir.join(sanitize_surface_key(session_id))) {
            return InterviewSurface::parse(&raw);
        }
    }
    // The GLOBAL default lives in config.toml (`[fleet.interview] surface`), not
    // a hidden state file, so it sits visible beside every other setting. Only
    // the per-session override above stays runtime state: it is ephemeral and
    // keyed by uuid, which does not belong in a config file.
    crate::config::AppConfig::load().map_or(InterviewSurface::Native, |config| {
        InterviewSurface::parse(config.fleet.interview.surface_token())
    })
}

/// Persist the surface for a session id, or for the global default when
/// `session_id` is `None`.
///
/// # Errors
/// Returns an [`std::io::Error`] if the directory or file cannot be written.
pub fn set_interview_surface(
    home: &std::path::Path,
    session_id: Option<&str>,
    surface: InterviewSurface,
) -> std::io::Result<()> {
    let Some(session_id) = session_id else {
        // Global default: persist into config.toml so `ainb fleet interview
        // surface fleet` is visible in the same file the user already edits.
        let mut config = crate::config::AppConfig::load()
            .map_err(|e| std::io::Error::other(format!("loading config: {e}")))?;
        config.fleet.interview.surface = Some(surface.token().to_string());
        return config.save().map_err(|e| std::io::Error::other(format!("saving config: {e}")));
    };
    let dir = interview_surface_dir(home);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(sanitize_surface_key(session_id)), surface.token())
}

/// A session id is used as a FILE NAME here, so anything that could escape the
/// directory or collide is replaced. Ids are UUID-shaped in practice; this is a
/// guard, not a transformation anyone should depend on.
fn sanitize_surface_key(session_id: &str) -> String {
    session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn interview_banner_state(home: &std::path::Path) -> Option<std::path::PathBuf> {
    let pane = std::env::var("TMUX_PANE").ok().filter(|value| !value.is_empty())?;
    let key = blake3::hash(pane.as_bytes()).to_hex();
    // Scoped to the `home` THIS invocation was given, not to $AINB_HOME.
    // `plumbing::paths` already treats $AINB_HOME AS the ainb home, so joining
    // `.agents-in-a-box` onto it again wrote
    // `~/.agents-in-a-box/.agents-in-a-box/…`, and an empty $AINB_HOME wrote a
    // RELATIVE path into whatever cwd the hook ran in. Threading `home` also
    // keeps the tests inside their own tempdir.
    Some(home.join("fleet").join("interview-banner").join(format!("{key}.txt")))
}

/// tmux expands `#` sequences in BOTH `rename-window` and `display-message`
/// arguments: `#{...}` is a format, `#(...)` is a shell job. The header is
/// MODEL-AUTHORED text, so every `#` is doubled before interpolation.
fn tmux_escape(value: &str) -> String {
    value.replace('#', "##")
}

/// Whether this process may touch tmux at all.
///
/// `cargo test` inherits `TMUX_PANE`, so without this the unit tests rename the
/// DEVELOPER's own window, and leave it renamed if a test panics between
/// announce and clear.
const fn tmux_banner_enabled() -> bool {
    !cfg!(test)
}

/// Tell the OPERATOR AT THE TERMINAL that this session is holding on a question.
///
/// While the hook blocks, Claude's own UI cannot be reached: hook stdout is read
/// only at exit, and writing to `/dev/tty` fights the TUI repaint. tmux is the
/// one surface that is ours, so the window name carries the flag for the whole
/// wait and a status overlay announces it once.
///
/// Silent on every failure. A session outside tmux, or a tmux that refuses the
/// rename, must still get its interview — an indication is worth nothing if its
/// absence can block the answer path.
/// The provider session id whose interview is being held in THIS tmux pane.
///
/// `prefix + A` runs inside the held pane and must release that pane's own
/// interview, not "the only one held" — with two held at once the latter frees
/// somebody else's question into a terminal nobody is watching. The hook is the
/// only party that knows both facts at once, so it records the pairing here.
#[must_use]
pub fn pane_held_session(home: &std::path::Path) -> Option<String> {
    let path = interview_banner_state(home)?.with_extension("session");
    let id = std::fs::read_to_string(path).ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_string())
}

fn announce_pending_interview(
    home: &std::path::Path,
    session_id: &str,
    questions: &[serde_json::Value],
) {
    if !tmux_banner_enabled() {
        return;
    }
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return;
    };
    if pane.is_empty() {
        return;
    }
    let header = questions
        .first()
        .and_then(|q| q.get("header").or_else(|| q.get("question")))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("question")
        .chars()
        .take(24)
        .collect::<String>();
    let header = tmux_escape(&header);

    // Save the current name AND the automatic-rename setting, so the wait does
    // not permanently freeze a window that was tracking its running program.
    if let Some(path) = interview_banner_state(home) {
        // NEVER overwrite an existing record. A hook that died without clearing
        // (pane closed, session interrupted, hook killed) leaves the banner in
        // place; capturing it again would record "[ASK>FLEET] …" and the yellow
        // style AS the previous state, and the next clear would restore those
        // permanently.
        if !path.exists() {
            if let Ok(out) = std::process::Command::new("tmux")
                .args([
                    "display-message",
                    "-p",
                    "-t",
                    &pane,
                    "-F",
                    "#{window_name}\t#{automatic-rename}",
                ])
                .output()
            {
                if out.status.success() {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    // `#{window-status-style}` reports the EFFECTIVE (inherited)
                    // value, not "set at window scope", so restoring from it would
                    // pin an explicit window-level style and stop the window
                    // following later theme changes. `show-window-options` prints
                    // nothing when the option is unset, which is the distinction.
                    let style = std::process::Command::new("tmux")
                        .args(["show-window-options", "-t", &pane, "window-status-style"])
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .unwrap_or_default();
                    let style = style
                        .split_once(' ')
                        .map_or(String::new(), |(_, value)| value.trim().to_string());
                    let _ = std::fs::write(
                        &path,
                        format!("{}\t{}", String::from_utf8_lossy(&out.stdout).trim(), style),
                    );
                    let _ = std::fs::write(path.with_extension("session"), session_id);
                }
            }
        }
    }
    // Name the SURFACE, not just the fact of a question: this banner only ever
    // appears while the hook is holding for Fleet, and "[ASK]" alone left the
    // operator guessing whether to answer here or somewhere else.
    let _ = std::process::Command::new("tmux")
        .args([
            "rename-window",
            "-t",
            &pane,
            &format!("[ASK>FLEET] {header}"),
        ])
        .status();
    // Light the window up in the status bar. A rename alone is easy to scan
    // past in a bar full of windows; a colour change is not.
    let _ = std::process::Command::new("tmux")
        .args([
            "set-window-option",
            "-t",
            &pane,
            "window-status-style",
            "fg=black,bg=yellow,bold",
        ])
        .status();
    let _ = std::process::Command::new("tmux")
        .args([
            "display-message",
            "-t",
            &pane,
            &format!("Interview waiting: {header} — answer in Fleet, or release to answer here"),
        ])
        .status();
}

/// Restore the window name and its automatic-rename setting once the wait ends,
/// however it ended.
///
/// Called on EVERY exit from the wait (answered, released, timed out): a window
/// left reading `[ASK]` after the question is gone is worse than no banner, and
/// `rename-window` itself turns `automatic-rename` off, so restoring the name
/// without the option would silently freeze the window for good.
fn clear_pending_interview_banner(home: &std::path::Path) {
    if !tmux_banner_enabled() {
        return;
    }
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return;
    };
    if pane.is_empty() {
        return;
    }
    let Some(path) = interview_banner_state(home) else {
        return;
    };
    if let Ok(saved) = std::fs::read_to_string(&path) {
        let mut fields = saved.trim().splitn(3, '\t');
        let previous = fields.next().unwrap_or_default();
        let automatic = fields.next().unwrap_or_default();
        let style = fields.next().unwrap_or_default();
        if !previous.is_empty() {
            let _ = std::process::Command::new("tmux")
                .args(["rename-window", "-t", &pane, previous])
                .status();
        }
        if automatic == "1" {
            let _ = std::process::Command::new("tmux")
                .args(["set-window-option", "-t", &pane, "automatic-rename", "on"])
                .status();
        }
        // Put the status style back. An empty capture means the window had no
        // explicit style, so the option is UNSET rather than set to "" — the
        // latter would pin an empty style and stop it inheriting the theme.
        let _ = if style.is_empty() {
            std::process::Command::new("tmux")
                .args([
                    "set-window-option",
                    "-t",
                    &pane,
                    "-u",
                    "window-status-style",
                ])
                .status()
        } else {
            std::process::Command::new("tmux")
                .args([
                    "set-window-option",
                    "-t",
                    &pane,
                    "window-status-style",
                    style,
                ])
                .status()
        };
    }
    let _ = std::fs::remove_file(path.with_extension("session"));
    let _ = std::fs::remove_file(&path);
}

fn extract_structured_tool_input(
    payload: &str,
) -> Result<(serde_json::Value, Vec<serde_json::Value>, String)> {
    let hook_input: serde_json::Value =
        serde_json::from_str(payload).context("parsing AskUserQuestion hook payload")?;
    let tool_input = hook_input
        .get("tool_input")
        .or_else(|| hook_input.get("input"))
        .cloned()
        .context("AskUserQuestion payload missing tool_input")?;
    let questions = tool_input
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .filter(|questions| !questions.is_empty())
        .cloned()
        .context("AskUserQuestion tool_input missing questions")?;
    let request_identity = serde_json::json!({
        "tool_use_id": hook_input.get("tool_use_id").cloned().unwrap_or(serde_json::Value::Null),
        "tool_input": tool_input.clone(),
    });
    let fingerprint = ainb_plugin_notifyd::broker::request_fingerprint(&request_identity);
    Ok((tool_input, questions, fingerprint))
}

fn structured_emit_json(
    mut tool_input: serde_json::Value,
    resolution: &ainb_plugin_notifyd::broker::StructuredResolution,
) -> String {
    use ainb_plugin_notifyd::broker::StructuredResolution;

    let hook_output = match resolution {
        StructuredResolution::Answered { answers } => {
            let answer_map = answers
                .iter()
                .map(|answer| {
                    (
                        answer.question.clone(),
                        serde_json::Value::String(answer.selected_options.join(", ")),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            let Some(input) = tool_input.as_object_mut() else {
                return structured_emit_json(
                    serde_json::Value::Null,
                    &StructuredResolution::Rejected {
                        reason: "AskUserQuestion tool_input is not an object".to_string(),
                    },
                );
            };
            input.insert("answers".to_string(), serde_json::Value::Object(answer_map));
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "permissionDecisionReason": "answered through Fleet structured broker",
                    "updatedInput": tool_input,
                }
            })
        }
        StructuredResolution::Rejected { reason } => serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        }),
        StructuredResolution::ReleasedToNative => return String::new(),
    };
    hook_output.to_string()
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
    let agent = match std::env::var("AINB_AGENT").ok().as_deref() {
        Some("codex") => "codex",
        _ => "claude",
    };

    hook_core_for_agent(
        &home,
        agent,
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
#[cfg(test)]
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
    hook_core_for_agent(
        home,
        "claude",
        event,
        session_id,
        cwd,
        env_parent,
        done_summary,
        now_ms,
        payload,
        matcher,
    )
}

#[allow(clippy::too_many_arguments)]
fn hook_core_for_agent(
    home: &std::path::Path,
    agent: &str,
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
    let matcher = matcher.map(str::to_string).or_else(|| {
        serde_json::from_str::<serde_json::Value>(payload)
            .ok()
            .and_then(|value| resolve_matcher(base_event, None, Some(&value)))
    });
    let inbox_dir = plumbing::paths::inbox_dir_in(home);

    // Resolve the parent of THIS session (env first, then durable map). Used to
    // route our own completion up and gate broker-mediated control.
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
    // (fixes the split-brain in C-A3).
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
    let is_structured_ask = agent == "claude"
        && base_event == "PreToolUse"
        && matcher.as_deref() == Some("AskUserQuestion")
        && !session_id.is_empty();

    // 1. Event log append — the durable record this session's lifecycle is
    //    event-sourced from. Replaces the per-event status/<id>.json write: notifyd
    //    ingests events.jsonl into the SQLite `events` table, and (Wave 3) folds it
    //    into current_state. The append is O(1), best-effort, and NEVER fails the
    //    hook (any error is swallowed) so a global Stop hook can't wedge any host
    //    session.
    //
    //    The hooks are installed globally, so every session with a provider
    //    session ID writes a raw envelope. Broker-mediated structured answers
    //    and approvals remain limited to known Fleet members below.
    let is_fleet_member = our_parent.is_some() || has_self_inbox || atc_name.is_some();
    let append_event_with = |delivery: Option<&str>| {
        let event_id = uuid::Uuid::new_v4().to_string();
        let raw_payload_stored = persist_raw_hook_payload(home, &event_id, payload).is_ok();
        let mut line = build_event_line_for_agent(
            now_ms,
            &event_id,
            agent,
            session_id,
            cwd,
            base_event,
            matcher.as_deref(),
            payload,
            our_parent.as_deref(),
            raw_payload_stored,
        );
        // The stamp SELECTS THE ANSWER TRANSPORT downstream: `mirrored` routes
        // Fleet's answer through `execute_claude_mirrored_picker` (synthetic tmux
        // keys + screen verification), anything else through the exact-JSON
        // broker branch of `execute_claude_structured`. A blocked ask must NOT be
        // stamped mirrored, or the daemon would type at a picker that is not on
        // screen because we are still holding the tool call open.
        if let Some(delivery) = delivery {
            line["fleet_delivery"] = serde_json::Value::String(delivery.to_string());
        }
        let _ = append_event_line(home, &line);
    };
    // A structured ask defers its event: the branch below decides whether this
    // hook BLOCKS (answer returns as JSON) or yields to Claude's native picker
    // (answer must be keyed in), and the card has to say which.
    if !session_id.is_empty() && !is_structured_ask {
        append_event_with(None);
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

    // 4. PreToolUse(AskUserQuestion): HOLD the tool call open and answer with
    //    exact JSON.
    //
    //    The alternative — return here, let Claude draw its own picker, and have
    //    Fleet type synthetic arrow keys at it — is what this replaces. Once the
    //    hook returns, `hookSpecificOutput` is gone and keystrokes are the only
    //    channel left, which forces the answer's correctness to be re-derived
    //    from a screenshot of a vendor TUI that has no compatibility contract.
    //    That re-derivation broke four times in one week (multi-select Enter
    //    toggling instead of confirming; a mid-token hard wrap with no space to
    //    match on; the same normalisation silently breaking the sibling route;
    //    and 2.1.237's two-column layout splicing a side panel through the
    //    middle of a wrapped label). Holding the call open removes the screen
    //    from the answer path entirely.
    //
    //    Three ways out, and none of them may wedge Claude:
    //      * the broker cannot be reached / registration fails -> yield to the
    //        native picker exactly as before, stamped `mirrored` so Fleet's
    //        answer still has the keystroke transport available.
    //      * a human at the terminal (or Fleet) explicitly releases -> yield,
    //        same stamp, and Claude draws its picker untouched.
    //      * an answer arrives -> emit it as `hookSpecificOutput`, no keys, no
    //        screen parsing.
    //
    //    The await deadline (640s) sits UNDER the hook timeout this event
    //    registers (660s, see `plumbing::hooks`), so the broker's own fallback
    //    always answers before Claude hard-kills the hook.
    // FLEET MEMBERS ONLY, exactly as the permission gate below. Without this an
    // unrelated `claude` typed in any terminal on a host that merely has
    // ainb-hooks installed has its native picker suppressed and its tool call
    // held by our broker, with no surface open to answer it and no CLI verb to
    // release it. `notify.sh` lazily spawns notifyd, so registration succeeds
    // almost anywhere.
    if is_structured_ask {
        // FLEET MEMBERS ONLY may BLOCK, exactly as the permission gate below.
        // Without this an unrelated `claude` typed in any terminal on a host
        // that merely has ainb-hooks installed has its native picker suppressed
        // and its tool call held by our broker, with no surface open to answer
        // it and no CLI verb to release it. `notify.sh` lazily spawns notifyd,
        // so registration would succeed almost anywhere.
        //
        // The CARD is still written for everyone: the hooks are installed
        // globally and a non-member's interview should still be visible to
        // Fleet, just answerable by the keystroke transport rather than held.
        // NATIVE IS THE DEFAULT. Holding is opt-in per session (or globally)
        // via `ainb fleet interview surface`, and only ever for fleet members.
        // A non-member, or a session left on the default, keeps Claude's own
        // picker and gets a mirrored card so Fleet can still see and answer it.
        if !is_fleet_member || interview_surface(home, session_id) != InterviewSurface::Fleet {
            append_event_with(Some("mirrored"));
            return Ok(None);
        }
        let socket = ainb_plugin_notifyd::paths::Paths::under(home).approve_socket;
        let Ok((tool_input, questions, fingerprint)) = extract_structured_tool_input(payload)
        else {
            // Unparseable payload: record it as a keystroke-answerable card
            // rather than holding a tool call we cannot describe to anyone.
            append_event_with(Some("mirrored"));
            return Ok(None);
        };
        let registered = ainb_plugin_notifyd::broker::client_register_structured(
            &socket,
            session_id,
            &fingerprint,
            &questions,
        )
        .unwrap_or(false);
        if !registered {
            // Fleet is an OPTIONAL control surface. A broker outage must never
            // reject or stall Claude's own tool call.
            append_event_with(Some("mirrored"));
            return Ok(None);
        }
        append_event_with(None);
        announce_pending_interview(home, session_id, &questions);
        let resolution = ainb_plugin_notifyd::broker::client_await_structured(
            &socket,
            session_id,
            &fingerprint,
            &questions,
            ainb_plugin_notifyd::broker::CLIENT_AWAIT_DEADLINE,
        );
        clear_pending_interview_banner(home);
        // A resolution meaning NOBODY ANSWERED is not a refusal. Rendering the
        // 640s fallback (or a supersede) as `deny` hands the model a refused
        // tool, which it re-asks, which blocks another 640s: a deny/retry loop
        // for any session with no Fleet surface watching. Yield to the native
        // picker and re-stamp the card `mirrored`, so the keystroke transport
        // applies to the picker now on screen. Only an explicit human dismiss
        // reaches `structured_emit_json` as a deny.
        if resolution.is_unanswered() {
            append_event_with(Some("mirrored"));
            return Ok(None);
        }
        if matches!(
            resolution,
            ainb_plugin_notifyd::broker::StructuredResolution::ReleasedToNative
        ) {
            append_event_with(Some("mirrored"));
            // Ownership yielded on purpose. No hook output means Claude renders
            // its own picker unchanged, and Fleet keeps the keystroke route.
            return Ok(None);
        }
        return Ok(Some(HookEmit::Structured(structured_emit_json(
            tool_input,
            &resolution,
        ))));
    }

    // 5. PermissionRequest: SYNCHRONOUS approve/deny round-trip. The waiting
    //    Claude hook BLOCKS here on the approve broker socket until a human
    //    answers from the fleet UI (or `ainb fleet ... approve/deny`), then
    //    relays the verdict straight back as a `hookSpecificOutput` permission
    //    decision. Fleet members only — an unrelated host session must NOT wedge
    //    on our socket. AskUserQuestion is owned by the structured PreToolUse
    //    path above, never by this generic permission path. `client_await`
    //    re-dials to its deadline, so a notifyd
    //    restart mid-wait is survived; a dead socket / no answer deny-falls-back
    //    (never auto-approves).
    if agent == "claude"
        && base_event == "PermissionRequest"
        && matcher.as_deref() != Some("AskUserQuestion")
        && !session_id.is_empty()
        && is_fleet_member
    {
        let sock = ainb_plugin_notifyd::paths::Paths::under(home).approve_socket;
        let tool = matcher.as_deref().unwrap_or_default();
        let context = extract_permission_context(payload);
        let fingerprint = ainb_plugin_notifyd::broker::permission_fingerprint(tool, &context);
        let decision = ainb_plugin_notifyd::broker::client_await_exact(
            &sock,
            session_id,
            tool,
            &context,
            &fingerprint,
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
/// payload is piped in by `notify.sh` — it never arrives on a terminal, so a
/// TTY stdin (a human ran the verb by hand, or a test binary inherited the
/// shell's terminal) is skipped rather than blocking forever on `read_to_string`
/// waiting for input that will never come.
fn read_stdin_to_string() -> String {
    use std::io::{IsTerminal, Read};
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return String::new();
    }
    let mut buf = String::new();
    let _ = stdin.lock().read_to_string(&mut buf);
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
        "StopFailure" => {
            return v
                .get("error")
                .or_else(|| v.get("error_type"))
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        _ => return None,
    };
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn current_tmux_identity() -> Option<(String, String)> {
    let pane = std::env::var_os("TMUX_PANE").filter(|value| !value.is_empty())?;
    let tmux = std::ffi::OsString::from("tmux");
    let output = std::process::Command::new(tmux)
        .args([
            std::ffi::OsStr::new("display-message"),
            std::ffi::OsStr::new("-p"),
            std::ffi::OsStr::new("-t"),
            pane.as_os_str(),
            std::ffi::OsStr::new("-F"),
            std::ffi::OsStr::new(
                "#{session_name}\t#{window_index}\t#{pane_index}\t#{pane_id}\t#{pane_pid}\t#{session_created}",
            ),
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    parse_hook_tmux_identity(std::str::from_utf8(&output.stdout).ok()?)
}

fn parse_hook_tmux_identity(row: &str) -> Option<(String, String)> {
    let fields = row.trim().split('\t').collect::<Vec<_>>();
    if fields.len() != 6 || fields.iter().any(|field| field.is_empty()) {
        return None;
    }
    let target = format!("{}:{}.{}", fields[0], fields[1], fields[2]);
    let fingerprint = format!(
        "pane={};pid={};session_started={}",
        fields[3], fields[4], fields[5]
    );
    Some((target, fingerprint))
}

/// Build the canonical `events.jsonl` line for one hook fire. This is the exact
/// shape the notifyd ingest tailer parses (`crate ainb-plugin-notifyd::ingest`)
/// and the Hangar materializer consumes: ts, session_id, cwd, transcript_path,
/// agent, event_type, matcher, and a bounded index payload plus raw sidecar.
/// Parent is folded into the payload so the materializer can set current_state.parent.
///
/// The raw payload is parsed exactly ONCE here (host-hot-path: this runs on
/// every Claude lifecycle event) and the parsed `Value` is
/// reused for the matcher + transcript_path fields and the embedded payload.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn build_event_line(
    now_ms: i64,
    event_id: &str,
    session_id: &str,
    cwd: &str,
    base_event: &str,
    matcher: Option<&str>,
    payload: &str,
    parent: Option<&str>,
    raw_payload_stored: bool,
) -> serde_json::Value {
    build_event_line_for_agent(
        now_ms,
        event_id,
        "claude",
        session_id,
        cwd,
        base_event,
        matcher,
        payload,
        parent,
        raw_payload_stored,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_event_line_for_agent(
    now_ms: i64,
    event_id: &str,
    agent: &str,
    session_id: &str,
    cwd: &str,
    base_event: &str,
    matcher: Option<&str>,
    payload: &str,
    parent: Option<&str>,
    raw_payload_stored: bool,
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
    let (tmux_target, process_start_fingerprint) = current_tmux_identity()
        .map_or((None, None), |(target, fingerprint)| {
            (Some(target), Some(fingerprint))
        });
    // Bounded raw payload: embed the parsed value if it fit, else a truncation
    // marker so the line stays small and always valid JSON.
    let payload_val: serde_json::Value = if payload.len() <= MAX_EVENT_PAYLOAD_BYTES {
        parsed.clone().unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({ "_truncated": true, "_bytes": payload.len() })
    };
    serde_json::json!({
        "event_id": event_id,
        "ts": now_ms,
        "session_id": session_id,
        "cwd": cwd,
        "transcript_path": transcript_path,
        "agent": agent,
        "event_type": base_event,
        "matcher": matcher_val,
        "parent": parent,
        "tmux_target": tmux_target,
        "process_start_fingerprint": process_start_fingerprint,
        "payload": payload_val,
        "raw_payload_ref": raw_payload_stored.then_some(event_id),
    })
}

/// Persist a complete hook payload before its bounded JSONL index entry. Each
/// UUID-named file is published with rename, so concurrent global hooks cannot
/// interleave a large envelope and daemon restart can replay it exactly.
fn persist_raw_hook_payload(
    home: &std::path::Path,
    event_id: &str,
    payload: &str,
) -> std::io::Result<()> {
    let dir = home.join("hangar").join("provider-events");
    std::fs::create_dir_all(&dir)?;
    let destination = dir.join(format!("{event_id}.json"));
    let temporary = dir.join(format!(".{event_id}.{}.tmp", std::process::id()));
    std::fs::write(&temporary, payload)?;
    std::fs::rename(temporary, destination)
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

    /// A minimal idle needs row for the given session id.
    fn needs_row(session_id: &str) -> NeedsRow {
        use crate::fleet::read::{IdleContext, NeedsContext, RouteHint};
        use crate::fleet::types::{Session, SessionSource};
        NeedsRow {
            session: Session {
                id: session_id.into(),
                cwd: format!("/tmp/{session_id}"),
                pid: None,
                git_root: None,
                tmux_session: None,
                workspace_name: None,
                worktree_path: None,
                peer_id: None,
                bg_job_id: None,
                transcript_path: None,
                sources: vec![SessionSource::Ainb],
                summary: None,
                last_seen_ms: None,
            },
            context: NeedsContext::Idle(IdleContext {
                idle_minutes: 9,
                last_assistant_text: None,
            }),
            route_hint: RouteHint::Tmux,
            enrich_key: String::new(),
            enriched: None,
            need_enrich: false,
            source: None,
        }
    }

    /// A minimal open attention row for `session_id` routed to `channels`.
    fn attention_row(
        session_id: &str,
        channels: ainb_hangar_proto::ChannelSet,
    ) -> ainb_hangar_proto::events::AttentionRow {
        ainb_hangar_proto::events::AttentionRow {
            id: format!("att:{session_id}:0"),
            session_id: session_id.into(),
            cwd: format!("/tmp/{session_id}"),
            workspace_id: None,
            kind: "waiting".into(),
            payload: "{}".into(),
            degraded: false,
            created_at: 0,
            channels,
        }
    }

    #[test]
    fn atc_channel_gate_drops_atc_excluded_but_keeps_routed_and_unrouted() {
        use ainb_hangar_proto::{Channel, ChannelSet};
        let rows = vec![
            needs_row("s-atc"),
            needs_row("s-board"),
            needs_row("s-unknown"),
        ];
        let attention = vec![
            // Routed to ATC → keeps nudging.
            attention_row("s-atc", ChannelSet::from_channels([Channel::Atc])),
            // Board-only (waiting) → excludes Atc → stops nudging ATC.
            attention_row("s-board", ChannelSet::NONE),
            // s-unknown has NO attention row → kept (never silently dropped).
        ];
        let kept: Vec<String> =
            filter_atc_channel(rows, &attention).into_iter().map(|r| r.session.id).collect();
        assert_eq!(
            kept,
            vec!["s-atc".to_string(), "s-unknown".to_string()],
            "atc-excluded board-only row dropped; atc-routed + un-routed rows kept"
        );
    }

    #[test]
    fn atc_channel_gate_keeps_a_session_with_any_atc_routed_row() {
        use ainb_hangar_proto::{Channel, ChannelSet};
        // A session with one board-only row AND one Atc-routed row still nudges.
        let rows = vec![needs_row("s-multi")];
        let attention = vec![
            attention_row("s-multi", ChannelSet::NONE),
            attention_row(
                "s-multi",
                ChannelSet::from_channels([Channel::Os, Channel::Atc]),
            ),
        ];
        let kept = filter_atc_channel(rows, &attention);
        assert_eq!(
            kept.len(),
            1,
            "any Atc-routed row keeps the session nudging"
        );
    }

    #[test]
    fn heartbeat_cron_maps_interval_to_a_valid_cron() {
        // Sub-hour intervals map to an every-N-minute cron.
        assert_eq!(heartbeat_cron_for_interval(2), "*/2 * * * *");
        assert_eq!(heartbeat_cron_for_interval(15), "*/15 * * * *");
        // 0 is clamped to 1 (still a valid cron, never a panic / empty field).
        assert_eq!(heartbeat_cron_for_interval(0), "*/1 * * * *");
        // 60+ minutes maps to hourly (a `*/N` minute field only spans 0-59).
        assert_eq!(heartbeat_cron_for_interval(60), "0 * * * *");
        assert_eq!(heartbeat_cron_for_interval(120), "0 * * * *");
    }

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

    // --- Host-wide events.jsonl capture -------------------------------------

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
    fn unrelated_session_appends_event_without_broker_control() {
        let home = TempDir::new().unwrap();
        // A session with no Fleet relationship still contributes durable
        // observation, but it must not block on a broker.
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
            home.path().join("events.jsonl").exists(),
            "unrelated session must append one source event"
        );
        let lines = read_event_lines(home.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["session_id"], "random-unrelated-session");
    }

    #[test]
    fn codex_permission_event_is_durable_without_claude_broker_control() {
        let home = TempDir::new().unwrap();
        let raw = r#"{"hook_event_name":"PermissionRequest","session_id":"codex-sid","cwd":"/work","tool_name":"shell_command"}"#;

        let result = hook_core_for_agent(
            home.path(),
            "codex",
            "PermissionRequest",
            "codex-sid",
            "/work",
            None,
            None,
            7,
            raw,
            None,
        )
        .unwrap();

        assert!(
            result.is_none(),
            "Codex CLI hooks must not use Claude's blocking broker"
        );
        let line = read_event_lines(home.path()).pop().expect("durable Codex event");
        assert_eq!(line["agent"], "codex");
        assert_eq!(line["event_type"], "PermissionRequest");
        assert_eq!(line["matcher"], "shell_command");
        let event_id = line["event_id"].as_str().expect("event id");
        assert_eq!(
            std::fs::read_to_string(
                home.path().join("hangar/provider-events").join(format!("{event_id}.json")),
            )
            .unwrap(),
            raw,
            "the durable sidecar retains the exact provider payload"
        );
    }

    #[test]
    fn atc_session_appends_a_well_formed_event_line() {
        let home = TempDir::new().unwrap();
        let cwd = provision_atc(home.path(), "tower");
        // An ATC-managed session appends one event line.
        // Stamp a transcript_path + the PreToolUse matcher into the payload to
        // assert they land in the canonical line.
        //
        // Deliberately a NON-structured matcher: PreToolUse(AskUserQuestion) is
        // fenced onto the structured-ask path, which appends only once the
        // broker registers, and is covered by its own fail-open tests. This test
        // is about the canonical line shape, not about that fence.
        let payload = r#"{"transcript_path":"/t/atc.jsonl","tool_name":"Bash"}"#;
        hook_core(
            home.path(),
            "PreToolUse",
            "atc-sid",
            cwd.to_str().unwrap(),
            None,
            None,
            7,
            payload,
            Some("Bash"),
        )
        .unwrap();
        let lines = read_event_lines(home.path());
        assert_eq!(lines.len(), 1, "ATC session must append one event line");
        let l = &lines[0];
        assert_eq!(l["session_id"], "atc-sid");
        assert_eq!(l["event_type"], "PreToolUse");
        assert_eq!(l["matcher"], "Bash");
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
        // A child with a live env parent writes event and routes completion.
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
            "event-oversized",
            "session-with-a-realistically-long-claude-id-0123456789abcdef",
            "/Users/someone/very/deep/nested/work/tree/path/that/is/long",
            "Stop",
            None,
            &big,
            Some("some-parent-instance-name"),
            true,
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
            "event-max",
            &"s".repeat(64),
            &"/".repeat(256),
            "PreToolUse",
            Some("AskUserQuestion"),
            &payload,
            Some(&"p".repeat(64)),
            true,
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
        let line = build_event_line(
            7,
            "event-small",
            "sid",
            "/cwd",
            "PreToolUse",
            None,
            payload,
            None,
            true,
        );
        assert_eq!(line["matcher"], "AskUserQuestion");
        assert_eq!(line["transcript_path"], "/t/atc.jsonl");
        assert_eq!(line["payload"]["tool_name"], "AskUserQuestion");
    }

    #[test]
    fn hook_tmux_identity_matches_discovery_fingerprint_shape() {
        let identity = parse_hook_tmux_identity("claude-a\t2\t1\t%9\t4242\t1700000000\n")
            .expect("exact tmux identity");
        assert_eq!(identity.0, "claude-a:2.1");
        assert_eq!(identity.1, "pane=%9;pid=4242;session_started=1700000000");
        assert!(parse_hook_tmux_identity("claude-a\t2\t1").is_none());
    }

    // --- H-A1: the hook NEVER returns Err (always exit 0) --------------------

    #[test]
    fn hook_returns_ok_even_on_unrelated_session() {
        // A global Stop hook must never wedge an unrelated host session: the
        // outcome the real `hook()` derives has to be exit 0 even when the home
        // holds no ATC instance and no inbox for the session.
        //
        // We drive `hook_core` against a tempdir rather than calling `hook()`,
        // because `hook_inner` resolves the process-wide `ainb_home()` — calling
        // it here would append a synthetic Stop event and a raw-payload sidecar
        // to the DEVELOPER'S live `~/.agents-in-a-box`, which the daemon then
        // ingests into `fleet_provider_event`. `swallow_hook_result` is the same
        // wrapper `hook()` applies, so the exit-0 guarantee is still the thing
        // under test.
        let home = TempDir::new().unwrap();
        let result = hook_core(
            home.path(),
            "Stop",
            "unrelated",
            "",
            None,
            None,
            1,
            "{}",
            None,
        );
        let (exit_ok, emitted) = swallow_hook_result(result);
        assert!(exit_ok, "hook must always yield exit 0");
        assert!(
            emitted.is_none(),
            "an unrelated session emits no decision JSON"
        );
    }

    /// H-A1 (forced error): the drain error path is reachable, and the `hook()`
    /// wrapper swallows it. We force the inbox lock open to fail by making the
    /// `.lock` PATH a directory, so `Inbox::lock` errors inside
    /// `drain_with_budget` and the error propagates out of `hook_core`. The
    /// `hook()` swallow wrapper ([`swallow_hook_result`]) then maps that Err to
    /// `Ok(())` → exit 0, which we assert directly on the real error value.
    /// Read `events.jsonl` once the daemon has actually written it.
    ///
    /// The hook returning is NOT proof the event reached disk: the daemon
    /// writes that log on its own schedule. Reading it straight away is a race
    /// that loses on a slow runner, and it panics with `NotFound`, which reads
    /// like a missing feature rather than the timing bug it is.
    fn events_jsonl(home: &std::path::Path) -> String {
        let path = home.join("events.jsonl");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match std::fs::read_to_string(&path) {
                Ok(text) if !text.trim().is_empty() => return text,
                other => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the daemon never wrote {}: {other:?}",
                        path.display()
                    );
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
    }

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

    #[test]
    fn structured_request_fingerprint_includes_tool_use_id() {
        let payload = |tool_use_id: &str| {
            serde_json::json!({
                "tool_name": "AskUserQuestion",
                "tool_use_id": tool_use_id,
                "tool_input": {
                    "questions": [{
                        "question": "Same question?",
                        "header": "Same",
                        "options": [{"label": "Yes"}],
                        "multiSelect": false
                    }]
                }
            })
            .to_string()
        };
        let (_, _, first) = extract_structured_tool_input(&payload("toolu_1")).unwrap();
        let (_, _, second) = extract_structured_tool_input(&payload("toolu_2")).unwrap();
        assert_ne!(
            first, second,
            "later identical question needs distinct identity"
        );
    }

    #[test]
    fn ask_hook_mirrors_native_picker_and_persists_fleet_request() {
        let home = TempDir::new().unwrap();
        let cwd = home.path().join("plain-claude-worktree");
        std::fs::create_dir_all(&cwd).unwrap();
        let payload = serde_json::json!({
            "tool_name": "AskUserQuestion",
            "tool_use_id": "toolu_mirrored",
            "tool_input": {
                "questions": [{
                    "question": "Continue?",
                    "header": "Continue",
                    "options": [{"label": "Yes"}, {"label": "No"}]
                }]
            }
        })
        .to_string();

        let emitted = hook_core(
            home.path(),
            "PreToolUse",
            "mirrored-session",
            cwd.to_str().unwrap(),
            None,
            None,
            50,
            &payload,
            Some("AskUserQuestion"),
        )
        .unwrap();
        assert!(emitted.is_none(), "Claude must render its native picker");
        let events = events_jsonl(home.path());
        let event: serde_json::Value = serde_json::from_str(events.trim()).unwrap();
        assert_eq!(event["fleet_delivery"], "mirrored");
        assert_eq!(
            event["payload"]["tool_input"]["questions"][0]["question"],
            "Continue?"
        );
    }

    #[test]
    fn native_is_the_configured_default_surface() {
        // Pinned WITHOUT touching the filesystem. `interview_surface` falls back
        // to the real `AppConfig::load()`, so asserting the default through it
        // reads the developer's own config.toml and fails on any machine that
        // has opted into fleet — which is exactly how this test first broke.
        // Absent, not "native": the merge needs to tell "unset" from "set to
        // native", and the default is applied by surface_token().
        assert_eq!(crate::config::InterviewConfig::default().surface, None);
        assert_eq!(
            crate::config::InterviewConfig::default().surface_token(),
            "native"
        );
    }

    #[test]
    fn a_per_session_surface_overrides_whatever_the_global_default_is() {
        let home = TempDir::new().unwrap();
        let global = super::interview_surface(home.path(), "");
        super::set_interview_surface(home.path(), Some("sess-a"), super::InterviewSurface::Fleet)
            .unwrap();
        assert_eq!(
            super::interview_surface(home.path(), "sess-a"),
            super::InterviewSurface::Fleet
        );
        // Per-session, not global: a sibling session still follows the default.
        assert_eq!(super::interview_surface(home.path(), "sess-b"), global);
        // And it flips back.
        super::set_interview_surface(home.path(), Some("sess-a"), super::InterviewSurface::Native)
            .unwrap();
        assert_eq!(
            super::interview_surface(home.path(), "sess-a"),
            super::InterviewSurface::Native
        );
    }

    #[test]
    fn unrecognised_surface_token_reads_as_native() {
        // A truncated or hand-edited value must never silently start holding
        // tool calls.
        let home = TempDir::new().unwrap();
        let dir = super::interview_surface_dir(home.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sess-c"), "flee").unwrap();
        assert_eq!(
            super::interview_surface(home.path(), "sess-c"),
            super::InterviewSurface::Native
        );
    }

    #[test]
    fn ask_hook_from_a_non_member_session_never_blocks_but_still_persists_a_card() {
        // An unrelated `claude` on a host that merely has ainb-hooks installed
        // must keep its native picker. Holding its tool call would suppress the
        // picker with no Fleet surface open to answer and no CLI verb to
        // release it. The card is still written so Fleet can see the interview
        // and answer it through the keystroke transport.
        let home = TempDir::new().unwrap();
        let cwd = home.path().join("unrelated-worktree");
        std::fs::create_dir_all(&cwd).unwrap();
        let payload = serde_json::json!({
            "tool_name": "AskUserQuestion",
            "tool_use_id": "toolu_nonmember",
            "tool_input": {
                "questions": [{
                    "question": "Region?",
                    "header": "Region",
                    "options": [{"label": "eu"}, {"label": "us"}]
                }]
            }
        });
        let started = std::time::Instant::now();
        let emitted = hook_core(
            home.path(),
            "PreToolUse",
            "nonmember-session",
            cwd.to_str().unwrap(),
            None,
            None,
            50,
            &payload.to_string(),
            Some("AskUserQuestion"),
        )
        .unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "a non-member ask must return immediately, not hold the tool call"
        );
        assert!(
            emitted.is_none(),
            "a non-member ask must leave Claude's native picker unblocked"
        );
        let events = std::fs::read_to_string(home.path().join("events.jsonl")).unwrap();
        assert!(
            events.contains("\"fleet_delivery\":\"mirrored\""),
            "the card must stay keystroke-answerable: {events}"
        );
    }

    #[test]
    fn ask_hook_without_broker_fails_open_and_persists_a_mirrored_fleet_card() {
        let home = TempDir::new().unwrap();
        let cwd = home.path().join("plain-claude-worktree");
        std::fs::create_dir_all(&cwd).unwrap();
        let payload = serde_json::json!({
            "tool_name": "AskUserQuestion",
            "tool_use_id": "toolu_missing_broker",
            "tool_input": {
                "questions": [{
                    "question": "Continue?",
                    "header": "Continue",
                    "options": [{"label": "Yes"}]
                }]
            }
        })
        .to_string();

        let emitted = hook_core(
            home.path(),
            "PreToolUse",
            "missing-broker-session",
            cwd.to_str().unwrap(),
            None,
            None,
            50,
            &payload,
            Some("AskUserQuestion"),
        )
        .unwrap();
        assert!(
            emitted.is_none(),
            "unavailable Fleet broker must leave Claude's native interview unblocked"
        );
        let events = events_jsonl(home.path());
        assert!(events.contains("\"fleet_delivery\":\"mirrored\""));
    }

    #[test]
    fn ask_hook_with_unreadable_payload_fails_open() {
        let home = TempDir::new().unwrap();
        let cwd = home.path().join("plain-claude-worktree");
        std::fs::create_dir_all(&cwd).unwrap();

        let emitted = hook_core(
            home.path(),
            "PreToolUse",
            "malformed-payload-session",
            cwd.to_str().unwrap(),
            None,
            None,
            50,
            "not-json",
            Some("AskUserQuestion"),
        )
        .unwrap();

        assert!(
            emitted.is_none(),
            "Fleet hook parse failures must leave Claude's native tool unblocked"
        );
    }

    /// The daemon-owned branch of `repair` is not reachable without a live
    /// daemon, so pin the arbitration rule directly: exactly one scheduler, and
    /// a disabled heartbeat never gets one. An implementation that installed
    /// the local timer alongside a daemon registration (two nudges per interval
    /// into one session, split-brain retry caps) fails the middle case; one
    /// that resurrected a disabled heartbeat fails the last two.
    #[test]
    fn repair_picks_exactly_one_scheduler() {
        assert_eq!(repair_scheduler(true, true), Scheduler::Daemon);
        assert_eq!(repair_scheduler(true, false), Scheduler::LocalTimer);
        assert_eq!(repair_scheduler(false, true), Scheduler::None);
        assert_eq!(repair_scheduler(false, false), Scheduler::None);

        assert_eq!(Scheduler::Daemon.as_str(), "daemon");
        assert_eq!(Scheduler::LocalTimer.as_str(), "local_timer");
        assert_eq!(Scheduler::None.as_str(), "none");
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ask_hook_returns_complete_structured_answer_without_text_routing() {
        use ainb_plugin_notifyd::broker::{
            BrokerState, StructuredQuestionAnswer, client_answer_structured, client_list,
        };
        use tokio::net::UnixListener;

        let home = TempDir::new().unwrap();
        let cwd = home.path().join("plain-claude-worktree");
        std::fs::create_dir_all(&cwd).unwrap();
        let paths = ainb_plugin_notifyd::paths::Paths::under(home.path());
        std::fs::create_dir_all(paths.approve_socket.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&paths.approve_socket).unwrap();
        let server = tokio::spawn(ainb_plugin_notifyd::broker::serve(
            listener,
            BrokerState::with_timeout(std::time::Duration::from_secs(30)),
        ));

        let payload = serde_json::json!({
            "tool_name": "AskUserQuestion",
            "tool_use_id": "toolu_ask_1",
            "tool_input": {
                "questions": [
                    {
                        "question": "Region?",
                        "header": "Region",
                        "options": [
                            {"label": "EU", "description": "Europe"},
                            {"label": "US", "description": "United States"}
                        ],
                        "multiSelect": false
                    },
                    {
                        "question": "Checks?",
                        "header": "Checks",
                        "options": [
                            {"label": "Lint", "description": "Run linter"},
                            {"label": "Test", "description": "Run tests"}
                        ],
                        "multiSelect": true
                    }
                ]
            }
        });
        let original_questions = payload["tool_input"]["questions"].clone();
        // Native is the DEFAULT, so this session has to opt into fleet-hold or
        // the hook returns immediately and there is nothing to answer.
        super::set_interview_surface(
            home.path(),
            Some("ask-session"),
            super::InterviewSurface::Fleet,
        )
        .unwrap();
        let hook_home = home.path().to_path_buf();
        let hook_cwd = cwd.clone();
        let hook_payload = payload.to_string();
        let waiter = tokio::task::spawn_blocking(move || {
            hook_core(
                &hook_home,
                "PreToolUse",
                "ask-session",
                hook_cwd.to_str().expect("temporary cwd is valid UTF-8"),
                // A parent makes this a FLEET MEMBER, which is what earns the
                // right to hold the tool call open. Non-members get a mirrored
                // card and their native picker, never a block.
                Some("ask-parent"),
                None,
                50,
                &hook_payload,
                Some("AskUserQuestion"),
            )
        });

        let pending = loop {
            let socket = paths.approve_socket.clone();
            let listed = tokio::task::spawn_blocking(move || client_list(&socket))
                .await
                .unwrap()
                .unwrap_or_default();
            if let Some(pending) = listed.into_iter().find(|item| item.session_id == "ask-session")
            {
                break pending;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        assert_eq!(
            pending.questions,
            original_questions.as_array().unwrap().clone()
        );
        let fingerprint = pending.request_fingerprint.unwrap();
        // The hook REGISTERS with the broker before it appends the event, so a
        // pending observed by `client_list` does not yet imply the file exists.
        // Reading immediately made this test pass or fail on scheduling luck.
        let events_path = home.path().join("events.jsonl");
        let mut events = String::new();
        for _ in 0..200 {
            if let Ok(read) = std::fs::read_to_string(&events_path) {
                if read.contains("ask-session") {
                    events = read;
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(!events.is_empty(), "hook never appended its Fleet event");
        assert!(
            events.contains("ask-session"),
            "Fleet event must appear only after broker registration"
        );

        let stale_socket = paths.approve_socket.clone();
        let stale = tokio::task::spawn_blocking(move || {
            client_answer_structured(
                &stale_socket,
                "ask-session",
                "fnv1a64:stale",
                &[StructuredQuestionAnswer {
                    question: "Region?".to_string(),
                    selected_options: vec!["EU".to_string()],
                }],
            )
        })
        .await
        .unwrap()
        .unwrap();
        assert!(stale.stale);
        assert!(!stale.matched);

        let answer_socket = paths.approve_socket.clone();
        let accepted = tokio::task::spawn_blocking(move || {
            client_answer_structured(
                &answer_socket,
                "ask-session",
                &fingerprint,
                &[
                    StructuredQuestionAnswer {
                        question: "Region?".to_string(),
                        selected_options: vec!["EU".to_string()],
                    },
                    StructuredQuestionAnswer {
                        question: "Checks?".to_string(),
                        selected_options: vec!["Lint".to_string(), "Test".to_string()],
                    },
                ],
            )
        })
        .await
        .unwrap()
        .unwrap();
        assert!(accepted.matched);

        let emitted = waiter.await.unwrap().unwrap().expect("structured hook output");
        let HookEmit::Structured(line) = emitted else {
            panic!("AskUserQuestion must emit structured hook output");
        };
        let output: serde_json::Value = serde_json::from_str(&line).unwrap();
        let specific = &output["hookSpecificOutput"];
        assert_eq!(specific["hookEventName"], "PreToolUse");
        assert_eq!(specific["permissionDecision"], "allow");
        assert_eq!(specific["updatedInput"]["questions"], original_questions);
        assert_eq!(specific["updatedInput"]["answers"]["Region?"], "EU");
        assert_eq!(specific["updatedInput"]["answers"]["Checks?"], "Lint, Test");
        assert!(
            output.get("text").is_none(),
            "must not route labels as generic text"
        );

        server.abort();
        let _ = std::fs::remove_file(&paths.approve_socket);
    }
}
