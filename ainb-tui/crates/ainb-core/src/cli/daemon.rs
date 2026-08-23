// ABOUTME: `ainb daemon <kind> <start|stop|restart>` — one uniform lifecycle
// surface over every daemon in the Daemons view.
//
// Before this, each daemon had its own bespoke verbs (or none): ATC had
// `setup`/`teardown`/`repair`, notifyd had `restart`, the MCP pool had only a
// foreground `daemon` plus `stop`, and the fleet daemon and phone bridge had no
// way to be restarted at all. The Daemons screen needs ONE verb set it can put
// behind every row, and the operator needs the same verbs from a terminal.
//
// This module is that surface. It does not reimplement any lifecycle: each
// action delegates to the machinery that already owns it — in-process where a
// function exists, and as a child `ainb …` invocation where the existing
// subcommand is the only implementation. The delegation is deliberate: the TUI
// shells this command and shows its stderr verbatim in the row's error view, so
// whatever the underlying verb reports is what the user reads.

use anyhow::{Context, Result, bail};
use clap::ArgMatches;

use crate::fleet::daemons::probe::DaemonKind;

/// The three verbs every daemon row offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
}

impl Action {
    /// Stable CLI spelling.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }

    /// Parse a CLI verb.
    #[must_use]
    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            _ => None,
        }
    }

    /// Every verb, in menu order.
    pub const ALL: [Self; 3] = [Self::Start, Self::Restart, Self::Stop];
}

/// Every controllable daemon, in the order the Daemons table lists them.
pub const CONTROLLABLE: [DaemonKind; 8] = [
    DaemonKind::Bridge,
    DaemonKind::Notifyd,
    DaemonKind::ApproveBroker,
    DaemonKind::Atc,
    DaemonKind::FleetDaemon,
    DaemonKind::McpPool,
    DaemonKind::HangarDaemon,
    DaemonKind::HeadroomProxy,
];

/// Resolve a daemon by its stable CLI id.
#[must_use]
pub fn kind_from_id(id: &str) -> Option<DaemonKind> {
    CONTROLLABLE.into_iter().find(|k| k.id() == id)
}

/// `ainb daemon …` entry point.
pub async fn execute(matches: &ArgMatches, _format: crate::cli::OutputFormat) -> Result<()> {
    let Some((kind_id, sub)) = matches.subcommand() else {
        bail!("expected a daemon name — try `ainb daemon --help`")
    };
    if kind_id == "list" {
        for kind in CONTROLLABLE {
            println!("{:<16} {}", kind.id(), kind.display_name());
        }
        return Ok(());
    }
    let kind = kind_from_id(kind_id).with_context(|| format!("unknown daemon: {kind_id}"))?;
    let Some((verb, _)) = sub.subcommand() else {
        bail!("expected start, stop, or restart — try `ainb daemon {kind_id} --help`")
    };
    let action = Action::from_id(verb).with_context(|| format!("unknown action: {verb}"))?;
    let report = control(kind, action).await?;
    println!("{report}");
    Ok(())
}

/// Perform one lifecycle action and return the one-line report.
///
/// Errors carry the underlying failure verbatim — the TUI shows them in the
/// row's error view, so a vague message here is a vague message on screen.
pub async fn control(kind: DaemonKind, action: Action) -> Result<String> {
    match kind {
        DaemonKind::McpPool => mcp_pool(action),
        DaemonKind::HeadroomProxy => headroom(action).await,
        DaemonKind::HangarDaemon => delegate(&["hangar", "daemon", action.id()]),
        DaemonKind::Notifyd | DaemonKind::ApproveBroker => notifyd(kind, action),
        DaemonKind::Atc => atc(action),
        DaemonKind::Bridge => bridge(action),
        DaemonKind::FleetDaemon => fleet_daemon(action),
    }
}

// ── MCP pool ────────────────────────────────────────────────────────────────

fn mcp_pool(action: Action) -> Result<String> {
    use crate::mcp_pool::client;
    match action {
        Action::Start => {
            client::ensure_daemon().context("start the MCP pool")?;
            Ok("mcp pool started".to_string())
        }
        Action::Stop => {
            if !client::daemon_alive() {
                return Ok("mcp pool was not running".to_string());
            }
            client::daemon_stop().context("stop the MCP pool")?;
            Ok("mcp pool stopped".to_string())
        }
        Action::Restart => {
            client::restart_daemon().context("restart the MCP pool")?;
            Ok("mcp pool restarted".to_string())
        }
    }
}

// ── Headroom proxy ──────────────────────────────────────────────────────────

