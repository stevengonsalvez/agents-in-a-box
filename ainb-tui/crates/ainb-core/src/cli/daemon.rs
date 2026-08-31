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
    /// Mint a Codex remote-control pairing code for the phone app.
    ///
    /// Not a lifecycle verb: it neither starts nor stops anything. It is here
    /// because the Hangar daemon owns the Codex transport, so the Daemons
    /// screen is where a user already goes to reason about it. Offered only for
    /// [`DaemonKind::HangarDaemon`]; see `Action::for_kind`.
    Pair,
}

impl Action {
    /// Stable CLI spelling.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Pair => "pair",
        }
    }

    /// Parse a CLI verb.
    #[must_use]
    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            "pair" => Some(Self::Pair),
            _ => None,
        }
    }

    /// Every lifecycle verb, in menu order.
    pub const ALL: [Self; 3] = [Self::Start, Self::Restart, Self::Stop];

    /// The verbs a given daemon offers.
    ///
    /// Pairing is Codex-specific, so it appears only on the daemon that owns
    /// the Codex transport. Offering it on the bridge or the MCP pool would be
    /// an action that cannot mean anything there.
    #[must_use]
    pub fn for_kind(kind: DaemonKind) -> Vec<Self> {
        let mut verbs = Self::ALL.to_vec();
        if matches!(kind, DaemonKind::HangarDaemon) {
            verbs.push(Self::Pair);
        }
        verbs
    }
}

/// Every controllable daemon, in the order the Daemons table lists them.
pub const CONTROLLABLE: [DaemonKind; 9] = [
    DaemonKind::Bridge,
    DaemonKind::Notifyd,
    DaemonKind::ApproveBroker,
    DaemonKind::Atc,
    DaemonKind::FleetDaemon,
    DaemonKind::McpPool,
    DaemonKind::HangarDaemon,
    DaemonKind::HeadroomProxy,
    DaemonKind::ReleaseChecker,
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
    // Pairing is not a lifecycle verb and only the Codex transport has one, so
    // it never reaches the per-daemon handlers below.
    if action == Action::Pair {
        return if matches!(kind, DaemonKind::HangarDaemon) {
            codex_pair()
        } else {
            bail!("`pair` is only available on the hangar daemon, which owns the Codex transport")
        };
    }
    match kind {
        DaemonKind::McpPool => mcp_pool(action),
        DaemonKind::HeadroomProxy => headroom(action).await,
        DaemonKind::HangarDaemon => delegate(&["hangar", "daemon", action.id()]),
        DaemonKind::Notifyd | DaemonKind::ApproveBroker => notifyd(kind, action),
        DaemonKind::Atc => atc(action),
        DaemonKind::Bridge => bridge(action),
        DaemonKind::FleetDaemon => fleet_daemon(action),
        DaemonKind::ReleaseChecker => release_checker(action),
    }
}

fn release_checker(action: Action) -> Result<String> {
    match action {
        Action::Start | Action::Restart => {
            crate::cli::update::ensure_schedule().context("enable daily release checker")?;
            Ok("daily release checker enabled".to_string())
        }
        Action::Stop => {
            crate::cli::update::disable_schedule().context("disable daily release checker")?;
            Ok("daily release checker disabled".to_string())
        }
        Action::Pair => bail!("`pair` is not a lifecycle verb for this daemon"),
    }
}

/// Mint a Codex remote-control pairing code for the phone app.
///
/// Shells `codex remote-control pair` rather than reimplementing it: the code
/// is minted by, and only meaningful to, Codex's own relay. We surface its
/// output verbatim so the user reads the real code and the real failure.
///
/// Deliberately NOT run at startup. The code is a short-lived credential;
/// minting one on every boot is noise, and pairing is a human handshake —
/// the code has to be typed into the phone while it is still valid.
fn codex_pair() -> Result<String> {
    let out = std::process::Command::new("codex")
        .args(["remote-control", "pair"])
        .output()
        .context("run `codex remote-control pair` (is the codex CLI installed?)")?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        let detail = if stderr.is_empty() { stdout } else { stderr };
        bail!("codex remote-control pair failed: {detail}");
    }
    if stdout.is_empty() {
        bail!("codex remote-control pair produced no pairing code");
    }
    Ok(stdout)
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
        // Unreachable: `control` intercepts Pair before any handler.
        Action::Pair => bail!("`pair` is not a lifecycle verb for this daemon"),
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
        // Unreachable: `control` intercepts Pair before any handler.
        Action::Pair => bail!("`pair` is not a lifecycle verb for this daemon"),
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
        // Unreachable: `control` intercepts Pair before any handler.
        Action::Pair => bail!("`pair` is not a lifecycle verb for this daemon"),
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

