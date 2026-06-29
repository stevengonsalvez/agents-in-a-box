//! Discovery + classification of running `notifyd` processes.
//!
//! The daemon's own [`crate::pid`] / [`crate::install::status`] surfaces
//! only know about the single pid recorded in `notify.pid`. That is
//! structurally blind to *orphans* — extra `ainb notifyd` processes left
//! behind by a spawn race or a `brew upgrade` that never bound the socket.
//! This module enumerates every notifyd-family process on the host and
//! classifies each against the canonical owner so the TUI can show — and
//! the user can reap — orphans.
//!
//! Enumeration shells out to `ps` rather than pulling in a process-table
//! crate: it runs only on demand (when the Daemons overlay is open), the
//! two supported targets (macOS, Linux) both ship a `ps` that accepts
//! `-axo pid=,etime=,args=`, and the classification logic — the part worth
//! testing — is a pure function over the parsed rows.

use std::process::Command;

use crate::Paths;

/// One running notifyd-family process discovered on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifydProc {
    /// Process id.
    pub pid: u32,
    /// Resolved binary path from `argv[0]` — e.g.
    /// `/opt/homebrew/Cellar/ainb/1.7.4/libexec/ainb`. The version often
    /// reads straight off this path.
    pub bin: String,
    /// Full command line as reported by `ps`.
    pub cmd: String,
    /// Elapsed-time string from `ps` (`[[DD-]HH:]MM:SS`), best-effort.
    pub etime: String,
}

/// How a discovered notifyd process relates to the canonical daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonClass {
    /// Pid recorded in `notify.pid` *and* the socket is present — the
    /// process actually serving notifications.
    LiveOwner,
    /// Pid recorded in `notify.pid` but the socket is missing — the owner
    /// is wedged / not listening.
    StaleOwner,
    /// A running notifyd whose pid is **not** the recorded owner — an
    /// orphan that should be reaped.
    Orphan,
}

impl DaemonClass {
    /// Short human label for the TUI.
    pub fn label(self) -> &'static str {
        match self {
            Self::LiveOwner => "LIVE owner",
            Self::StaleOwner => "STALE owner",
            Self::Orphan => "ORPHAN",
        }
    }

    /// Whether this class is healthy (the one we want) vs something the
    /// user probably wants to clean up.
    pub fn is_healthy(self) -> bool {
        matches!(self, Self::LiveOwner)
    }
}

/// A discovered process plus its relationship to the canonical daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedDaemon {
    /// The underlying process.
    pub proc: NotifydProc,
    /// Live owner / stale owner / orphan.
    pub class: DaemonClass,
    /// True when this process's binary differs from the currently-running
    /// `ainb` binary — i.e. a stale install lingering after an upgrade.
    pub binary_drift: bool,
}

/// Enumerate every running notifyd-family process. Returns an empty vec
/// if `ps` is unavailable or fails — discovery is best-effort.
pub(crate) fn enumerate() -> Vec<NotifydProc> {
    let output = match Command::new("ps").args(["-axo", "pid=,etime=,args="]).output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output)
        .lines()
        .filter_map(parse_ps_line)
        .filter(is_notifyd)
        .collect()
}

/// Parse one `pid etime args...` line from `ps`. `ps` right-justifies the
/// pid and pads between columns, so split on whitespace runs and rejoin
/// the command remainder (original spacing is irrelevant for matching and
/// display).
fn parse_ps_line(line: &str) -> Option<NotifydProc> {
    let mut toks = line.split_whitespace();
    let pid: u32 = toks.next()?.parse().ok()?;
    let etime = toks.next()?.to_string();
    let rest: Vec<&str> = toks.collect();
    if rest.is_empty() {
        return None;
    }
    let cmd = rest.join(" ");
    let bin = rest[0].to_string();
    Some(NotifydProc {
        pid,
        bin,
        cmd,
        etime,
    })
}

/// Does this process look like a *daemon* notifyd? Matches the long-running
/// forms — `ainb notifyd` / `ainb notifyd run`, or the slim `ainb-notifyd`
/// (`run`) binary — but NOT the transient CLI subcommands (`status`, `stop`,
/// `install`, `uninstall`, `list`): those are short-lived invocations, and
/// labelling one ORPHAN with a "kill it" hint would be actively wrong. The
/// `ainb` TUI itself never carries a `notifyd` token, so it is not matched.
fn is_notifyd(p: &NotifydProc) -> bool {
    let base = p.bin.rsplit('/').next().unwrap_or(&p.bin);
    let toks: Vec<&str> = p.cmd.split_whitespace().collect();
    // The subcommand token that selects daemon mode, if any.
    let sub = match base {
        "ainb" => match toks.iter().position(|t| *t == "notifyd") {
            Some(i) => toks.get(i + 1).copied(),
            None => return false,
        },
        "ainb-notifyd" => toks.get(1).copied(),
        _ => return false,
    };
    // Daemon mode is the bare command or an explicit `run`.
    matches!(sub, None | Some("run"))
}

