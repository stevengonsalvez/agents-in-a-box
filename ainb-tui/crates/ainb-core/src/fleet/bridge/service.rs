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

const LABEL: &str = "com.agentsinabox.phone-bridge";
const SYSTEMD_UNIT: &str = "ainb-phone-bridge.service";

/// Resolved paths used to build the service definition.
#[derive(Debug, Clone)]
pub struct ServicePaths {
    /// Absolute path to the running `ainb` binary (ProgramArguments[0]).
    pub ainb_bin: PathBuf,
    /// `~/.agents-in-a-box/phone-bridge.log`.
    pub log_path: PathBuf,
    /// Optional config override to propagate into the unit env.
    pub config_override: Option<String>,
}

/// Resolve the service paths from the current process. The binary is the
/// canonical absolute path to the running `ainb` so the service runs the exact
/// binary the operator installed from.
pub fn resolve_paths() -> Result<ServicePaths> {
    let ainb_bin = std::env::current_exe().context("resolving the current ainb binary path")?;
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

/// ProgramArguments / ExecStart argv: `<ainb> fleet bridge run`. No token, ever.
fn daemon_argv(paths: &ServicePaths) -> Vec<String> {
    vec![
        paths.ainb_bin.to_string_lossy().into_owned(),
        "fleet".to_string(),
        "bridge".to_string(),
        "run".to_string(),
    ]
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
    let path_env = std::env::var("PATH")
        .unwrap_or_else(|_| "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".to_string());
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
    let exec_start = daemon_argv(paths)
        .iter()
        .map(|a| {
            if a.contains(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
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
pub fn status() -> Result<String> {
    if cfg!(target_os = "macos") {
        let p = launchd_plist_path()?;
        Ok(format!(
            "launchd: {} ({})",
            if p.exists() {
                "installed"
            } else {
                "not installed"
            },
            p.display()
        ))
    } else {
        let p = systemd_unit_path()?;
        Ok(format!(
            "systemd: {} ({})",
            if p.exists() {
                "installed"
            } else {
                "not installed"
            },
            p.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> ServicePaths {
        ServicePaths {
            ainb_bin: PathBuf::from("/usr/local/bin/ainb"),
            log_path: PathBuf::from("/home/u/.agents-in-a-box/phone-bridge.log"),
            config_override: None,
        }
    }

    #[test]
    fn plist_argv_is_fleet_bridge_run_and_carries_no_token() {
        let xml = build_plist(&paths());
        assert!(xml.contains("<string>/usr/local/bin/ainb</string>"));
        assert!(xml.contains("<string>fleet</string>"));
        assert!(xml.contains("<string>bridge</string>"));
        assert!(xml.contains("<string>run</string>"));
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
        assert!(unit.contains("ExecStart=/usr/local/bin/ainb fleet bridge run"));
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
}
