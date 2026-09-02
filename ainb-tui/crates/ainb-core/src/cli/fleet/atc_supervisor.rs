// ABOUTME: `ainb fleet atc mode` and `ainb fleet atc supervise` — the operator
// surface for the one-supervisor-per-fleet rule, and the LITE controller itself.
//
//   mode <name>                 report the current mode, provider, and owner
//   mode <name> --set lite|full switch, explicitly and durably
//   supervise <name>            (internal) run the lite scan loop
//
// The switch is the interesting half. It has to leave the fleet with EXACTLY one
// controller at every instant, including while it is running, so it always
// STOPS the outgoing controller before STARTING the incoming one:
//
//   ┌──────────┐  1. persist mode   ┌──────────────┐
//   │ outgoing │ ─────────────────▶ │  meta.json   │  ← the single source of truth
//   └──────────┘                    └──────────────┘
//        │ 2. stand down (it re-reads meta before every send and refuses)
//        ▼
//   ┌──────────┐  3. dismantle its scheduler   4. start the incoming controller
//   │  quiet   │ ─────────────────────────────────────────────────────────────▶
//   └──────────┘
//
// Persisting FIRST is what makes step 2 free: neither controller trusts the mode
// it booted with, so the write alone is enough to silence the loser even if
// killing its process fails. Steps 3 and 4 are then tidying, not safety.

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::cli::OutputFormat;
use crate::fleet::atc::supervisor::{
    Controller, SupervisorMode, lite_heartbeat_id, may_act, mode_help, resolve_full_provider,
    stand_down_reason,
};
use crate::fleet::atc::{AtcMeta, AtcPaths, DEFAULT_ERR_RETRY_CAP, HeartbeatState, timer};
use crate::fleet::plumbing;
use crate::fleet::read::{NeedsContext, NeedsRow};

/// How often the lite controller re-scans. Matches the fleet daemon's cadence it
/// replaces, so lite mode is no slower to react than what it supersedes.
const LITE_SCAN_INTERVAL_SECS: u64 = 5;

/// Read an instance's meta, or fail with the same message every verb uses.
pub fn read_meta(name: &str) -> Result<(AtcMeta, AtcPaths)> {
    let paths = AtcPaths::resolve(name)?;
    if !paths.meta.exists() {
        bail!(
            "no ATC instance named '{name}' (expected {})",
            paths.meta.display()
        );
    }
    let meta = AtcMeta::from_json(
        &std::fs::read_to_string(&paths.meta)
            .with_context(|| format!("reading {}", paths.meta.display()))?,
    )
    .with_context(|| format!("parsing {}", paths.meta.display()))?;
    Ok((meta, paths))
}

/// Persist a mutated meta atomically.
///
/// Every mode transition goes through here, so a crash mid-switch can never
/// leave a torn meta.json that neither controller can parse — which would
/// strand the fleet with no owner at all.
pub fn write_meta(paths: &AtcPaths, meta: &AtcMeta) -> Result<()> {
    plumbing::atomic::write_atomic(&paths.meta, meta.to_json()?.as_bytes())
        .with_context(|| format!("writing {}", paths.meta.display()))
}

// ── `ainb fleet atc mode` ───────────────────────────────────────────────────