/// Classify each discovered process against the canonical owner pid +
/// whether the socket is actually accepting + the currently-running binary.
/// Pure — the testable core. `socket_accepting` must reflect a real connect
/// probe, not mere file presence: a wedged owner that left a stale socket
/// file behind is the failure this surface exists to show, so it must land
/// as `StaleOwner`, not `LiveOwner`.
pub(crate) fn classify(
    procs: Vec<NotifydProc>,
    owner_pid: Option<u32>,
    socket_accepting: bool,
    current_bin: Option<&str>,
) -> Vec<ClassifiedDaemon> {
    procs
        .into_iter()
        .map(|p| {
            let class = match owner_pid {
                Some(owner) if owner == p.pid => {
                    if socket_accepting {
                        DaemonClass::LiveOwner
                    } else {
                        DaemonClass::StaleOwner
                    }
                }
                _ => DaemonClass::Orphan,
            };
            let binary_drift = current_bin.is_some_and(|cur| !same_binary(&p.bin, cur));
            ClassifiedDaemon {
                proc: p,
                class,
                binary_drift,
            }
        })
        .collect()
}

/// Compare two binary paths for identity, canonicalizing both so a
/// symlinked install (e.g. a `~/.cargo/bin` or Homebrew shim) doesn't read
/// as drift. A path that no longer resolves (old install removed by an
/// upgrade) falls back to a string compare — which correctly reports
/// drift.
fn same_binary(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a).ok(), std::fs::canonicalize(b).ok()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// True when a daemon is actually accepting on the unix socket. A bare
/// socket *file* left by a crashed daemon connects with `ECONNREFUSED`, so
/// this correctly returns false for a wedged owner — unlike `Path::exists`.
/// The throwaway connection sends nothing; the daemon reads EOF and skips
/// it, same as the hook script's `nc` probe.
fn socket_accepting(path: &std::path::Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// Outcome of a [`reap`] sweep.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReapReport {
    /// Pids that were signalled and confirmed gone.
    pub killed: Vec<u32>,
    /// Pids that could not be reaped, with the reason (e.g. `EPERM` —
    /// another user's process — or survival past `SIGKILL`).
    pub failed: Vec<(u32, String)>,
    /// The healthy live owner left untouched, if one was found.
    pub spared: Option<u32>,
}

/// The pids worth reaping from a classified set: everything that is not a
/// healthy live owner (orphans plus a wedged stale owner). Pure — the
/// selection is the part worth testing; the killing is not.
pub(crate) fn reapable(daemons: &[ClassifiedDaemon]) -> Vec<u32> {
    daemons.iter().filter(|d| !d.class.is_healthy()).map(|d| d.proc.pid).collect()
}

fn signal_pid(pid: u32, sig: nix::sys::signal::Signal) -> nix::Result<()> {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), sig)
}

/// Kill every notifyd process that isn't the healthy live owner — the
/// orphans (and a wedged stale owner) the TUI flags. Sends `SIGTERM`,
/// waits briefly, then `SIGKILL`s any survivor: wedged daemons don't
/// always honour `SIGTERM`. The live owner is spared, and this very
/// process can't be hit because its `notifyd reap` command line is
/// excluded by the scan filter. Kills by exact pid only — never a name
/// match or signal broadcast.
pub fn reap() -> ReapReport {
    use nix::errno::Errno;
    use nix::sys::signal::Signal;

    let daemons = scan();
    let spared = daemons.iter().find(|d| d.class.is_healthy()).map(|d| d.proc.pid);
    let mut report = ReapReport {
        spared,
        ..Default::default()
    };

    // Phase 1 — SIGTERM each target; collect the ones that took the signal.
    let mut pending = Vec::new();
    for pid in reapable(&daemons) {
        match signal_pid(pid, Signal::SIGTERM) {
            Ok(()) => pending.push(pid),
            Err(Errno::ESRCH) => report.killed.push(pid), // already gone
            Err(e) => report.failed.push((pid, e.to_string())), // EPERM, etc.
        }
    }

    // Phase 2 — give them a moment, then SIGKILL any survivor and confirm.
    if !pending.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(500));
        for pid in pending {
            if crate::pid::is_running(pid) {
                let _ = signal_pid(pid, Signal::SIGKILL);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if crate::pid::is_running(pid) {
                report.failed.push((pid, "survived SIGKILL".to_string()));
            } else {
                report.killed.push(pid);
            }
        }
    }

    report
}

