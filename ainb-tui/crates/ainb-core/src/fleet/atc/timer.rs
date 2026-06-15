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

/// launchd label / systemd unit stem for an instance.
fn unit_stem(name: &str) -> String {
    format!("com.agentsinabox.atc.{name}")
}

/// Resolve the `ainb` binary path for the timer command. Prefers `AINB_BIN`,
/// then the current executable, then bare `ainb` on `$PATH`.
fn ainb_bin() -> String {
    if let Ok(b) = std::env::var("AINB_BIN") {
        if !b.is_empty() {
            return b;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(s) = exe.to_str() {
            return s.to_string();
        }
    }
    "ainb".to_string()
}

/// The heartbeat command argv the timer runs.
fn heartbeat_argv(name: &str) -> Vec<String> {
    vec![
        ainb_bin(),
        "fleet".into(),
        "atc".into(),
        "heartbeat".into(),
        name.into(),
    ]
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
    let label = unit_stem(&meta.name);
    let argv = heartbeat_argv(&meta.name);
    let path = std::env::var("PATH")
        .unwrap_or_else(|_| "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".into());
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
    let argv = heartbeat_argv(&meta.name)
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[Unit]\n\
Description=ATC heartbeat for fleet instance {name}\n\
\n\
[Service]\n\
Type=oneshot\n\
ExecStart={argv}\n",
        name = meta.name,
    )
}

/// Build the systemd timer unit driving the service on a cadence.
#[must_use]
pub fn build_systemd_timer(meta: &AtcMeta) -> String {
    let stem = unit_stem(&meta.name);
    format!(
        "[Unit]\n\
Description=ATC heartbeat timer for fleet instance {name}\n\
\n\
[Timer]\n\
OnBootSec={interval}\n\
OnUnitActiveSec={interval}\n\
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

fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_'))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_carries_label_interval_and_heartbeat_verb() {
        let meta = AtcMeta::new("tower");
        let xml = build_plist(&meta);
        assert!(xml.contains("<string>com.agentsinabox.atc.tower</string>"));
        // 15 min default → 900s StartInterval.
        assert!(xml.contains("<key>StartInterval</key>"));
        assert!(xml.contains("<integer>900</integer>"));
        assert!(xml.contains("<string>fleet</string>"));
        assert!(xml.contains("<string>atc</string>"));
        assert!(xml.contains("<string>heartbeat</string>"));
        assert!(xml.contains("<string>tower</string>"));
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

    #[test]
    fn shell_quote_wraps_spaces() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("plain"), "plain");
        assert_eq!(shell_quote("/usr/bin/ainb"), "/usr/bin/ainb");
    }
}