async fn headroom(action: Action) -> Result<String> {
    match action {
        Action::Start => {
            crate::headroom::ensure_proxy_running()
                .await
                .context("start the Headroom proxy")?;
            Ok("headroom proxy started".to_string())
        }
        Action::Stop => Ok(if crate::headroom::stop() {
            "headroom proxy stopped".to_string()
        } else {
            // No pid file means we did not start it. Killing a proxy the user
            // runs themselves is not ours to do.
            "no ainb-managed headroom proxy to stop".to_string()
        }),
        Action::Restart => {
            crate::headroom::stop();
            crate::headroom::ensure_proxy_running()
                .await
                .context("restart the Headroom proxy")?;
            Ok("headroom proxy restarted".to_string())
        }
    }
}

// ── notifyd (and the approve broker it serves) ──────────────────────────────

fn notifyd(kind: DaemonKind, action: Action) -> Result<String> {
    // approve.sock is served on notifyd's runtime, so the broker's lifecycle IS
    // notifyd's. Say so in the report rather than pretending it has its own.
    let note = if kind == DaemonKind::ApproveBroker {
        " (served by notifyd)"
    } else {
        ""
    };
    let verb = match action {
        // `notifyd restart` stops, reaps, and respawns — the documented
        // resume/repair command, and a correct bring-up from stopped.
        Action::Start | Action::Restart => "restart",
        Action::Stop => "stop",
    };
    delegate(&["notifyd", verb]).map(|out| format!("{out}{note}"))
}

// ── ATC ─────────────────────────────────────────────────────────────────────

/// The ATC instance the Daemons row represents.
///
/// The view surfaces ATC as one daemon, so a control action needs one instance.
/// A single provisioned instance is unambiguous; several is not, and guessing
/// which one to tear down would be the wrong kind of helpful.
fn atc_instance() -> Result<String> {
    let root = crate::fleet::plumbing::paths::ainb_home()?.join("atc");
    let mut names = crate::fleet::atc::paths::list_instance_names_in(&root);
    match names.len() {
        0 => bail!("no ATC instance provisioned — run `ainb fleet atc setup <name>` first"),
        1 => Ok(names.remove(0)),
        _ => bail!(
            "{} ATC instances provisioned ({}) — act on one by name with `ainb fleet atc`",
            names.len(),
            names.join(", ")
        ),
    }
}

fn atc(action: Action) -> Result<String> {
    let name = atc_instance()?;
    match action {
        // `repair` re-asserts the OS timer and the daemon registration, which is
        // exactly what starting a stopped ATC means.
        Action::Start | Action::Restart => delegate(&["fleet", "atc", "repair", &name]),
        // No confirmation flag: `fleet atc teardown` takes only <name> and
        // --purge. Passing --yes made clap reject the whole invocation, so
        // `stop` failed every time.
        Action::Stop => delegate(&["fleet", "atc", "teardown", &name]),
    }
}

// ── Phone bridge ────────────────────────────────────────────────────────────

fn bridge(action: Action) -> Result<String> {
    // The bridge runs under the OS supervisor (launchd/systemd), so its
    // lifecycle is the unit's: installing it starts it, removing it stops it.
    match action {
        Action::Start => delegate(&["fleet", "bridge", "install"]),
        Action::Stop => delegate(&["fleet", "bridge", "uninstall"]),
        Action::Restart => {
            delegate(&["fleet", "bridge", "uninstall"])?;
            delegate(&["fleet", "bridge", "install"])
        }
    }
}

// ── Fleet daemon ────────────────────────────────────────────────────────────

fn fleet_daemon(action: Action) -> Result<String> {
    match action {
        Action::Start => {
            detach(&["fleet", "daemon"])?;
            Ok("fleet daemon started".to_string())
        }
        Action::Stop => match stop_by_heartbeat_pid("fleet-daemon") {
            Some(pid) => Ok(format!("fleet daemon stopped (pid {pid})")),
            None => Ok("fleet daemon was not running".to_string()),
        },
        Action::Restart => {
            let stopped = stop_by_heartbeat_pid("fleet-daemon");
            detach(&["fleet", "daemon"])?;
            Ok(match stopped {
                Some(pid) => format!("fleet daemon restarted (replaced pid {pid})"),
                None => "fleet daemon started".to_string(),
            })
        }
    }
}

/// SIGTERM the pid a daemon's own heartbeat recorded, if it is still alive.
///
/// The heartbeat is the daemon's own claim about which process it is, which is
/// a better answer than matching on a process name — that would happily kill
/// somebody else's `ainb fleet daemon` in another checkout.
fn stop_by_heartbeat_pid(name: &str) -> Option<u32> {
    use crate::fleet::daemons::heartbeat::DaemonHeartbeat;
    let pid = DaemonHeartbeat::read(name)?.pid;
    let target = nix::unistd::Pid::from_raw(i32::try_from(pid).ok()?);
    nix::sys::signal::kill(target, nix::sys::signal::Signal::SIGTERM).ok()?;
    Some(pid)
}

