// ABOUTME: The ATC heartbeat timer — periodic, NOT a keep-alive daemon.
//
// The heartbeat is an OS timer that fires `ainb fleet atc heartbeat <name>` on a
// cadence: launchd `StartInterval` on macOS, a systemd `--user` timer+service
// pair on Linux. That command builds the `[HEARTBEAT]` nudge from the LLM-free
// `fleet needs` read and tmux-sends it into the ATC session. Install is
// idempotent; teardown removes the unit(s) cleanly and is safe when nothing is
// installed.
//
// The pure builders (`build_plist`, `build_systemd_service`, `build_systemd_timer`)
// are unit-tested; the install/teardown wrappers shell out to launchctl/systemctl
// and are exercised end-to-end.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::fleet::atc::meta::AtcMeta;
use crate::fleet::unit_program;
// Choosing the binary, wrapping it in a shell, and parsing it back out to
// check it resolves are shared with the phone-bridge service unit, which had
// the identical defect (#608). Re-exported so `installed_program_health`'s
// return type stays reachable as `timer::ProgramHealth`.
pub use crate::fleet::unit_program::ProgramHealth;

/// launchd label / systemd unit stem for an instance.
fn unit_stem(name: &str) -> String {
    format!("com.agentsinabox.atc.{name}")
}

/// Resolve the `ainb` binary for the timer command. See
/// [`unit_program::ainb_bin_from`] for why this is never `current_exe()`.
fn ainb_bin() -> String {
    unit_program::ainb_bin_from(std::env::var("AINB_BIN").ok().as_deref())
}

/// The heartbeat command argv the timer runs.
///
/// `argv[0]` is the SHELL, not `ainb`. Neither scheduler will PATH-search the
/// way a naive reading suggests: launchd resolves `ProgramArguments[0]` against
/// its own job environment and ignores the plist's `EnvironmentVariables` PATH
/// entirely (a bare name there dies with exit 78 `EX_CONFIG` before the process
/// ever starts), and systemd resolves a non-absolute `ExecStart` against a
/// fixed compile-time list that excludes `~/.cargo/bin` and `~/.local/bin`.
/// Going through `/bin/sh -c` moves the lookup to a place that DOES honour the
/// unit's `PATH`, and does it at every firing rather than freezing it at setup,
/// which is the whole point: the heartbeat survives a cargo-to-homebrew move
/// with no reinstall.
fn heartbeat_argv_with(bin: &str, name: &str) -> Vec<String> {
    unit_program::shell_wrapped_argv(bin, &["fleet", "atc", "heartbeat", name])
}

// --- macOS launchd ----------------------------------------------------------

/// Path to the per-instance launchd plist.
pub fn launchd_plist_path(name: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not resolve home directory")?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", unit_stem(name))))
}

/// Build the launchd plist XML for the heartbeat timer (`StartInterval`).
#[must_use]
pub fn build_plist(meta: &AtcMeta) -> String {
    build_plist_with(&ainb_bin(), &unit_program::unit_path_env(), meta)
}

fn build_plist_with(bin: &str, path: &str, meta: &AtcMeta) -> String {
    let label = unit_stem(&meta.name);
    let argv = heartbeat_argv_with(bin, &meta.name);
    let home = dirs::home_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let log = home_log_path(&meta.name);

    let args_xml: String = argv
        .iter()
        .map(|a| format!("    <string>{}</string>", xml_escape(a)))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{args_xml}
  </array>
  <key>StartInterval</key>
  <integer>{interval}</integer>
  <key>RunAtLoad</key>
  <true/>
  <key>LowPriorityIO</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
    <key>HOME</key>
    <string>{home}</string>
  </dict>
</dict>
</plist>
"#,
        interval = meta.interval_secs(),
    )
}

// --- Linux systemd (user) ---------------------------------------------------

/// Path to the per-instance systemd `--user` service unit.
pub fn systemd_service_path(name: &str) -> Result<PathBuf> {
    Ok(systemd_user_dir()?.join(format!("{}.service", unit_stem(name))))
}

/// Path to the per-instance systemd `--user` timer unit.
pub fn systemd_timer_path(name: &str) -> Result<PathBuf> {
    Ok(systemd_user_dir()?.join(format!("{}.timer", unit_stem(name))))
}

fn systemd_user_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not resolve home directory")?;
    Ok(home.join(".config").join("systemd").join("user"))
}

