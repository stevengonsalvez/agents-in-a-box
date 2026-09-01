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

    // 1. Persist first — this alone stands the outgoing controller down, because
    //    both controllers re-read the mode before every send.
    meta.mode = target;
    meta.provider = provider.clone();
    write_meta(&paths, &meta)?;

    // 2/3/4. Reconcile the machinery: stop the outgoing scheduler, start the
    //        incoming one. Best-effort and reported, never fatal — the safety
    //        property is already held by the write above.
    let reconcile = if unchanged {
        Reconcile::default()
    } else {
        reconcile_controllers(&meta).await
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
async fn reconcile_controllers(meta: &AtcMeta) -> Reconcile {
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
            if unregister_daemon_cron(&meta.name).await {
                out.scheduler_removed = true;
            } else {
                out.notes.push(
                    "daemon heartbeat cron not unregistered (daemon down?); the beat stands down on its own"
                        .to_string(),
                );
            }
            match start_lite_supervisor(&meta.name) {
                Ok(()) => {
                    out.lite_started = true;
                    out.notes.push("lite scanner started".to_string());
                }
                Err(e) => out.notes.push(format!("lite scanner not started: {e}")),
            }
        }
        SupervisorMode::Full => {
            out.lite_stopped_pid = stop_lite_supervisor(&meta.name);
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
                    out.notes.extend(brain_session_note(meta));
                }
                Err(e) => out.notes.push(format!("heartbeat scheduler not re-asserted: {e}")),
            }
        }
    }
    out
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
fn brain_session_note(meta: &AtcMeta) -> Option<String> {
    let session = meta.tmux_session();
    (crate::tmux::session_alive(&session) == Some(false)).then(|| {
        format!(
            "no brain session: {session} is not running, so beats would land nowhere. \
`ainb daemon atc start` spawns it without reconfiguring the instance"
        )
    })
}

async fn unregister_daemon_cron(name: &str) -> bool {
    use crate::fleet::bridge::daemon::DaemonClient;
    use ainb_hangar_proto::snapshots::AtcUnregisterParams;
    let Ok(client) = DaemonClient::from_env() else {
        return false;
    };
    client
        .atc_unregister(AtcUnregisterParams {
            name: name.to_string(),
            expected_generation: None,
        })
        .await
        .is_ok_and(|r| r.disabled)
}

// ── The lite supervisor process ─────────────────────────────────────────────

/// Start the lite scanner for `name` if it is not already running. The public
/// entry point `setup` and the daemon lifecycle verbs use.
pub fn ensure_lite_running(name: &str) -> Result<()> {
    start_lite_supervisor(name)
}

/// Stop the lite scanner for `name`, returning the pid it signalled. Public so
/// `ainb daemon atc stop` can take down whichever controller is actually there.
pub fn stop_lite(name: &str) -> Option<u32> {
    stop_lite_supervisor(name)
}

/// Start the lite scanner detached. Idempotent-ish: a live recorded pid means one
/// is already running, and a second would be the double-controller this whole
/// design exists to prevent.
fn start_lite_supervisor(name: &str) -> Result<()> {
    use crate::fleet::daemons::heartbeat::DaemonHeartbeat;
    if let Some(hb) = DaemonHeartbeat::read(&lite_heartbeat_id(name)) {
        if crate::fleet::daemons::is_pid_alive(hb.pid) {
            return Ok(());
        }
    }
    detach(&["fleet", "atc", "supervise", name])
}

/// SIGTERM the lite scanner's own recorded pid, if it is still alive.
fn stop_lite_supervisor(name: &str) -> Option<u32> {
    use crate::fleet::daemons::heartbeat::DaemonHeartbeat;
    let pid = DaemonHeartbeat::read(&lite_heartbeat_id(name))?.pid;
    let target = nix::unistd::Pid::from_raw(i32::try_from(pid).ok()?);
    nix::sys::signal::kill(target, nix::sys::signal::Signal::SIGTERM).ok()?;
    Some(pid)
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

    let mut heartbeat = crate::fleet::daemons::DaemonHeartbeat::starting();
    heartbeat.set_connected(true, Some(format!("{name} · lite scan")));
    let hb_id = lite_heartbeat_id(&name);
    let _ = heartbeat.write(&hb_id);

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
            let _ = crate::fleet::daemons::DaemonHeartbeat::clear(&hb_id);
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
        let _ = heartbeat.write(&hb_id);

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
    let (report, to_continue) = plan(&rows, cap, &mut state);

    if dry_run {
        // Neither send nor persist: a dry run must be observably free of side
        // effects, or it is not a dry run.
        return Ok(report);
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
    match state.to_json() {
        Ok(json) => {
            if let Err(e) = plumbing::atomic::write_atomic(&paths.heartbeat_state, json.as_bytes())
            {
                tracing::warn!(error = %e, "atc lite: heartbeat-state write failed");
            }
        }
        Err(e) => tracing::warn!(error = %e, "atc lite: heartbeat-state serialize failed"),
    }
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

/// Spawn `ainb <argv>` detached, so it outlives the invoking command.
fn detach(argv: &[&str]) -> Result<()> {
    use std::process::Stdio;
    std::process::Command::new(ainb_bin()?)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawning `ainb {}`", argv.join(" ")))?;
    Ok(())
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