/// One-shot scan: enumerate notifyd processes and classify them against
/// the on-disk owner pid, the live socket, and this process's own binary.
/// Used by the TUI's Daemons overlay.
pub fn scan() -> Vec<ClassifiedDaemon> {
    let Ok(paths) = Paths::from_home() else {
        return Vec::new();
    };
    let owner_pid = crate::pid::read(&paths.pid).ok().flatten();
    let current_bin = std::env::current_exe().ok().and_then(|p| p.to_str().map(String::from));
    classify(
        enumerate(),
        owner_pid,
        socket_accepting(&paths.socket),
        current_bin.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, bin: &str) -> NotifydProc {
        NotifydProc {
            pid,
            bin: bin.to_string(),
            cmd: format!("{bin} notifyd run"),
            etime: "01:23".to_string(),
        }
    }

    fn classified(class: DaemonClass, pid: u32) -> ClassifiedDaemon {
        ClassifiedDaemon {
            proc: proc(pid, "/x/ainb"),
            class,
            binary_drift: false,
        }
    }

    #[test]
    fn reapable_selects_everything_but_the_live_owner() {
        let ds = vec![
            classified(DaemonClass::LiveOwner, 1),
            classified(DaemonClass::Orphan, 2),
            classified(DaemonClass::StaleOwner, 3),
            classified(DaemonClass::Orphan, 4),
        ];
        assert_eq!(reapable(&ds), vec![2, 3, 4]);
    }

    #[test]
    fn reapable_empty_when_only_live_owner() {
        let ds = vec![classified(DaemonClass::LiveOwner, 1)];
        assert!(reapable(&ds).is_empty());
    }

    #[test]
    fn owner_with_socket_is_live() {
        let out = classify(
            vec![proc(100, "/usr/local/bin/ainb")],
            Some(100),
            true,
            None,
        );
        assert_eq!(out[0].class, DaemonClass::LiveOwner);
    }

    #[test]
    fn owner_without_socket_is_stale() {
        let out = classify(
            vec![proc(100, "/usr/local/bin/ainb")],
            Some(100),
            false,
            None,
        );
        assert_eq!(out[0].class, DaemonClass::StaleOwner);
    }

    #[test]
    fn non_owner_is_orphan() {
        let out = classify(
            vec![proc(200, "/usr/local/bin/ainb")],
            Some(100),
            true,
            None,
        );
        assert_eq!(out[0].class, DaemonClass::Orphan);
    }

    #[test]
    fn no_owner_pid_makes_everything_orphan() {
        let out = classify(
            vec![proc(100, "/a/ainb"), proc(200, "/b/ainb")],
            None,
            true,
            None,
        );
        assert!(out.iter().all(|d| d.class == DaemonClass::Orphan));
    }

    #[test]
    fn binary_drift_flagged_for_nonexistent_mismatched_path() {
        // Neither path resolves, so falls back to string compare → drift.
        let out = classify(
            vec![proc(100, "/opt/homebrew/Cellar/ainb/1.7.4/libexec/ainb")],
            Some(100),
            true,
            Some("/opt/homebrew/Cellar/ainb/1.9.4/libexec/ainb"),
        );
        assert!(out[0].binary_drift);
        assert_eq!(out[0].class, DaemonClass::LiveOwner);
    }

    #[test]
    fn no_drift_when_paths_match() {
        let out = classify(
            vec![proc(100, "/same/path/ainb")],
            Some(100),
            true,
            Some("/same/path/ainb"),
        );
        assert!(!out[0].binary_drift);
    }

    #[test]
    fn parse_ps_line_extracts_fields() {
        let p = parse_ps_line(
            "  41530 06-04:04:24 /opt/homebrew/Cellar/ainb/1.7.4/libexec/ainb notifyd",
        )
        .unwrap();
        assert_eq!(p.pid, 41530);
        assert_eq!(p.etime, "06-04:04:24");
        assert_eq!(p.bin, "/opt/homebrew/Cellar/ainb/1.7.4/libexec/ainb");
        assert!(is_notifyd(&p));
    }

    #[test]
    fn is_notifyd_rejects_plain_ainb_tui() {
        let p = NotifydProc {
            pid: 1,
            bin: "/usr/local/bin/ainb".to_string(),
            cmd: "/usr/local/bin/ainb".to_string(),
            etime: "01:00".to_string(),
        };
        assert!(!is_notifyd(&p));
    }

    #[test]
    fn is_notifyd_accepts_slim_binary() {
        let p = NotifydProc {
            pid: 1,
            bin: "/usr/local/bin/ainb-notifyd".to_string(),
            cmd: "/usr/local/bin/ainb-notifyd run".to_string(),
            etime: "01:00".to_string(),
        };
        assert!(is_notifyd(&p));
    }

    #[test]
    fn is_notifyd_accepts_bare_subcommand() {
        let p = NotifydProc {
            pid: 1,
            bin: "/usr/local/bin/ainb".to_string(),
            cmd: "/usr/local/bin/ainb notifyd".to_string(),
            etime: "01:00".to_string(),
        };
        assert!(is_notifyd(&p));
    }

    #[test]
    fn is_notifyd_rejects_transient_cli_subcommands() {
        // `ainb notifyd status|stop|install|...` are short-lived CLI calls,
        // not daemons — labelling one ORPHAN with a kill hint would be wrong.
        for sub in ["status", "stop", "install", "uninstall", "list"] {
            let p = NotifydProc {
                pid: 1,
                bin: "/usr/local/bin/ainb".to_string(),
                cmd: format!("/usr/local/bin/ainb notifyd {sub}"),
                etime: "00:01".to_string(),
            };
            assert!(!is_notifyd(&p), "should reject `ainb notifyd {sub}`");
        }
    }
}