/// Build the systemd service unit (the oneshot that fires one heartbeat).
#[must_use]
pub fn build_systemd_service(meta: &AtcMeta) -> String {
    build_systemd_service_with(&ainb_bin(), &unit_program::unit_path_env(), meta)
}

fn build_systemd_service_with(bin: &str, path: &str, meta: &AtcMeta) -> String {
    let argv = heartbeat_argv_with(bin, &meta.name)
        .iter()
        .map(|a| unit_program::shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    // ExecStart is an absolute /bin/sh, so systemd's fixed compile-time binary
    // search path never comes into it. The unit's own PATH is what the shell
    // then resolves `ainb` against, which is why it is written here.
    format!(
        "[Unit]\n\
Description=ATC heartbeat for fleet instance {name}\n\
\n\
[Service]\n\
Type=oneshot\n\
Environment=\"PATH={path}\"\n\
ExecStart={argv}\n",
        name = meta.name,
    )
}

/// Build the systemd timer unit driving the service on a cadence.
#[must_use]
pub fn build_systemd_timer(meta: &AtcMeta) -> String {
    let stem = unit_stem(&meta.name);
    // `Persistent=true` makes systemd fire one catch-up run immediately if a
    // scheduled tick was MISSED while the machine was asleep/off (M-A1), so a
    // laptop that slept through several intervals gets a heartbeat on wake rather
    // than silently skipping until the next on-active tick.
    format!(
        "[Unit]\n\
Description=ATC heartbeat timer for fleet instance {name}\n\
\n\
[Timer]\n\
OnBootSec={interval}\n\
OnUnitActiveSec={interval}\n\
Persistent=true\n\
Unit={stem}.service\n\
\n\
[Install]\n\
WantedBy=timers.target\n",
        name = meta.name,
        interval = meta.interval_secs(),
    )
}

// --- install / teardown -----------------------------------------------------

/// Install the heartbeat timer for `meta`. Idempotent: overwrites any existing
/// unit cleanly. Returns the path(s) written.
pub fn install(meta: &AtcMeta) -> Result<Vec<PathBuf>> {
    if cfg!(target_os = "macos") {
        install_launchd(meta)
    } else {
        install_systemd(meta)
    }
}

/// `Some(warning)` when the unit `install` would write names a program that
/// does not resolve, so setup can say so instead of reporting a success that
/// can never fire. Pinning `current_exe()` used to make this true by
/// construction; resolving at firing time does not, so it is checked.
#[must_use]
pub fn install_would_be_unrunnable(meta: &AtcMeta) -> Option<String> {
    let unit = if cfg!(target_os = "macos") {
        build_plist(meta)
    } else {
        build_systemd_service(meta)
    };
    match unit_program::unit_program_health(&unit) {
        ProgramHealth::Missing(p) => Some(format!(
            "'{p}' is not on the PATH the timer will run with, so the heartbeat cannot fire. \
             Install ainb somewhere on PATH, or set AINB_BIN to its full path and re-run setup."
        )),
        _ => None,
    }
}

/// Remove the heartbeat timer for `name`. Safe when nothing is installed.
/// Returns the path(s) removed.
pub fn teardown(name: &str) -> Result<Vec<PathBuf>> {
    if cfg!(target_os = "macos") {
        teardown_launchd(name)
    } else {
        teardown_systemd(name)
    }
}

/// Whether a heartbeat timer is currently installed for `name`.
pub fn is_installed(name: &str) -> bool {
    if cfg!(target_os = "macos") {
        launchd_plist_path(name).map(|p| p.exists()).unwrap_or(false)
    } else {
        systemd_timer_path(name).map(|p| p.exists()).unwrap_or(false)
    }
}

/// Health of the program the installed unit for `name` would run.
pub fn installed_program_health(name: &str) -> ProgramHealth {
    let unit = if cfg!(target_os = "macos") {
        launchd_plist_path(name)
    } else {
        systemd_service_path(name)
    };
    let Ok(unit) = unit else {
        return ProgramHealth::Unreadable("cannot resolve unit path".into());
    };
    if !unit.exists() {
        return ProgramHealth::NoUnit;
    }
    match std::fs::read_to_string(&unit) {
        Ok(text) => unit_program::unit_program_health(&text),
        Err(e) => ProgramHealth::Unreadable(format!("cannot read {}: {e}", unit.display())),
    }
}

fn install_launchd(meta: &AtcMeta) -> Result<Vec<PathBuf>> {
    let plist = launchd_plist_path(&meta.name)?;
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent).context("creating LaunchAgents dir")?;
    }
    // Unload any prior version first so the reload picks up changes.
    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist.display().to_string()])
        .output();
    std::fs::write(&plist, build_plist(meta)).context("writing launchd plist")?;
    let _ = std::process::Command::new("launchctl")
        .args(["load", &plist.display().to_string()])
        .output();
    Ok(vec![plist])
}

