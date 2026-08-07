// ABOUTME: Install / teardown the native bridge as a launchd (macOS) or systemd
// (Linux) service. Idempotent: install overwrites cleanly, teardown removes
// everything.
//
// The daemon is launched as `<ainb-binary> fleet bridge run`. The token is
// NEVER on the command line — the daemon reads it from config.toml at startup
// via the secret resolver. The unit environment carries only PATH and HOME (and
// AINB_CONFIG_PATH when the running process has an override, so the service
// reads the same config the operator installed from).

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::fleet::unit_program;

const LABEL: &str = "com.agentsinabox.phone-bridge";
const SYSTEMD_UNIT: &str = "ainb-phone-bridge.service";

/// Resolved paths used to build the service definition.
#[derive(Debug, Clone)]
pub struct ServicePaths {
    /// The `ainb` binary the unit's command runs. Bare `ainb` unless
    /// `$AINB_BIN` overrides it — see [`resolve_paths`]. NOT necessarily
    /// `argv[0]`: on launchd that is the shell wrapping this command.
    pub ainb_bin: String,
    /// `~/.agents-in-a-box/phone-bridge.log`.
    pub log_path: PathBuf,
    /// Optional config override to propagate into the unit env.
    pub config_override: Option<String>,
}

/// Resolve the service paths from the current process.
///
/// The binary is deliberately NOT `current_exe()`: that freezes a
/// version-scoped absolute path (a `/opt/homebrew/Cellar/ainb/<version>/libexec`
/// directory, `~/.cargo/bin/ainb`) into the unit, and the next
/// `brew upgrade ainb` deletes it — launchd then exits 78 `EX_CONFIG` and parks
/// the bridge in the penalty box forever. Bare `ainb` is looked up again at
/// every launch, by the shell the unit runs it through (see [`daemon_argv`]);
/// `$AINB_BIN` remains the explicit override. See issue #608.
pub fn resolve_paths() -> Result<ServicePaths> {
    let ainb_bin = unit_program::ainb_bin_from(std::env::var("AINB_BIN").ok().as_deref());
    let mut log_path = dirs::home_dir().context("no home directory")?;
    log_path.push(".agents-in-a-box");
    log_path.push("phone-bridge.log");
    let config_override = std::env::var("AINB_CONFIG_PATH").ok();
    Ok(ServicePaths {
        ainb_bin,
        log_path,
        config_override,
    })
}

/// The command the daemon runs: `<ainb> fleet bridge run`. No token, ever.
const DAEMON_ARGS: [&str; 3] = ["fleet", "bridge", "run"];

/// launchd `ProgramArguments`: `/bin/sh -c "exec <ainb> fleet bridge run"`.
///
/// The shell wrapper is load-bearing, not cosmetic. launchd resolves
/// `ProgramArguments[0]` against its OWN job environment and ignores the
/// plist's `EnvironmentVariables` PATH entirely, so a bare `ainb` there does
/// not spawn at all — worse than the pinned path it replaces. Probed against
/// launchd with this exact plist shape: bare `argv[0]` with the plist PATH set
/// exits 78 `EX_CONFIG` and never runs, `/bin/sh -c "exec …"` exits 0.
fn daemon_argv(paths: &ServicePaths) -> Vec<String> {
    unit_program::shell_wrapped_argv(&paths.ainb_bin, &DAEMON_ARGS)
}

// ── macOS launchd ───────────────────────────────────────────────────────────

fn launchd_plist_path() -> Result<PathBuf> {
    let mut p = dirs::home_dir().context("no home directory")?;
    p.push("Library");
    p.push("LaunchAgents");
    p.push(format!("{LABEL}.plist"));
    Ok(p)
}