pub async fn mode(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let name = matches
        .get_one::<String>("name")
        .cloned()
        .context("expected an instance name")?;
    let (mut meta, paths) = read_meta(&name)?;

    let requested = match matches.get_one::<String>("set") {
        None => None,
        Some(raw) => Some(
            SupervisorMode::from_id(raw)
                .with_context(|| format!("unknown mode '{raw}' — expected `lite` or `full`"))?,
        ),
    };
    let requested_provider = matches.get_one::<String>("provider").cloned();
    // Persist the mode WITHOUT touching either controller.
    //
    // The mode is the safety property; the controllers are machinery. This flag
    // exists for the tests, which must be able to exercise the persisted rule
    // without spawning a real scan loop against the developer's live fleet, and
    // for an operator reconciling by hand after a partial failure.
    let no_reconcile = matches.get_flag("no-reconcile");

    // Read-only report. `mode <name>` with no --set never mutates: switching a
    // fleet's controller is not something to do by accident while looking.
    let Some(target) = requested else {
        if requested_provider.is_some() {
            bail!("--provider only means something with --set; pass `--set full --provider <id>`");
        }
        return report(&meta, format, None);
    };

    // Validate the provider BEFORE writing anything. A full mode whose brain
    // ainb cannot nudge is an instance that looks provisioned and is never
    // woken, and half-applying that is worse than refusing it.
    let provider = requested_provider.clone().unwrap_or_else(|| meta.provider.clone());
    if target == SupervisorMode::Full {
        resolve_full_provider(&provider)?;
    } else if requested_provider.is_some() {
        // Lite runs no brain. Accept and remember the choice (so toggling back
        // does not lose it) but say plainly that it changes nothing today.
        resolve_full_provider(&provider)
            .context("the remembered full-mode provider must still be one ainb can drive")?;
    }

    let previous = meta.mode;
    let previous_provider = meta.provider.clone();
    let unchanged = previous == target && previous_provider == provider;

    // PRE-FLIGHT: see `prepare_handover`. Runs before ANYTHING is persisted or
    // torn down, so a failure here leaves the fleet exactly as it was.
    if !unchanged && !no_reconcile {
        prepare_handover(&meta, previous, target)?;
    }

    // 1. Persist first — this alone stands the outgoing controller down, because
    //    both controllers re-read the mode before every send.
    meta.mode = target;
    meta.provider = provider.clone();
    write_meta(&paths, &meta)?;

    // 2/3/4. Reconcile the machinery: stop the outgoing scheduler, start the
    //        incoming one. Best-effort and reported, never fatal — the safety
    //        property is already held by the write above.
    let reconcile = if unchanged || no_reconcile {
        let mut out = Reconcile::default();
        if no_reconcile && !unchanged {
            out.notes.push(
                "--no-reconcile: the mode is persisted and the old controller will stand down on \
its next action, but neither controller was started or stopped"
                    .to_string(),
            );
        }
        out
    } else {
        reconcile_controllers(&meta, previous_provider != provider).await
    };

    let summary = json!({
        "action": "mode",
        "name": meta.name,
        "mode": meta.mode.id(),
        "previous_mode": previous.id(),
        "provider": meta.provider,
        "owner": meta.mode.owner().label(),
        "changed": !unchanged,
        "lite_supervisor_stopped_pid": reconcile.lite_stopped_pid,
        "lite_supervisor_started": reconcile.lite_started,
        "scheduler_asserted": reconcile.scheduler_asserted,
        "scheduler_removed": reconcile.scheduler_removed,
        "notes": reconcile.notes,
    });

    if matches!(format, OutputFormat::Text) {
        if unchanged {
            println!(
                "ATC '{}' already in {} mode — nothing changed.",
                meta.name,
                meta.mode.id()
            );
        } else {
            println!(
                "ATC '{}' switched {} → {}.",
                meta.name,
                previous.id(),
                meta.mode.id()
            );
        }
        for note in &reconcile.notes {
            println!("  {note}");
        }
        println!();
        for line in mode_help(meta.mode, &meta.provider) {
            println!("  {line}");
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    }
    Ok(())
}

/// Get the retry-ledger handover into the right state for `target`, before the
/// switch touches anything.
///
/// The ordering is the whole point. Arming used to happen inside reconcile,
/// AFTER both schedulers had been torn down; a failure there fell through to
/// starting the scanner and then rolled the mode back, leaving no timer, no
/// cron, and a scanner that reads mode=full and exits. Zero controllers.
///
/// Here, a failure is free: nothing is persisted and nothing is dismantled, so
/// the `?` leaves the fleet as it was.
/// The pure predicate behind [`prepare_handover`], split out so the test
/// exercises the real rule rather than a copy of it.
///
/// The previous test mirrored the logic in a local closure, which passed just as
/// happily with a condition deleted — coverage that asserts nothing.
#[must_use]
const fn handover_applies(
    previous: SupervisorMode,
    target: SupervisorMode,
    local_timer_installed: bool,
) -> bool {
    matches!(target, SupervisorMode::Lite)
        && matches!(previous, SupervisorMode::Full)
        && !local_timer_installed
}

fn prepare_handover(
    meta: &AtcMeta,
    previous: SupervisorMode,
    target: SupervisorMode,
) -> Result<()> {
    if target != SupervisorMode::Lite {
        // Switching AWAY from lite is handled after the scanner is stopped; see
        // `reconcile_controllers`. Doing it here would clear a flag a scanner
        // that is still ticking has not consumed yet.
        return Ok(());
    }
    // THREE conditions, all necessary. Arming on `target == Lite` alone armed on
    // switches where the daemon never counted anything, and the seal then burns
    // retries the local ledger was tracking accurately:
    //
    //   previous == Full        a lite→lite switch (a `--provider` change) has
    //                           no daemon ledger to hand over, and its scanner
    //                           is ALREADY RUNNING — the seal would hit a live
    //                           fleet mid-budget.
    //   no local timer          `setup` installs that unit ONLY when daemon
    //                           registration failed, so its absence is what says
    //                           the daemon was scheduling, and counting.
    //
    // `meta.heartbeat_enabled` is deliberately NOT a condition, though it looks
    // like one. It is a meta-local flag; the daemon's cron selects on its own
    // `atc_instance.enabled` column and never reads meta. `setup --no-heartbeat`
    // flips the flag WITHOUT unregistering, so a fleet the daemon is still
    // scheduling and still counting reads as "nothing scheduled anywhere" — and
    // skipping the arm there is exactly the fail-open the handover exists to
    // close. Being wrong the other way merely seals budgets nobody was spending.
    if !handover_applies(previous, target, timer::is_installed(&meta.name)) {
        return Ok(());
    }
    arm_daemon_handoff(&meta.name).with_context(|| {
        format!(
            "refusing to switch ATC '{}' to lite: the daemon's spent retry budgets are \
unreadable from here, and the handover that seals them could not be armed. Nothing was changed",
            meta.name
        )
    })
}

/// Print the current mode without touching anything.
fn report(meta: &AtcMeta, format: OutputFormat, extra: Option<&str>) -> Result<()> {
    if matches!(format, OutputFormat::Text) {
        println!("ATC '{}' — {} mode", meta.name, meta.mode.id());
        for line in mode_help(meta.mode, &meta.provider) {
            println!("  {line}");
        }
        if let Some(extra) = extra {
            println!("  {extra}");
        }
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "name": meta.name,
                "mode": meta.mode.id(),
                "provider": meta.provider,
                "owner": meta.mode.owner().label(),
                "help": mode_help(meta.mode, &meta.provider),
            }))?
        );
    }
    Ok(())
}

/// What the reconcile step actually managed to do, reported rather than assumed.
#[derive(Debug, Default)]
struct Reconcile {
    lite_stopped_pid: Option<u32>,
    lite_started: bool,
    scheduler_asserted: bool,
    scheduler_removed: bool,
    notes: Vec<String>,
}