/// Respawn a dead ATC session WITHOUT resetting the instance's configuration.
///
/// `fleet atc setup` rebuilds meta from `AtcMeta::new` and applies only the
/// flags it is given, so a bare `setup <name>` silently resets an instance
/// provisioned at 10m back to the 15m default, and flips a deliberately
/// disabled heartbeat back on. Pressing Start must not reconfigure anything,
/// so the current values are read off meta.json and passed straight back.
fn respawn_atc_session(name: &str) -> Result<String> {
    let home = crate::fleet::plumbing::paths::ainb_home()?;
    let paths = crate::fleet::atc::paths::AtcPaths::under_root(&home.join("atc"), name);
    let meta = std::fs::read_to_string(&paths.meta)
        .ok()
        .and_then(|raw| crate::fleet::atc::meta::AtcMeta::from_json(&raw).ok())
        .with_context(|| {
            format!(
                "reading {} to respawn without reconfiguring it",
                paths.meta.display()
            )
        })?;
    let interval = meta.heartbeat_interval_min.to_string();
    let idle_pause = meta.idle_pause_min.to_string();
    let mut argv = vec![
        "fleet",
        "atc",
        "setup",
        name,
        "--interval",
        &interval,
        "--idle-pause",
        &idle_pause,
    ];
    if !meta.heartbeat_enabled {
        argv.push("--no-heartbeat");
    }
    delegate(&argv)
}

fn atc(action: Action) -> Result<String> {
    let name = atc_instance()?;
    match action {
        // ATC has two halves and they fail independently. `repair` re-asserts
        // the SCHEDULER (OS timer + daemon registration); it does nothing about
        // the Claude session the scheduler beats into. Starting an ATC whose
        // session has died must respawn the session, and `setup` is the verb
        // that does it: idempotent, and it preserves state.json / task-log.md.
        //
        // Routing everything through `repair` is why "start" failed with
        // "the daemon did NOT accept the unregister" on a host whose only
        // actual fault was a dead tmux session: the wrong half was being fixed,
        // and the scheduler guard then refused a change it did not need.
        Action::Start | Action::Restart => {
            let session = crate::fleet::atc::meta::AtcMeta::new(&name).tmux_session();
            // Only a PROVEN dead session takes the respawn path. `None` means
            // the check could not run, and guessing "dead" there would rewrite
            // a healthy instance's config for an environment problem.
            if crate::tmux::session_alive(&session) == Some(false) {
                respawn_atc_session(&name)
            } else {
                delegate(&["fleet", "atc", "repair", &name])
            }
        }
        // No confirmation flag: `fleet atc teardown` takes only <name> and
        // --purge. Passing --yes made clap reject the whole invocation, so
        // `stop` failed every time.
        Action::Stop => delegate(&["fleet", "atc", "teardown", &name]),
        // Unreachable: `control` intercepts Pair before any handler.
        Action::Pair => bail!("`pair` is not a lifecycle verb for this daemon"),
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
        // Unreachable: `control` intercepts Pair before any handler.
        Action::Pair => bail!("`pair` is not a lifecycle verb for this daemon"),
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
        // Unreachable: `control` intercepts Pair before any handler.
        Action::Pair => bail!("`pair` is not a lifecycle verb for this daemon"),
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
///
/// Refuses outright under `cargo test`: `current_exe()` is then the TEST
/// harness, libtest reads the trailing argv as name FILTERS rather than a
/// subcommand, and `detach` puts each copy in its own process group — so a
/// single delegation becomes an unbounded, detached re-run of the suite. See
/// `crate::self_exec_guard` and issue #715.
fn ainb_bin() -> Result<std::path::PathBuf> {
    if crate::self_exec_guard::running_under_cargo_test() {
        bail!(
            "refusing to run an ainb subcommand from a cargo test binary \
             (current_exe is a test harness, not `ainb`)"
        );
    }
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

    /// `pair` is offered only by the daemon that owns the Codex transport.
    ///
    /// Every other daemon would be advertising an action that cannot mean
    /// anything there, and the CLI would accept a verb its handler rejects.
    #[test]
    fn pair_is_offered_only_by_the_hangar_daemon() {
        for kind in super::CONTROLLABLE {
            let verbs = super::Action::for_kind(kind);
            let has_pair = verbs.contains(&super::Action::Pair);
            assert_eq!(
                has_pair,
                matches!(kind, super::DaemonKind::HangarDaemon),
                "{} offered pair = {has_pair}",
                kind.id()
            );
            // The lifecycle verbs stay on every daemon regardless.
            for verb in super::Action::ALL {
                assert!(verbs.contains(&verb), "{} lost {}", kind.id(), verb.id());
            }
        }
    }

    /// The verb spelling round-trips, so the TUI's shell-out and the CLI agree.
    #[test]
    fn pair_round_trips_through_its_cli_spelling() {
        assert_eq!(super::Action::Pair.id(), "pair");
        assert_eq!(super::Action::from_id("pair"), Some(super::Action::Pair));
    }

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
            // Start on a dead session respawns it, passing the instance's own
            // settings back so nothing is reconfigured.
            &[
                "fleet",
                "atc",
                "setup",
                "main",
                "--interval",
                "10",
                "--idle-pause",
                "60",
            ],
            &[
                "fleet",
                "atc",
                "setup",
                "main",
                "--interval",
                "10",
                "--idle-pause",
                "60",
                "--no-heartbeat",
            ],
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