/// Render the launchd plist XML. Public for unit testing the generated content
/// (token never present, argv correct, env scoped).
#[must_use]
pub fn build_plist(paths: &ServicePaths) -> String {
    let path_env = unit_program::unit_path_env();
    let home = dirs::home_dir().unwrap_or_default();
    let argv = daemon_argv(paths)
        .into_iter()
        .map(|a| format!("\t\t<string>{}</string>", xml_escape(&a)))
        .collect::<Vec<_>>()
        .join("\n");

    let mut env_entries = format!(
        "\t\t<key>PATH</key>\n\t\t<string>{}</string>\n\t\t<key>HOME</key>\n\t\t<string>{}</string>",
        xml_escape(&path_env),
        xml_escape(&home.to_string_lossy()),
    );
    if let Some(cfg) = &paths.config_override {
        env_entries.push_str(&format!(
            "\n\t\t<key>AINB_CONFIG_PATH</key>\n\t\t<string>{}</string>",
            xml_escape(cfg)
        ));
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
{argv}
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ThrottleInterval</key>
	<integer>10</integer>
	<key>LowPriorityIO</key>
	<true/>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
	<key>EnvironmentVariables</key>
	<dict>
{env_entries}
	</dict>
</dict>
</plist>
"#,
        label = LABEL,
        log = xml_escape(&paths.log_path.to_string_lossy()),
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn install_launchd(paths: &ServicePaths) -> Result<PathBuf> {
    let plist_path = launchd_plist_path()?;
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if let Some(parent) = paths.log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&plist_path, build_plist(paths))
        .with_context(|| format!("writing {}", plist_path.display()))?;
    // Idempotent reload: unload (ignore errors), then load.
    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .output();
    let _ = std::process::Command::new("launchctl")
        .args(["load", &plist_path.to_string_lossy()])
        .output();
    Ok(plist_path)
}

fn teardown_launchd() -> Result<Option<PathBuf>> {
    let plist_path = launchd_plist_path()?;
    if plist_path.exists() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &plist_path.to_string_lossy()])
            .output();
        std::fs::remove_file(&plist_path).ok();
        return Ok(Some(plist_path));
    }
    Ok(None)
}

// ── Linux systemd (user) ────────────────────────────────────────────────────

fn systemd_unit_path() -> Result<PathBuf> {
    let mut p = dirs::config_dir().context("no config directory")?;
    p.push("systemd");
    p.push("user");
    p.push(SYSTEMD_UNIT);
    Ok(p)
}

/// Render the systemd user unit. Public for unit testing.
#[must_use]
pub fn build_systemd_unit(paths: &ServicePaths) -> String {
    // Unlike launchd, systemd DOES resolve a non-absolute `ExecStart` against
    // the $PATH the unit supplies via Environment=, so no shell wrapper is
    // needed here — hence the direct argv rather than `daemon_argv`.
    //
    // Caveat, and the reason `bridge status` checks: that is systemd 250+
    // behaviour. Older systemd searches a fixed compile-time list
    // (/usr/local/bin:/usr/bin:/bin) that misses `~/.cargo/bin` and
    // `~/.local/bin`, where an install outside that list needs the explicit
    // $AINB_BIN override. Status reports such a unit as MISSING rather than
    // certifying a service that can never start as healthy.
    let exec_start = std::iter::once(paths.ainb_bin.as_str())
        .chain(DAEMON_ARGS)
        .map(unit_program::shell_quote)
        .collect::<Vec<_>>()
        .join(" ");
    let config_env = paths
        .config_override
        .as_ref()
        .map(|c| format!("Environment=AINB_CONFIG_PATH={c}\n"))
        .unwrap_or_default();
    format!(
        "[Unit]\n\
         Description=ainb phone bridge (Telegram + Slack)\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         Environment=\"PATH={path}\"\n\
         {config_env}\
         ExecStart={exec_start}\n\
         Restart=always\n\
         RestartSec=10\n\
         StandardOutput=append:{log}\n\
         StandardError=append:{log}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        log = paths.log_path.display(),
        path = unit_program::unit_path_env(),
    )
}

fn install_systemd(paths: &ServicePaths) -> Result<PathBuf> {
    let unit_path = systemd_unit_path()?;
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if let Some(parent) = paths.log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&unit_path, build_systemd_unit(paths))
        .with_context(|| format!("writing {}", unit_path.display()))?;
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", SYSTEMD_UNIT])
        .output();
    Ok(unit_path)
}