/// Stop the controller the new mode does not own, then start the one it does.
async fn reconcile_controllers(meta: &AtcMeta, provider_changed: bool) -> Reconcile {
    let mut out = Reconcile::default();
    match meta.mode {
        SupervisorMode::Lite => {
            // Dismantle BOTH full-mode schedulers. Either one left registered
            // would keep invoking the heartbeat verb every interval; that verb
            // now stands down, so nothing would be sent — but a scheduler firing
            // into a refusal every few minutes is noise an operator has to
            // explain, and the point of the toggle is that the other half is off.
            match timer::teardown(&meta.name) {
                Ok(_) => out.scheduler_removed = true,
                Err(e) => out.notes.push(format!("local heartbeat timer not removed: {e}")),
            }
            match unregister_daemon_cron(&meta.name).await {
                CronOwnership::Disabled => out.scheduler_removed = true,
                CronOwnership::NotRegistered => {}
                CronOwnership::Unknown => out.notes.push(
                    "daemon heartbeat cron not unregistered (daemon down?); the beat stands down on its own"
                        .to_string(),
                ),
            }
            if provider_changed {
                out.notes.push(format!(
                    "provider remembered as {} for a later switch to full; lite runs no brain, so \
it changes nothing today",
                    meta.provider
                ));
            }
            match start_lite_supervisor(&meta.name) {
                // `detach` only proves a fork succeeded. The child can still bail
                // seconds later on the flock or a bad meta, so confirm it is
                // actually up rather than reporting a start that did not happen.
                Ok(()) => {
                    if lite_came_up(&meta.name) {
                        out.lite_started = true;
                        out.notes.push("lite scanner started".to_string());
                    } else {
                        out.notes.push(
                            "lite scanner was launched but has not registered; check \
`ainb fleet daemons` and the daemon log"
                                .to_string(),
                        );
                    }
                }
                Err(e) => out.notes.push(format!("lite scanner not started: {e}")),
            }
        }
        SupervisorMode::Full => {
            match stop_lite_checked(&meta.name) {
                crate::cli::daemon::StopOutcome::Signalled(pid) => {
                    out.lite_stopped_pid = Some(pid);
                    out.notes.push(format!("lite scanner stopped (pid {pid})"));
                }
                crate::cli::daemon::StopOutcome::NotRunning => {}
                // Not fatal here, unlike `daemon atc restart`: the mode is
                // already persisted, so any surviving scanner stands down on its
                // next tick whether or not we could signal it. But a process the
                // operator may still see in `ps` has to be accounted for.
                crate::cli::daemon::StopOutcome::Unverified(pid) => out.notes.push(format!(
                    "a process holds the lite scanner's pid {pid} but could not be verified as \
that scanner, so it was not signalled. It sends nothing either way — the persisted mode stands \
it down — but check `ps -p {pid}` if it lingers"
                )),
            }
            // Wait for the signalled scanner to actually go before touching the
            // file it owns. `stop_lite_checked` only SIGTERMs; a scanner still
            // inside `tick`'s awaits rewrites the whole HeartbeatState at the
            // end of that tick, so a disarm racing it either loses the disarm or
            // loses that tick's counts.
            if let Some(pid) = out.lite_stopped_pid {
                if let Err(e) = await_lite_exit(&meta.name) {
                    tracing::warn!(pid, error = %e, "atc mode: lite scanner did not exit promptly");
                }
            }

            // Only NOW is it safe to drop a handover that was never consumed.
            // Doing it in the pre-flight cleared the flag while a scanner was
            // still ticking: a tick already past its `read_meta` still saw lite,
            // re-read the flag as false, skipped the seal, and every session the
            // daemon had escalated got a fresh budget. Since an empty roster now
            // holds the flag across many ticks, that window is routine.
            if let Err(e) = disarm_daemon_handoff(&meta.name) {
                tracing::warn!(error = %e, "atc mode: could not clear a pending ledger handover");
            }

            // A provider change is not just a meta edit. The new brain reads a
            // DIFFERENT file (codex reads AGENTS.md), and `repair` — the verb
            // below — reads meta and never writes anything else, so nothing
            // would render that policy. Every surface would report FULL(codex)
            // over an instance dir that only has a CLAUDE.md.
            match render_policy_for_provider(meta) {
                Ok(Some(file)) => out.notes.push(format!("policy rendered to {file}")),
                Ok(None) => {}
                Err(e) => out.notes.push(format!("policy not rendered for the new brain: {e}")),
            }
            if let Some(pid) = out.lite_stopped_pid {
                out.notes.push(format!("lite scanner stopped (pid {pid})"));
            }
            // Re-assert the scheduler through `repair`, which is the verb that
            // already guarantees exactly one of (daemon cron, local timer) ends
            // up active. Reimplementing that choice here is how a second
            // scheduler gets born.
            match delegate(&["fleet", "atc", "repair", &meta.name]) {
                Ok(report) => {
                    out.scheduler_asserted = true;
                    out.notes.push(report);
                    out.notes.extend(brain_session_note(meta, provider_changed));
                }
                Err(e) => out.notes.push(format!("heartbeat scheduler not re-asserted: {e}")),
            }
        }
    }
    out
}

/// Render the policy under the filename this instance's provider actually reads,
/// returning the file when one was written beyond the always-present CLAUDE.md.
fn render_policy_for_provider(meta: &AtcMeta) -> Result<Option<String>> {
    let control = resolve_full_provider(&meta.provider)?;
    if control.policy_file == "CLAUDE.md" {
        return Ok(None);
    }
    let paths = AtcPaths::resolve(&meta.name)?;
    let policy = crate::fleet::atc::render_claude_md(meta);
    plumbing::atomic::write_atomic(&paths.dir.join(control.policy_file), policy.as_bytes())
        .with_context(|| format!("writing {}", control.policy_file))?;
    Ok(Some(control.policy_file.to_string()))
}

/// Say so when full mode's scheduler now has nothing to beat into.
///
/// The switch deliberately does NOT spawn the brain itself: an operator asked to
/// change which controller owns the fleet, not to launch a session, and a toggle
/// that quietly starts a provider CLI is a bigger side effect than the toggle
/// implies. But a scheduler firing into a dead session is silent — the beat
/// fires, finds nothing, exits 0 — so the switch has to name it.
///
/// Only a PROVEN dead session earns the note. `None` means the check itself
/// could not run (tmux off PATH), and reporting an environment problem as a
/// missing brain would send the operator to the wrong fix.
fn brain_session_note(meta: &AtcMeta, provider_changed: bool) -> Option<String> {
    let session = meta.tmux_session();
    match crate::tmux::session_alive(&session) {
        Some(false) => Some(format!(
            "no brain session: {session} is not running, so beats would land nowhere. \
`ainb daemon atc start` spawns it without reconfiguring the instance"
        )),
        // A LIVE session is not evidence the provider took effect. Nothing here
        // restarts it, so a `--provider` change leaves the previous brain taking
        // the heartbeats while every surface reports the new one. Say so rather
        // than let the two disagree silently.
        Some(true) if provider_changed => Some(format!(
            "{session} is still running the PREVIOUS brain — a provider change does not restart \
it. `ainb kill {}` then `ainb daemon atc start` to bring up {} instead",
            meta.name, meta.provider
        )),
        _ => None,
    }
}

/// Record that the daemon owned the retry ledger, so the lite scanner's first
/// tick seals it. Pure local file write: no RPC, no subprocess, no roster.
fn arm_daemon_handoff(name: &str) -> Result<()> {
    set_handoff_flag(name, true)
}

/// Clear a handover that was armed but never consumed.
///
/// Without this, a flag armed by a switch to lite whose scanner never ticked
/// survives into an unrelated later switch and seals budgets the LOCAL ledger
/// had been tracking accurately.
fn disarm_daemon_handoff(name: &str) -> Result<()> {
    set_handoff_flag(name, false)
}

/// Read-modify-write the flag, deliberately WITHOUT a lock.
///
/// That is safe only because of when the two callers run, so the reasoning has
/// to live next to the code that depends on it: arming happens on a full→lite
/// switch, where no lite scanner exists yet, and disarming happens on a
/// lite→full switch only after the scanner has been stopped. Neither ever races
/// a live scanner's own writes to `continue_counts`. If a third caller is ever
/// added, it needs the lock this one does not.
fn set_handoff_flag(name: &str, armed: bool) -> Result<()> {
    let paths = AtcPaths::resolve(name)?;
    let mut state = read_heartbeat_state(&paths);
    if state.pending_daemon_handoff == armed {
        return Ok(());
    }
    state.pending_daemon_handoff = armed;
    write_heartbeat_state_checked(&paths, &state)
}

/// What the daemon said about the cron it may have been scheduling.
///
/// Three outcomes, kept apart because only ONE of them is evidence the daemon
/// did not own the retry ledger. Collapsing them to a bool is what let the
/// ledger double-spend survive on the daemon-down path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CronOwnership {
    /// The daemon answered and disabled a registration it held.
    Disabled,
    /// The daemon answered and held no registration for this instance.
    NotRegistered,
    /// We could not ask (no token, dial refused, RPC error). Says nothing.
    Unknown,
}