fn teardown_launchd(name: &str) -> Result<Vec<PathBuf>> {
    let plist = launchd_plist_path(name)?;
    let mut removed = Vec::new();
    if plist.exists() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &plist.display().to_string()])
            .output();
        std::fs::remove_file(&plist).context("removing launchd plist")?;
        removed.push(plist);
    }
    Ok(removed)
}

fn install_systemd(meta: &AtcMeta) -> Result<Vec<PathBuf>> {
    let dir = systemd_user_dir()?;
    std::fs::create_dir_all(&dir).context("creating systemd user dir")?;
    let service = systemd_service_path(&meta.name)?;
    let timer = systemd_timer_path(&meta.name)?;
    std::fs::write(&service, build_systemd_service(meta)).context("writing systemd service")?;
    std::fs::write(&timer, build_systemd_timer(meta)).context("writing systemd timer")?;
    let stem = unit_stem(&meta.name);
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", &format!("{stem}.timer")])
        .output();
    Ok(vec![service, timer])
}

fn teardown_systemd(name: &str) -> Result<Vec<PathBuf>> {
    let stem = unit_stem(name);
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", &format!("{stem}.timer")])
        .output();
    let mut removed = Vec::new();
    for path in [systemd_timer_path(name)?, systemd_service_path(name)?] {
        if path.exists() {
            std::fs::remove_file(&path).context("removing systemd unit")?;
            removed.push(path);
        }
    }
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    Ok(removed)
}

// --- helpers ----------------------------------------------------------------