fn teardown_systemd() -> Result<Option<PathBuf>> {
    let unit_path = systemd_unit_path()?;
    if unit_path.exists() {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", SYSTEMD_UNIT])
            .output();
        std::fs::remove_file(&unit_path).ok();
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output();
        return Ok(Some(unit_path));
    }
    Ok(None)
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Provision the daemon service for the current platform. Idempotent.
pub fn install() -> Result<PathBuf> {
    let paths = resolve_paths()?;
    if cfg!(target_os = "macos") {
        install_launchd(&paths)
    } else {
        install_systemd(&paths)
    }
}

/// Remove the daemon service. Safe to call when nothing is installed.
pub fn uninstall() -> Result<Option<PathBuf>> {
    if cfg!(target_os = "macos") {
        teardown_launchd()
    } else {
        teardown_systemd()
    }
}

/// Human-readable install status for the current platform.
///
/// A unit file existing is NOT the same as a working bridge: if the binary it
/// points at has moved (a `brew upgrade` deleting the old Cellar directory),
/// launchd exits 78 `EX_CONFIG` and parks the job forever while `installed`
/// keeps claiming health. So the program is resolved too — see issue #608.
pub fn status() -> Result<String> {
    let (kind, unit) = if cfg!(target_os = "macos") {
        ("launchd", launchd_plist_path()?)
    } else {
        ("systemd", systemd_unit_path()?)
    };
    if !unit.exists() {
        return Ok(format!("{kind}: not installed ({})", unit.display()));
    }
    let health = match std::fs::read_to_string(&unit) {
        Ok(text) => unit_program::unit_program_health(&text),
        Err(e) => {
            unit_program::ProgramHealth::Unreadable(format!("cannot read {}: {e}", unit.display()))
        }
    };
    let note = health.problem().map_or_else(String::new, |problem| {
        format!(
            ", {problem}\n  \
             ↳ the bridge can never start — re-run `ainb fleet bridge install` to repoint it"
        )
    });
    Ok(format!("{kind}: installed ({}){note}", unit.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> ServicePaths {
        ServicePaths {
            ainb_bin: "ainb".to_string(),
            log_path: PathBuf::from("/home/u/.agents-in-a-box/phone-bridge.log"),
            config_override: None,
        }
    }

    /// A Cellar path for a version that cannot be installed, so the "moved
    /// binary" assertions never depend on what this host happens to have.
    const GONE: &str = "/opt/homebrew/Cellar/ainb/0.0.0-uninstalled/libexec/ainb";

    #[test]
    fn plist_argv_is_fleet_bridge_run_and_carries_no_token() {
        let xml = build_plist(&paths());
        assert!(xml.contains("<string>/bin/sh</string>"));
        assert!(xml.contains("<string>-c</string>"));
        assert!(xml.contains("<string>exec ainb fleet bridge run</string>"));
        // No token leakage: nothing token-ish in the rendered plist.
        let lower = xml.to_lowercase();
        assert!(!lower.contains("token"), "plist must never embed a token");
        assert!(
            !lower.contains("xoxb"),
            "plist must never embed a slack token"
        );
    }

    #[test]
    fn plist_scopes_env_to_path_and_home() {
        let xml = build_plist(&paths());
        assert!(xml.contains("<key>PATH</key>"));
        assert!(xml.contains("<key>HOME</key>"));
    }

    #[test]
    fn plist_includes_config_override_when_set() {
        let mut p = paths();
        p.config_override = Some("/custom/config.toml".to_string());
        let xml = build_plist(&p);
        assert!(xml.contains("<key>AINB_CONFIG_PATH</key>"));
        assert!(xml.contains("<string>/custom/config.toml</string>"));
    }

    #[test]
    fn systemd_exec_start_is_fleet_bridge_run() {
        let unit = build_systemd_unit(&paths());
        assert!(unit.contains("ExecStart=ainb fleet bridge run"));
        assert!(unit.contains("Restart=always"));
        assert!(!unit.to_lowercase().contains("token"));
    }

    #[test]
    fn systemd_includes_config_override_when_set() {
        let mut p = paths();
        p.config_override = Some("/custom/config.toml".to_string());
        let unit = build_systemd_unit(&p);
        assert!(unit.contains("Environment=AINB_CONFIG_PATH=/custom/config.toml"));
    }

    /// Regression for issue #608: `ainb fleet bridge install` on a homebrew
    /// install pinned the Cellar VERSION directory, which the next
    /// `brew upgrade` deletes. Nothing derived from `current_exe()` may reach
    /// a unit.
    #[test]
    fn units_do_not_pin_an_absolute_binary_path() {
        let resolved = resolve_paths().expect("resolve_paths");
        let plist = build_plist(&resolved);
        let unit = build_systemd_unit(&resolved);

        let exe = std::env::current_exe().expect("current_exe");
        let exe = exe.display().to_string();
        assert!(!plist.contains(&exe), "plist pinned current_exe: {plist}");
        assert!(
            !unit.contains(&exe),
            "systemd unit pinned current_exe: {unit}"
        );

        // Absent an explicit $AINB_BIN override, the program the unit actually
        // runs carries no directory component, so there is no Cellar/cargo
        // prefix that a later upgrade can invalidate.
        if std::env::var("AINB_BIN").unwrap_or_default().is_empty() {
            for text in [&plist, &unit] {
                let program = unit_program::unit_program_health(text)
                    .program()
                    .map(str::to_string)
                    .expect("program");
                assert_eq!(program, "ainb", "program should be bare, got {program}");
            }
        }
    }

    /// launchd will NOT PATH-search `ProgramArguments[0]`; it resolves it
    /// against its own job environment and ignores the plist's
    /// `EnvironmentVariables` PATH. So the plist must hand it an absolute
    /// `/bin/sh` and let the shell — which does honour PATH — find `ainb`.
    /// A bare `ainb` here would not spawn at all.
    #[test]
    fn launchd_gets_an_absolute_program_and_defers_lookup_to_the_shell() {
        let plist = build_plist(&paths());
        let argv0 = plist
            .split_once("<key>ProgramArguments</key>")
            .and_then(|(_, rest)| rest.split_once("<string>"))
            .and_then(|(_, rest)| rest.split_once("</string>"))
            .map(|(v, _)| v.trim().to_string())
            .expect("argv[0]");
        assert_eq!(argv0, "/bin/sh");
        assert!(
            std::path::Path::new(&argv0).is_absolute(),
            "launchd cannot spawn a non-absolute argv[0]"
        );
        // And the PATH the shell will use is still written into the plist.
        assert!(plist.contains("<key>PATH</key>"));
    }

    /// systemd, unlike launchd, DOES resolve a non-absolute `ExecStart`
    /// against the `$PATH` the unit supplies, so no shell wrapper is needed —
    /// but the unit must actually supply that PATH.
    #[test]
    fn systemd_unit_carries_the_path_its_bare_exec_start_needs() {
        let expected = unit_program::unit_path_env();
        let unit = build_systemd_unit(&paths());
        assert!(unit.contains("ExecStart=ainb fleet bridge run"));
        assert!(
            unit.contains(&format!("Environment=\"PATH={expected}\"")),
            "systemd unit carries no PATH: {unit}"
        );
    }

    /// The decision `status()` makes: a unit whose program has moved is
    /// flagged, a resolvable one is not. Run over whole units in both flavours
    /// so the bare case is judged against the unit's own PATH.
    #[test]
    fn status_flags_a_unit_whose_program_is_missing() {
        use unit_program::ProgramHealth;

        // Repoint the binary at the one place it appears in each flavour: the
        // shell command string for launchd, the ExecStart line for systemd.
        let repoint = |to: &str| {
            let mut p = paths();
            p.ainb_bin = to.to_string();
            [build_plist(&p), build_systemd_unit(&p)]
        };

        for stale in repoint(GONE) {
            assert_eq!(
                unit_program::unit_program_health(&stale),
                ProgramHealth::Missing(GONE.into()),
                "moved binary not flagged: {stale}"
            );
        }
        // `ls`, not `sh`: a shell name is treated as a wrapper and unwrapped,
        // so it cannot stand in for an ordinary resolvable binary here.
        for healthy in repoint("ls") {
            assert_eq!(
                unit_program::unit_program_health(&healthy),
                ProgramHealth::Resolves("ls".into()),
                "resolvable binary wrongly flagged: {healthy}"
            );
        }
    }

    /// The wrapper must not become a blindfold: `/bin/sh` always resolves, so
    /// a detector reading `argv[0]` would certify every broken bridge as
    /// healthy — the exact bug class #608 is about.
    #[test]
    fn the_shell_wrapper_does_not_hide_a_dead_binary_from_status() {
        let mut p = paths();
        p.ainb_bin = GONE.to_string();
        let plist = build_plist(&p);
        assert!(
            plist.contains("<string>/bin/sh</string>"),
            "premise: {plist}"
        );
        assert_eq!(
            unit_program::unit_program_health(&plist).program(),
            Some(GONE)
        );
    }

    /// A stale unit installed by an older ainb has no shell wrapper at all.
    /// It must still be judged on its pinned binary, not reported unreadable.
    #[test]
    fn a_legacy_unwrapped_unit_is_still_flagged() {
        let legacy = format!(
            "<key>ProgramArguments</key>\n<array>\n\t<string>{GONE}</string>\n\t\
             <string>fleet</string>\n\t<string>bridge</string>\n\t<string>run</string>\n</array>"
        );
        assert_eq!(
            unit_program::unit_program_health(&legacy),
            unit_program::ProgramHealth::Missing(GONE.into())
        );
    }
}