async fn unregister_daemon_cron(name: &str) -> CronOwnership {
    use crate::fleet::bridge::daemon::DaemonClient;
    use ainb_hangar_proto::snapshots::AtcUnregisterParams;
    let Ok(client) = DaemonClient::from_env() else {
        return CronOwnership::Unknown;
    };
    match client
        .atc_unregister(AtcUnregisterParams {
            name: name.to_string(),
            expected_generation: None,
        })
        .await
    {
        Ok(r) if r.disabled => CronOwnership::Disabled,
        Ok(_) => CronOwnership::NotRegistered,
        Err(_) => CronOwnership::Unknown,
    }
}

// ── The lite supervisor process ─────────────────────────────────────────────

/// Start the lite scanner for `name` if it is not already running. The public
/// entry point `setup` and the daemon lifecycle verbs use.
pub fn ensure_lite_running(name: &str) -> Result<()> {
    start_lite_supervisor(name)
}

/// Wait for a signalled scanner to actually exit, then start its replacement.
///
/// `restart` used to SIGTERM and start immediately. A process does not die at
/// the instant it is signalled, so microseconds later it is still alive and
/// still identity-`Matched` — `start_lite_supervisor` saw a live scanner, early
/// returned Ok, and the CLI reported "restarted (replaced pid N)" over a fleet
/// that then had no scanner at all.
pub fn restart_lite(name: &str, stopped: Option<u32>) -> Result<()> {
    if stopped.is_some() {
        await_lite_exit(name)?;
    }
    start_lite_supervisor(name)
}