fn home_log_path(name: &str) -> String {
    dirs::home_dir()
        .map(|h| {
            h.join(".agents-in-a-box")
                .join("atc")
                .join(name)
                .join("heartbeat.log")
                .display()
                .to_string()
        })
        .unwrap_or_else(|| format!("/tmp/atc-{name}-heartbeat.log"))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build units from an explicit binary and PATH so the assertions never
    /// depend on the ambient environment of whoever runs the suite.
    fn plist_for(bin: &str, path: &str) -> String {
        build_plist_with(bin, path, &AtcMeta::new("tower"))
    }

    fn service_for(bin: &str, path: &str) -> String {
        build_systemd_service_with(bin, path, &AtcMeta::new("tower"))
    }

    #[test]
    fn plist_carries_label_interval_and_heartbeat_verb() {
        let meta = AtcMeta::new("tower");
        let xml = build_plist(&meta);
        assert!(xml.contains("<string>com.agentsinabox.atc.tower</string>"));
        // 15 min default is 900s StartInterval.
        assert!(xml.contains("<key>StartInterval</key>"));
        assert!(xml.contains("<integer>900</integer>"));
        assert!(xml.contains("fleet atc heartbeat tower"));
    }

    #[test]
    fn plist_respects_custom_interval() {
        let meta = AtcMeta {
            name: "alpha".into(),
            heartbeat_enabled: true,
            heartbeat_interval_min: 5,
            idle_pause_min: 60,
        };
        let xml = build_plist(&meta);
        assert!(xml.contains("<integer>300</integer>"));
    }

    #[test]
    fn systemd_timer_uses_seconds_cadence() {
        let meta = AtcMeta::new("tower");
        let timer = build_systemd_timer(&meta);
        assert!(timer.contains("OnUnitActiveSec=900"));
        assert!(timer.contains("Unit=com.agentsinabox.atc.tower.service"));
        assert!(timer.contains("WantedBy=timers.target"));
        // M-A1: missed-tick catch-up on wake from sleep.
        assert!(timer.contains("Persistent=true"));
    }

    #[test]
    fn systemd_service_invokes_heartbeat_verb() {
        let meta = AtcMeta::new("tower");
        let svc = build_systemd_service(&meta);
        assert!(svc.contains("Type=oneshot"));
        assert!(svc.contains("fleet atc heartbeat tower"));
    }

    #[test]
    fn xml_escape_neutralizes_markup() {
        assert_eq!(xml_escape("a<b>&c"), "a&lt;b&gt;&amp;c");
    }

    /// Regression for the 32-day silent outage: `current_exe()` must never be
    /// frozen into a unit. A cargo-installed ainb that later moved to homebrew
    /// left the scheduler exec'ing a path that no longer existed.
    #[test]
    fn units_do_not_pin_the_current_executable_path() {
        let exe = std::env::current_exe().expect("current_exe");
        let exe = exe.display().to_string();
        for unit in [
            plist_for("ainb", "/usr/bin"),
            service_for("ainb", "/usr/bin"),
        ] {
            assert!(!unit.contains(&exe), "unit pinned current_exe: {unit}");
        }
    }

    /// The heart of the fix, and the thing that has to stay true: the ONLY
    /// absolute path in a unit is the shell. Neither scheduler PATH-searches
    /// argv[0] against the unit's own environment (launchd ignores
    /// EnvironmentVariables entirely and exits 78 EX_CONFIG; systemd uses a
    /// fixed compile-time list), so the binary lookup has to happen inside a
    /// shell that does honour it, at every firing rather than once at setup.
    #[test]
    fn units_reach_the_binary_through_the_shell_not_a_frozen_path() {
        let argv = heartbeat_argv_with("ainb", "tower");
        assert_eq!(argv[0], "/bin/sh");
        assert_eq!(argv[1], "-c");
        assert_eq!(argv[2], "exec ainb fleet atc heartbeat tower");

        let plist = plist_for("ainb", "/usr/bin");
        assert!(plist.contains("<string>/bin/sh</string>"));
        assert!(service_for("ainb", "/usr/bin").contains("ExecStart=/bin/sh -c "));
    }

    /// The apostrophe case end to end, through both unit flavours: if the
    /// quoting round trip mangled the path, the reported program would differ
    /// from `bin` and the timer would be misdiagnosed.
    #[test]
    fn a_path_with_an_apostrophe_survives_the_round_trip() {
        let bin = "/home/o'brien/bin/ainb";
        for unit in [plist_for(bin, "/usr/bin"), service_for(bin, "/usr/bin")] {
            assert_eq!(
                unit_program::unit_program_health(&unit),
                ProgramHealth::Missing(bin.to_string()),
                "apostrophe path mangled in: {unit}"
            );
        }
    }

    #[test]
    fn units_carry_the_path_their_program_needs() {
        let path = "/opt/homebrew/bin:/usr/bin:/bin";
        let service = service_for("ainb", path);
        assert!(
            service.contains(&format!("Environment=\"PATH={path}\"")),
            "systemd service carries no PATH: {service}"
        );
        assert!(
            plist_for("ainb", path).contains(&format!("<string>{path}</string>")),
            "plist carries no PATH"
        );
    }

    /// The status-line decision, over whole units in both flavours: a unit
    /// whose program has moved is flagged, a healthy one is not.
    #[test]
    fn status_flags_a_unit_whose_program_is_missing() {
        let moved = "/Users/nobody/.cargo/bin/ainb";
        for unit in [plist_for(moved, "/usr/bin"), service_for(moved, "/usr/bin")] {
            assert_eq!(
                unit_program::unit_program_health(&unit),
                ProgramHealth::Missing(moved.to_string()),
                "moved binary not flagged: {unit}"
            );
        }
        for unit in [
            plist_for("sh", "/bin:/usr/bin"),
            service_for("sh", "/bin:/usr/bin"),
        ] {
            assert_eq!(
                unit_program::unit_program_health(&unit),
                ProgramHealth::Resolves("sh".to_string()),
                "resolvable binary wrongly flagged: {unit}"
            );
        }
    }

    /// A unit whose PATH cannot reach the binary must be flagged, not assumed
    /// healthy because the operator's own shell happens to resolve it.
    #[test]
    fn a_unit_whose_path_cannot_reach_the_binary_is_flagged() {
        assert_eq!(
            unit_program::unit_program_health(&plist_for("sh", "/nonexistent/bin")),
            ProgramHealth::Missing("sh".into())
        );
        assert_eq!(
            unit_program::unit_program_health(&service_for("sh", "/nonexistent/bin")),
            ProgramHealth::Missing("sh".into())
        );
    }
}