// ── Delegation plumbing ─────────────────────────────────────────────────────

/// This ainb binary, for re-entrant delegation.
fn ainb_bin() -> Result<std::path::PathBuf> {
    std::env::current_exe().context("resolve the running ainb binary")
}

/// Run `ainb <args>` to completion and return its trimmed stdout.
///
/// On a non-zero exit the error carries the child's stderr verbatim: that text
/// is what the Daemons screen shows in the row's error view, so it has to be
/// the real reason, not a paraphrase of it.
fn delegate(args: &[&str]) -> Result<String> {
    let bin = ainb_bin()?;
    let out = std::process::Command::new(&bin)
        .args(args)
        .output()
        .with_context(|| format!("run `ainb {}`", args.join(" ")))?;
    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return Ok(if stdout.is_empty() {
            format!("ainb {} ok", args.join(" "))
        } else {
            stdout
        });
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let tail = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("(no stderr)")
        .trim();
    bail!("`ainb {}` failed ({}): {tail}", args.join(" "), out.status)
}

/// Spawn `ainb <args>` detached, for the daemons whose only implementation runs
/// in the foreground. Own process group so a Ctrl-C aimed at the launching
/// terminal never reaches it; output to the daemon log rather than our stdio.
fn detach(args: &[&str]) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let bin = ainb_bin()?;
    let log = crate::fleet::plumbing::paths::ainb_home()?.join("daemons");
    std::fs::create_dir_all(&log).ok();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log.join(format!("{}.log", args.join("-"))))
        .with_context(|| format!("open the log for `ainb {}`", args.join(" ")))?;
    let mut cmd = std::process::Command::new(&bin);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log));
    cmd.process_group(0);
    cmd.spawn().with_context(|| format!("spawn `ainb {}`", args.join(" ")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every daemon in the view is addressable by a stable CLI id, and the ids
    /// round-trip. A kind the CLI cannot name is a row the action menu cannot
    /// act on.
    #[test]
    fn every_controllable_daemon_round_trips_through_its_id() {
        for kind in CONTROLLABLE {
            assert_eq!(
                kind_from_id(kind.id()),
                Some(kind),
                "{} must resolve from its id",
                kind.display_name()
            );
        }
    }

    /// The roster is the whole roster: adding a DaemonKind without adding it
    /// here would silently leave that row uncontrollable.
    #[test]
    fn the_controllable_roster_matches_the_daemons_view() {
        let home = tempfile::tempdir().unwrap();
        let notifyd = tempfile::tempdir().unwrap();
        let mut view: Vec<DaemonKind> =
            crate::fleet::daemons::probe::collect_in(home.path(), notifyd.path(), 0)
                .iter()
                .map(|r| r.kind)
                .collect();
        view.extend(crate::fleet::daemons::probe::collect_socket_daemons().iter().map(|r| r.kind));
        assert_eq!(view, CONTROLLABLE.to_vec());
    }

    /// Every argv this module delegates to must actually parse against the real
    /// clap tree. `fleet atc teardown --yes` did not — there is no such flag —
    /// so `ainb daemon atc stop`, one of the three verbs this whole surface
    /// exists to provide, failed with a usage error every single time.
    #[test]
    fn every_delegated_argv_parses_against_the_real_cli() {
        let registry = crate::cli::registry::CommandRegistry::built_ins();
        let app = registry.build_clap(clap::Command::new("ainb"));
        // The argvs `control` can produce. ATC's carry a placeholder instance
        // name; the shape is what is under test, not the name.
        let delegated: &[&[&str]] = &[
            &["hangar", "daemon", "start"],
            &["hangar", "daemon", "stop"],
            &["hangar", "daemon", "restart"],
            &["notifyd", "restart"],
            &["notifyd", "stop"],
            &["fleet", "atc", "repair", "main"],
            &["fleet", "atc", "teardown", "main"],
            &["fleet", "bridge", "install"],
            &["fleet", "bridge", "uninstall"],
            &["fleet", "daemon"],
        ];
        for argv in delegated {
            let full: Vec<&str> = std::iter::once("ainb").chain(argv.iter().copied()).collect();
            if let Err(e) = app.clone().try_get_matches_from(&full) {
                panic!("`ainb {}` does not parse: {e}", argv.join(" "));
            }
        }
    }

    #[test]
    fn actions_round_trip_through_their_cli_spelling() {
        for action in Action::ALL {
            assert_eq!(Action::from_id(action.id()), Some(action));
        }
        assert_eq!(Action::from_id("bounce"), None);
    }
}