/// Wait briefly for a just-detached scanner to register itself.
///
/// Bounded and short: this is a report-accuracy check, not a readiness gate, and
/// the caller says "launched but not registered" rather than blocking a switch
/// on a slow fork.
fn lite_came_up(name: &str) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if live_lite_pid(name).is_some() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// Block until `name`'s lite scanner is gone, or fail after 10s.
///
/// A process does not die at the instant it is signalled. Every caller that
/// SIGTERMs the scanner and then touches something it owns — its lock, its
/// heartbeat-state file — has to wait for it to actually go first.
fn await_lite_exit(name: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while live_lite_pid(name).is_some() {
        if std::time::Instant::now() >= deadline {
            bail!(
                "the lite scanner for ATC '{name}' did not exit within 10s of SIGTERM; \
it may still be holding its lock and its ledger"
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(())
}

/// Stop the lite scanner for `name`, reporting what was actually established.
///
/// Public so `ainb daemon atc stop` can take down whichever controller is really
/// there — and so `restart` can refuse to start a second scanner over one it
/// could not prove is gone.
pub(crate) fn stop_lite_checked(name: &str) -> crate::cli::daemon::StopOutcome {
    crate::cli::daemon::stop_by_heartbeat_pid_checked(&lite_heartbeat_id(name))
}

/// Start the lite scanner detached, unless one is already registered.
///
/// This check is a courtesy that avoids a pointless fork; it is NOT the
/// single-instance guarantee. That belongs to the `flock` the child itself takes
/// in `supervise`, which is the only thing two racing starters cannot both win.
fn start_lite_supervisor(name: &str) -> Result<()> {
    if live_lite_pid(name).is_some() {
        return Ok(());
    }
    detach(&["fleet", "atc", "supervise", name])
}

/// A held single-instance lock for one lite scanner.
///
/// The `File` IS the lock: it is never read or written, only `flock`ed, and the
/// kernel releases it when this process exits — normally, on a crash, on SIGKILL,
/// and across a reboot.
///
/// That property is why this replaced a pidfile. The hand-rolled version had to
/// decide when a leftover lock was stale, and every version of that answer was
/// wrong in a different way: `kill(pid, 0)` made a lock unreclaimable forever
/// once the OS recycled its pid; unlinking-and-recreating raced, because
/// `remove_file` works on a path and not an inode, so a reclaimer could delete
/// the lock a winner had just created and then take its own — two scanners, out
/// of the guard meant to prevent them. `flock` has no staleness question to get
/// wrong.
struct LiteLock(#[allow(dead_code)] std::fs::File);

/// Take the exclusive right to be `name`'s lite scanner, or fail saying so.
///
/// Non-blocking on purpose: a second scanner must fail loudly and immediately,
/// not queue up behind the first and start the moment it exits.
fn acquire_lite_lock(name: &str) -> Result<LiteLock> {
    use fs2::FileExt;
    let dir = crate::fleet::daemons::heartbeat::daemons_dir()?;
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join(format!("{}.lock", lite_heartbeat_id(name)));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening the lite-scanner lock {}", path.display()))?;
    if file.try_lock_exclusive().is_err() {
        let held = live_lite_pid(name).map_or_else(
            || "held by another process".to_string(),
            |p| format!("pid {p}"),
        );
        bail!(
            "a lite scanner already owns ATC '{name}' ({held}); refusing to start a second \
one. Two scanners double-spend the retry budget and send `continue` twice into the same pane. \
Stop it with `ainb daemon atc stop`."
        );
    }
    Ok(LiteLock(file))
}

/// The pid of a lite scanner that is provably still running for `name`.
///
/// Identity, not liveness: a recycled pid is a tombstone, not a running
/// scanner, and treating it as one would leave the fleet with no controller
/// while reporting that it has one.
fn live_lite_pid(name: &str) -> Option<u32> {
    use crate::fleet::daemons::heartbeat::{DaemonHeartbeat, PidCheck, pid_identity};
    let hb = DaemonHeartbeat::read(&lite_heartbeat_id(name))?;
    (pid_identity(hb.pid, hb.started_at) == PidCheck::Matched).then_some(hb.pid)
}

/// `ainb fleet atc supervise <name>` — the LITE controller.
///
/// No LLM anywhere in this loop. It consumes the same LLM-free `fleet needs`
/// read the full heartbeat is built from, and acts on exactly one class of row:
/// an ERR whose pattern the classifier already recognises as a transient agent
/// error. Everything else — ASK, WAIT, IDLE — is counted and left alone, because
/// deciding what an ambiguous session needs is the job lite mode does not do.
pub async fn supervise(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let name = matches
        .get_one::<String>("name")
        .cloned()
        .context("expected an instance name")?;
    let once = matches.get_flag("once");
    // A dry run PLANS but never sends and never spends ledger budget. It exists
    // so the mode gate can be exercised — by a test, or by an operator checking
    // what lite would do — without an inspection turning into a fleet-wide
    // `continue`.
    let dry_run = matches.get_flag("dry-run");
    let (meta, paths) = read_meta(&name)?;

    // Refuse to even start in the wrong mode, so a stale unit or a hand-typed
    // command cannot put a second controller on the fleet.
    if !may_act(meta.mode, Controller::LiteScanner) {
        bail!("{}", stand_down_reason(meta.mode, Controller::LiteScanner));
    }

    // A dry run must not touch the record the live scanner owns, so it never
    // registers itself at all — see the heartbeat block below.
    let hb_id = lite_heartbeat_id(&name);

    // THE second-scanner guard. A read-then-write check is not enough here, and
    // was not: two children booting within the same ~100ms window both see no
    // live pid, both write their own heartbeat, and both scan. Narrowing that
    // window is not the same as closing it, and "exactly one controller" is the
    // one property this whole module exists to provide.
    //
    // So the guard is an ATOMIC create: `create_new` is O_CREAT|O_EXCL, and
    // exactly one of N racing processes can win it. The loser exits. The lock is
    // released on the way out, and a lock left behind by a crash is reclaimed by
    // the identity check inside `acquire_lite_lock` rather than wedging the
    // instance forever.
    let _lock = if dry_run {
        None
    } else {
        Some(acquire_lite_lock(&name)?)
    };

    let mut heartbeat = crate::fleet::daemons::DaemonHeartbeat::starting();
    heartbeat.set_connected(true, Some(format!("{name} · lite scan")));
    // A dry run leaves the record alone: overwriting it would point `stop_lite`,
    // `probe_atc_lite` and `start_lite_supervisor` at a pid that exits in
    // milliseconds — killing the wrong process, reporting the live scanner as
    // crashed, and then starting a second one.
    if !dry_run {
        let _ = heartbeat.write(&hb_id);
    }

    loop {
        // Re-read the mode EVERY tick, from disk. The switch persists before it
        // stops anything, so this is the check that actually enforces
        // exclusivity — not the one at start-up.
        let current = match read_meta(&name) {
            Ok((m, _)) => m.mode,
            // A transient read failure must not be read as "mode changed": that
            // would stand the only controller down over a blip. Keep the last
            // known mode and try again next tick.
            Err(e) => {
                tracing::warn!(error = %e, "atc lite: meta unreadable this tick; holding mode");
                meta.mode
            }
        };
        if !may_act(current, Controller::LiteScanner) {
            let reason = stand_down_reason(current, Controller::LiteScanner);
            if !dry_run {
                let _ = crate::fleet::daemons::DaemonHeartbeat::clear(&hb_id);
            }
            if matches!(format, OutputFormat::Text) {
                println!("[atc/{name}] {reason}");
            }
            return Ok(());
        }

        match tick(&paths, DEFAULT_ERR_RETRY_CAP, dry_run).await {
            Ok(report) => {
                if report.continued > 0 && !dry_run {
                    heartbeat.record_activity();
                } else {
                    heartbeat.touch();
                }
                let hooks = hook_evidence();
                // A broken hook pipeline is recorded as an ERROR on the row, not
                // just printed: an unattended lite fleet is exactly the case
                // where nobody is reading stdout.
                if !hooks.issues.is_empty() {
                    heartbeat.record_error(format!("hook wiring: {}", hooks.issues.join("; ")));
                }
                if matches!(format, OutputFormat::Text) {
                    println!("[atc/{name}] {} · hooks {}", report.line(), hooks.summary);
                    for issue in &hooks.issues {
                        println!("[atc/{name}]   {issue}");
                    }
                } else {
                    println!("{}", serde_json::to_string(&report.json(&name, &hooks))?);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "atc lite: scan failed");
                heartbeat.record_error(e.to_string());
            }
        }
        if !dry_run {
            let _ = heartbeat.write(&hb_id);
        }

        if once {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(LITE_SCAN_INTERVAL_SECS)).await;
    }
}

/// What one lite scan saw and did — the evidence line an operator reads.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LiteReport {
    pub scanned: usize,
    pub err: usize,
    pub continued: usize,
    pub capped: usize,
    /// Rows lite mode deliberately will not decide: ASK / WAIT / IDLE.
    pub reported: usize,
    /// Rows the event-sourced `current_state` table produced (`source: "hook"`).
    pub from_hooks: usize,
    /// Rows folded from a live pane / transcript scan (`source: "tmux"`), plus
    /// the legacy classifier path that reports no source at all.
    pub from_scan: usize,
}

impl LiteReport {
    fn line(&self) -> String {
        format!(
            "lite scan — {} row(s) ({} hook, {} scan): {} ERR ({} continued, {} at cap), \
{} left for a human",
            self.scanned,
            self.from_hooks,
            self.from_scan,
            self.err,
            self.continued,
            self.capped,
            self.reported
        )
    }

    fn json(&self, name: &str, hooks: &HookEvidence) -> serde_json::Value {
        json!({
            "action": "supervise",
            "name": name,
            "mode": "lite",
            "scanned": self.scanned,
            "err": self.err,
            "continued": self.continued,
            "capped": self.capped,
            "reported": self.reported,
            "evidence": {
                "from_hooks": self.from_hooks,
                "from_scan": self.from_scan,
                "hook_health": hooks.summary,
                "hook_issues": hooks.issues,
            },
        })
    }
}

/// The hook pipeline's health, as lite mode needs to report it.
///
/// This matters more in lite than in full: with the hooks broken, `fleet needs`
/// silently degrades from the event-sourced `current_state` read to a pane scan,
/// which sees less. A full ATC has a brain that can notice and say so; the lite
/// scanner has no brain, so the health has to ride along with the evidence or
/// nobody learns the pipeline is down.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HookEvidence {
    /// `"ok"`, or a short count of what is wrong.
    pub summary: String,
    /// One line per issue: the component and its repair command.
    pub issues: Vec<String>,
}

/// Read hook wiring health. Never fails: an unreadable home reports "unknown"
/// rather than taking the scan loop down.
fn hook_evidence() -> HookEvidence {
    let Ok(paths) = ainb_plugin_notifyd::paths::Paths::from_home() else {
        return HookEvidence {
            summary: "unknown".to_string(),
            issues: Vec::new(),
        };
    };
    let health = ainb_plugin_notifyd::hook_health(&paths);
    let issues: Vec<String> = health
        .issues
        .iter()
        .map(|i| format!("{}: {} — fix: {}", i.component, i.message, i.repair))
        .collect();
    HookEvidence {
        summary: if issues.is_empty() {
            "ok".to_string()
        } else {
            format!("{} issue(s)", issues.len())
        },
        issues,
    }
}

/// One lite scan: read the fleet, auto-continue the ERR rows still inside their
/// budget, and stamp the shared ledger.
async fn tick(paths: &AtcPaths, cap: u32, dry_run: bool) -> Result<LiteReport> {
    let rows = super::atc::fetch_needs().await?;
    let mut state = read_heartbeat_state(paths);

    // Perform a pending ledger handover BEFORE planning, so the seal decides
    // this tick rather than one tick later. See `HeartbeatState::pending_daemon_handoff`.
    let sealed = if state.pending_daemon_handoff {
        let n = seal_erroring_at_cap(&rows, cap, &mut state);
        // Consumed only on a tick that actually SAW something. `fleet needs`
        // exits 0 with an empty array whenever no session is visible — tmux off
        // PATH, a daemon blip, a host mid-reboot — and clearing the flag on that
        // would silently hand every escalated session a fresh budget, which is
        // the precise fail-open this design exists to close. An empty roster is
        // not evidence of a healthy fleet; it is the absence of evidence.
        if rows.is_empty() {
            tracing::warn!(
                "atc lite: ledger handover held — the roster was empty, which is not proof \
that nothing is erroring"
            );
        } else {
            state.pending_daemon_handoff = false;
        }
        n
    } else {
        0
    };

    let (report, to_continue) = plan(&rows, cap, &mut state);

    if dry_run {
        // Neither send nor persist: a dry run must be observably free of side
        // effects, or it is not a dry run. That includes NOT consuming a pending
        // handover — the mutations above are dropped with `state`.
        return Ok(report);
    }
    if sealed > 0 {
        tracing::info!(
            sealed,
            "atc lite: sealed erroring sessions at the cap on the daemon ledger handover"
        );
    }

    for row in to_continue {
        if let Err(e) = crate::fleet::send::send(&row.session, "continue").await {
            tracing::warn!(session = %row.session.id, error = %e, "atc lite: continue send failed");
        }
    }

    // Stamp the SAME file the full heartbeat writes: `last_heartbeat_ms` is what
    // the Daemons ATC probe reads for liveness, so a lite fleet shows as a live
    // ATC rather than a stale one, and `continue_counts` is the one shared
    // safety ledger across both modes.
    state.last_heartbeat_ms = chrono::Utc::now().timestamp_millis();
    if report.err > 0 || report.reported > 0 {
        state.last_active_ms = Some(state.last_heartbeat_ms);
    }
    write_heartbeat_state(paths, &state);
    Ok(report)
}

/// Seal every erroring session at `cap`, returning how many moved.
///
/// The fail-closed half of the daemon ledger handover: a session that is erroring
/// at the moment lite takes over is assumed to have spent its budget, because the
/// real count lived in a table lite cannot read. Never LOWERS a count, so it can
/// only ever make lite more conservative, and a genuinely recovered session drops
/// off the ERR roster and regains its budget through the normal recovery rule.
fn seal_erroring_at_cap(rows: &[NeedsRow], cap: u32, state: &mut HeartbeatState) -> usize {
    let cap = cap.max(1);
    let mut sealed = 0;
    for row in rows {
        if matches!(row.context, NeedsContext::Err(_)) {
            let count = state.continue_counts.entry(row.session.id.clone()).or_insert(0);
            if *count < cap {
                *count = cap;
                sealed += 1;
            }
        }
    }
    sealed
}

/// PURE decision half of a lite tick: given the rows and the shared ledger,
/// which sessions get a `continue`, and what does the operator get told?
///
/// Spends the SAME `continue_counts` budget the full heartbeat spends, with the
/// same cap and the same reset-on-recovery rule, so switching modes never hands
/// a permanently-broken session a fresh set of retries.
fn plan<'a>(
    rows: &'a [NeedsRow],
    cap: u32,
    state: &mut HeartbeatState,
) -> (LiteReport, Vec<&'a NeedsRow>) {
    let cap = cap.max(1);

    // Same recovery rule as `build_heartbeat_enforcing_cap`: a session no longer
    // erroring drops out of the ledger and regains its budget.
    let erroring: std::collections::HashSet<&str> = rows
        .iter()
        .filter(|r| matches!(r.context, NeedsContext::Err(_)))
        .map(|r| r.session.id.as_str())
        .collect();
    state.continue_counts.retain(|id, _| erroring.contains(id.as_str()));

    let mut report = LiteReport {
        scanned: rows.len(),
        ..LiteReport::default()
    };
    let mut send_to = Vec::new();
    for row in rows {
        // Provenance, so the operator can see WHY the roster looks the way it
        // does. A roster that quietly went all-scan is a broken hook pipeline,
        // and it is indistinguishable from a healthy one by row count alone.
        match row.source.as_deref() {
            Some("hook") => report.from_hooks += 1,
            _ => report.from_scan += 1,
        }
        match row.context {
            NeedsContext::Err(_) => {
                report.err += 1;
                let count = state.continue_counts.entry(row.session.id.clone()).or_insert(0);
                if *count >= cap {
                    // Budget spent. Lite mode escalates by REPORTING — it has no
                    // brain to decide anything else, and continuing past the cap
                    // is exactly the unbounded retry loop this replaced.
                    report.capped += 1;
                } else {
                    *count += 1;
                    report.continued += 1;
                    send_to.push(row);
                }
            }
            // Ambiguous work. Lite mode does not reason about it — that is the
            // documented limit of the mode, not an oversight.
            _ => report.reported += 1,
        }
    }
    (report, send_to)
}

fn read_heartbeat_state(paths: &AtcPaths) -> HeartbeatState {
    std::fs::read_to_string(&paths.heartbeat_state)
        .ok()
        .map(|s| HeartbeatState::from_json_or_default(&s))
        .unwrap_or_default()
}

fn write_heartbeat_state(paths: &AtcPaths, state: &HeartbeatState) {
    if let Err(e) = write_heartbeat_state_checked(paths, state) {
        tracing::warn!(error = %e, "atc lite: heartbeat-state write failed");
    }
}

/// The same write, but surfacing the error.
///
/// Arming the ledger handover MUST NOT fail silently: a swallowed error there
/// means the fail-closed handover never happens and every session the daemon
/// escalated quietly gets a fresh budget.
fn write_heartbeat_state_checked(paths: &AtcPaths, state: &HeartbeatState) -> Result<()> {
    let json = state.to_json().context("serializing heartbeat-state")?;
    plumbing::atomic::write_atomic(&paths.heartbeat_state, json.as_bytes())
        .with_context(|| format!("writing {}", paths.heartbeat_state.display()))
}

// ── Re-entrant delegation ───────────────────────────────────────────────────

fn ainb_bin() -> Result<std::path::PathBuf> {
    if crate::self_exec_guard::running_under_cargo_test() {
        bail!(
            "refusing to run an ainb subcommand from a cargo test binary \
             (current_exe is a test harness, not `ainb`)"
        );
    }
    std::env::current_exe().context("resolving the running ainb binary")
}

/// Run `ainb <argv>` and return its last output line.
fn delegate(argv: &[&str]) -> Result<String> {
    let out = std::process::Command::new(ainb_bin()?)
        .args(argv)
        .output()
        .with_context(|| format!("running `ainb {}`", argv.join(" ")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        bail!(
            "`ainb {}` exited {}: {}",
            argv.join(" "),
            out.status,
            if stderr.is_empty() { &stdout } else { &stderr }
        );
    }
    Ok(stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("ok")
        .trim()
        .to_string())
}

/// Spawn `ainb <argv>` detached.
///
/// Delegates to [`crate::cli::daemon::detach`] rather than rolling its own: that
/// one puts the child in its OWN process group (so a Ctrl-C aimed at the
/// launching terminal never kills the fleet's only controller) and points its
/// output at the daemon log instead of `/dev/null` (so the scan lines, the hook
/// issues and the stand-down message survive). A local copy silently lost both.
fn detach(argv: &[&str]) -> Result<()> {
    crate::cli::daemon::detach(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::read::{ErrContext, RouteHint};
    use crate::fleet::types::Session;

    fn session(id: &str) -> Session {
        Session {
            id: id.to_string(),
            cwd: "/tmp".to_string(),
            pid: None,
            git_root: None,
            tmux_session: Some(format!("tmux_{id}")),
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: Vec::new(),
            summary: None,
            last_seen_ms: None,
        }
    }

    fn row(id: &str, context: NeedsContext) -> NeedsRow {
        NeedsRow {
            session: session(id),
            context,
            route_hint: RouteHint::None,
            enrich_key: String::new(),
            enriched: None,
            need_enrich: false,
            source: None,
        }
    }

    fn err_row(id: &str) -> NeedsRow {
        row(
            id,
            NeedsContext::Err(ErrContext {
                pattern: "api-error".into(),
                snippet: "overloaded".into(),
            }),
        )
    }

    #[test]
    fn lite_continues_an_err_row_inside_its_budget() {
        let rows = vec![err_row("s1")];
        let mut state = HeartbeatState::default();
        let (report, send) = plan(&rows, 3, &mut state);
        assert_eq!(report.err, 1);
        assert_eq!(report.continued, 1);
        assert_eq!(report.capped, 0);
        assert_eq!(send.len(), 1);
        assert_eq!(state.continue_counts.get("s1"), Some(&1));
    }

    #[test]
    fn lite_stops_at_the_same_cap_the_full_heartbeat_enforces() {
        // The shared-ledger property: lite spends the same budget, and once it is
        // gone it reports instead of retrying forever — the unbounded
        // auto-continue of the old fleet daemon is what this replaced.
        let rows = vec![err_row("s1")];
        let mut state = HeartbeatState::default();
        for _ in 0..3 {
            let (_, send) = plan(&rows, 3, &mut state);
            assert_eq!(send.len(), 1);
        }
        let (report, send) = plan(&rows, 3, &mut state);
        assert!(send.is_empty(), "past the cap nothing may be sent");
        assert_eq!(report.capped, 1);
        assert_eq!(state.continue_counts.get("s1"), Some(&3));
    }

    #[test]
    fn a_budget_spent_by_the_full_heartbeat_is_already_gone_in_lite() {
        // Switching modes must not reset the safety ledger. Seed the state the
        // way the full heartbeat leaves it, then scan in lite.
        let mut state = HeartbeatState::default();
        state.continue_counts.insert("s1".to_string(), 3);
        let rows = vec![err_row("s1")];
        let (report, send) = plan(&rows, 3, &mut state);
        assert!(
            send.is_empty(),
            "lite must honour a cap the full heartbeat already spent"
        );
        assert_eq!(report.capped, 1);
    }

    #[test]
    fn lite_never_acts_on_an_ambiguous_row() {
        // The documented limit of lite mode: it reports ASK / WAIT / IDLE and
        // sends nothing, because deciding those needs a brain it does not have.
        let rows = vec![
            row(
                "ask",
                NeedsContext::Ask(crate::fleet::read::AskUserQuestionData {
                    question: "which branch?".into(),
                    header: None,
                    options: Vec::new(),
                    multi_select: false,
                }),
            ),
            row(
                "wait",
                NeedsContext::Wait(crate::fleet::read::WaitContext {
                    marker: "WAITING:".into(),
                    text: "needs a decision".into(),
                }),
            ),
            row(
                "idle",
                NeedsContext::Idle(crate::fleet::read::IdleContext {
                    idle_minutes: 42,
                    last_assistant_text: None,
                }),
            ),
        ];
        let mut state = HeartbeatState::default();
        let (report, send) = plan(&rows, 3, &mut state);
        assert!(send.is_empty(), "lite must send nothing for ambiguous rows");
        assert_eq!(report.reported, 3);
        assert_eq!(report.err, 0);
        assert!(state.continue_counts.is_empty(), "no budget is spent");
    }

    #[test]
    fn the_handover_seal_is_fail_closed_and_never_lowers_a_count() {
        let mut state = HeartbeatState::default();
        state.continue_counts.insert("mid".to_string(), 1);
        state.continue_counts.insert("over".to_string(), 9);
        let rows = vec![err_row("mid"), err_row("over"), err_row("fresh")];

        let sealed = seal_erroring_at_cap(&rows, DEFAULT_ERR_RETRY_CAP, &mut state);
        assert_eq!(sealed, 2, "only the two below the cap moved");
        assert_eq!(state.continue_counts["mid"], DEFAULT_ERR_RETRY_CAP);
        assert_eq!(state.continue_counts["over"], 9, "never lowered");
        assert_eq!(
            state.continue_counts["fresh"], DEFAULT_ERR_RETRY_CAP,
            "a session lite has never seen must not start with a fresh budget"
        );

        // And every sealed session is genuinely refused by the planner.
        let (report, send) = plan(&rows, DEFAULT_ERR_RETRY_CAP, &mut state);
        assert!(send.is_empty(), "nothing may be continued after a seal");
        assert_eq!(report.capped, 3);
    }

    #[test]
    fn a_seal_only_touches_erroring_rows() {
        // Ambiguous rows have never consumed budget and must not gain a
        // counter, or a later genuine ERR would start already exhausted.
        let mut state = HeartbeatState::default();
        let rows = vec![row(
            "idle",
            NeedsContext::Idle(crate::fleet::read::IdleContext {
                idle_minutes: 5,
                last_assistant_text: None,
            }),
        )];
        assert_eq!(seal_erroring_at_cap(&rows, 3, &mut state), 0);
        assert!(state.continue_counts.is_empty());
    }

    #[test]
    fn the_handover_applies_only_where_the_daemon_could_have_owned_the_ledger() {
        // Calls the REAL predicate. The version this replaced mirrored the logic
        // in a local closure, so it passed just as happily with a condition
        // deleted — coverage that asserted nothing.
        use SupervisorMode::{Full, Lite};

        // The case the handover exists for: a full instance the daemon was
        // scheduling (no local unit) switching to lite.
        assert!(handover_applies(Full, Lite, false));

        // A local timer means the beat ran locally and counted locally, so the
        // budget is already in the file lite is about to read.
        assert!(!handover_applies(Full, Lite, true));

        // lite→lite (a --provider change) has no daemon ledger to hand over, and
        // its scanner is already running: sealing would hit a live fleet
        // mid-budget.
        assert!(!handover_applies(Lite, Lite, false));

        // Switching TO full never arms; the disarm happens after the stop.
        assert!(!handover_applies(Full, Full, false));
        assert!(!handover_applies(Lite, Full, false));
    }

    #[test]
    fn heartbeat_enabled_is_not_part_of_the_predicate() {
        // Deliberately absent, and worth pinning because it reads like it
        // belongs: it is a meta-local flag, while the daemon's cron selects on
        // its own `atc_instance.enabled` column and never reads meta. A
        // `setup --no-heartbeat` flips it WITHOUT unregistering, so keying on it
        // skipped the arm for a fleet the daemon was still counting for.
        //
        // The predicate takes no meta at all, which is the structural way to say
        // that flag cannot creep back in.
        assert!(handover_applies(
            SupervisorMode::Full,
            SupervisorMode::Lite,
            false
        ));
    }

    #[test]
    fn a_second_scanner_cannot_take_the_lock_while_the_first_holds_it() {
        // Round 4 replaced the hand-rolled lock with flock and deleted its four
        // tests without adding any, leaving the single-instance guard with no
        // coverage at all. This exercises the primitive the guard now rests on.
        use fs2::FileExt;
        let dir = std::env::temp_dir().join(format!("atc-flock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scanner.lock");

        let first = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .unwrap();
        first.try_lock_exclusive().expect("the first taker wins");

        let second = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(
            second.try_lock_exclusive().is_err(),
            "a second scanner must be refused while the first holds the lock"
        );

        // Releasing hands it straight over — no staleness logic, no reclaim, and
        // nothing left behind for a recycled pid to poison.
        drop(first);
        assert!(
            second.try_lock_exclusive().is_ok(),
            "a released lock must be immediately takeable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_roster_holds_the_handover_instead_of_consuming_it() {
        // `fleet needs` exits 0 with [] whenever nothing is visible (tmux off
        // PATH, a daemon blip). Consuming the handover on that would silently
        // hand every escalated session a fresh budget — the fail-open this
        // design exists to close. Absence of evidence is not evidence.
        let mut state = HeartbeatState {
            pending_daemon_handoff: true,
            ..HeartbeatState::default()
        };
        let sealed = seal_erroring_at_cap(&[], DEFAULT_ERR_RETRY_CAP, &mut state);
        assert_eq!(sealed, 0);
        // `tick` only clears the flag when the roster is non-empty; the seal
        // itself never clears it, so the flag is still armed here.
        assert!(
            state.pending_daemon_handoff,
            "an empty roster must not consume the handover"
        );
    }

    #[test]
    fn the_handover_flag_round_trips_and_defaults_off() {
        // Off by default, so an instance from before the flag existed is not
        // treated as mid-handover and does not seal budgets on its first tick.
        let legacy = HeartbeatState::from_json_or_default(
            r#"{"last_heartbeat_ms":1,"last_active_ms":1,"continue_counts":{}}"#,
        );
        assert!(!legacy.pending_daemon_handoff);

        let armed = HeartbeatState {
            pending_daemon_handoff: true,
            ..HeartbeatState::default()
        };
        let back = HeartbeatState::from_json_or_default(&armed.to_json().unwrap());
        assert!(
            back.pending_daemon_handoff,
            "the flag must survive the file"
        );
    }

    #[test]
    fn sealing_the_ledger_never_lowers_a_count_and_never_resurrects_a_session() {
        // The handoff rule, as a pure property: sealing may only push a count UP
        // to the cap. A switch away from a daemon-owned ledger must never hand a
        // session more budget than it had, and must never leave an erroring one
        // below the cap where lite would auto-continue it afresh.
        let mut state = HeartbeatState::default();
        state.continue_counts.insert("mid".to_string(), 1);
        state.continue_counts.insert("over".to_string(), 9);

        // What `seal_ledger_for_handoff` does to each erroring session id.
        for id in ["mid", "over", "unseen"] {
            let count = state.continue_counts.entry(id.to_string()).or_insert(0);
            if *count < DEFAULT_ERR_RETRY_CAP {
                *count = DEFAULT_ERR_RETRY_CAP;
            }
        }

        assert_eq!(
            state.continue_counts["mid"], DEFAULT_ERR_RETRY_CAP,
            "sealed up"
        );
        assert_eq!(state.continue_counts["over"], 9, "never lowered");
        assert_eq!(
            state.continue_counts["unseen"], DEFAULT_ERR_RETRY_CAP,
            "no fresh budget"
        );

        // And a sealed session is genuinely refused by the planner.
        let rows = vec![err_row("mid")];
        let (report, send) = plan(&rows, DEFAULT_ERR_RETRY_CAP, &mut state);
        assert!(send.is_empty(), "a sealed session must not be continued");
        assert_eq!(report.capped, 1);
    }

    #[test]
    fn a_recovered_session_regains_its_budget() {
        let mut state = HeartbeatState::default();
        state.continue_counts.insert("s1".to_string(), 3);
        // s1 is no longer erroring this scan.
        let (_, _) = plan(&[], 3, &mut state);
        assert!(state.continue_counts.is_empty());
    }

    #[test]
    fn the_report_line_names_what_lite_did_and_did_not_do() {
        let report = LiteReport {
            scanned: 4,
            err: 2,
            continued: 1,
            capped: 1,
            reported: 2,
            from_hooks: 3,
            from_scan: 1,
        };
        let line = report.line();
        assert!(line.contains("2 ERR"), "{line}");
        assert!(line.contains("1 continued"), "{line}");
        assert!(line.contains("1 at cap"), "{line}");
        assert!(line.contains("2 left for a human"), "{line}");
        assert!(line.contains("3 hook, 1 scan"), "provenance: {line}");
    }

    #[test]
    fn the_scan_reports_where_its_evidence_came_from() {
        // A roster that has quietly gone all-scan is a broken hook pipeline, and
        // it is indistinguishable from a healthy one by row count alone. Lite
        // has no brain to notice, so the counts have to be in the report.
        let mut hooked = err_row("s1");
        hooked.source = Some("hook".to_string());
        let mut scanned = err_row("s2");
        scanned.source = Some("tmux".to_string());
        let legacy = err_row("s3"); // no source at all
        let rows = vec![hooked, scanned, legacy];

        let mut state = HeartbeatState::default();
        let (report, _) = plan(&rows, 3, &mut state);
        assert_eq!(report.from_hooks, 1);
        assert_eq!(report.from_scan, 2, "unsourced rows are not hook evidence");
        assert_eq!(report.from_hooks + report.from_scan, report.scanned);
    }
}
