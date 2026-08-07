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

/// Resolve the `ainb` binary for the timer command.
///
/// Deliberately NOT `current_exe()`: that freezes an absolute path (e.g.
/// `~/.cargo/bin/ainb`) into the unit forever, so a later `brew install ainb`
/// leaves launchd exec'ing a path that no longer exists — exit 78 `EX_CONFIG`,
/// penalty box, heartbeat silently dead. Bare `ainb` re-resolves at every
/// firing against the `PATH` the unit already carries. `$AINB_BIN` stays as the
/// explicit override for installs that are not on `PATH`.
fn ainb_bin() -> String {
    ainb_bin_from(std::env::var("AINB_BIN").ok().as_deref())
}

fn ainb_bin_from(override_var: Option<&str>) -> String {
    match override_var {
        Some(b) if !b.is_empty() => b.to_string(),
        _ => "ainb".to_string(),
    }
}

/// The `PATH` written into the unit, and therefore the one a bare `argv[0]` is
/// resolved against at firing time.
fn unit_path_env() -> String {
    std::env::var("PATH")
        .unwrap_or_else(|_| "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".into())
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
    let path = unit_path_env();
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
    // The unit carries its own PATH, exactly as the plist does. Without it a
    // bare `ainb` would be resolved against systemd's fixed compile-time search
    // path (/usr/local/bin:/usr/bin:/bin), which misses `~/.cargo/bin` and
    // `~/.local/bin` — a $PATH supplied via Environment= is what systemd (250+)
    // searches instead. On older systemd, an install outside the fixed list
    // needs the explicit $AINB_BIN override; `atc status` now says so out loud
    // rather than reporting a timer that can never fire as healthy.
    format!(
        "[Unit]\n\
Description=ATC heartbeat for fleet instance {name}\n\
\n\
[Service]\n\
Type=oneshot\n\
Environment=\"PATH={path}\"\n\
ExecStart={argv}\n",
        name = meta.name,
        path = unit_path_env(),
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

/// The `argv[0]` of the installed unit for `name`, when a unit exists.
///
/// launchd reads `ProgramArguments[0]` from the plist; systemd reads the first
/// token of `ExecStart` in the service unit.
pub fn installed_program(name: &str) -> Option<String> {
    unit_program(&installed_unit_text(name)?)
}

/// `Some(program)` when the installed unit points at a program that does not
/// resolve — i.e. the timer is installed but can never fire. `None` when there
/// is no unit, or the program resolves fine.
pub fn installed_missing_program(name: &str) -> Option<String> {
    let text = installed_unit_text(name)?;
    missing_program(&text)
}

/// Text of the unit the scheduler actually execs for `name`.
fn installed_unit_text(name: &str) -> Option<String> {
    let unit = if cfg!(target_os = "macos") {
        launchd_plist_path(name)
    } else {
        systemd_service_path(name)
    }
    .ok()?;
    std::fs::read_to_string(unit).ok()
}

/// Extract `argv[0]` from a unit's text — launchd plist XML or a systemd
/// service unit. Returns `None` when the shape is not recognised.
fn unit_program(text: &str) -> Option<String> {
    plist_program(text).or_else(|| systemd_exec_start_program(text))
}

/// The `PATH` the unit itself carries, which is what the scheduler resolves a
/// bare `argv[0]` against — NOT the `PATH` of whoever is running `atc status`.
fn unit_search_path(text: &str) -> Option<String> {
    plist_string_after_key(text, "PATH").or_else(|| systemd_environment_path(text))
}

/// `Some(program)` when the unit's `argv[0]` does not resolve to an executable
/// under the unit's own `PATH` — i.e. the timer is installed but can never fire.
fn missing_program(unit_text: &str) -> Option<String> {
    let program = unit_program(unit_text)?;
    let search_path = unit_search_path(unit_text);
    (!program_resolves_in(&program, search_path.as_deref())).then_some(program)
}

/// Whether a unit's `argv[0]` resolves to something executable. An explicit
/// path is checked directly; a bare name is looked up under `search_path` (the
/// unit's own `PATH`), falling back to the caller's `$PATH` when the unit
/// carries none.
///
/// `which` rather than `Path::exists()` on purpose: `exists()` is true for a
/// directory or a non-executable file, either of which would let a bogus
/// `$AINB_BIN` report healthy while the scheduler cannot exec it.
fn program_resolves_in(program: &str, search_path: Option<&str>) -> bool {
    if program.is_empty() {
        return false;
    }
    if program.contains('/') {
        return which::which(program).is_ok();
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    search_path.map_or_else(
        || which::which(program).is_ok(),
        |path| which::which_in(program, Some(path), cwd).is_ok(),
    )
}

/// First `<string>` inside the `ProgramArguments` array of a launchd plist.
fn plist_program(xml: &str) -> Option<String> {
    let after_key = xml.split_once("<key>ProgramArguments</key>")?.1;
    let array = after_key.split_once("<array>")?.1;
    first_plist_string(array)
}

/// The `<string>` value following `<key>{key}</key>` in a launchd plist.
fn plist_string_after_key(xml: &str, key: &str) -> Option<String> {
    first_plist_string(xml.split_once(&format!("<key>{key}</key>"))?.1)
}

fn first_plist_string(xml: &str) -> Option<String> {
    let open = xml.split_once("<string>")?.1;
    let (value, _) = open.split_once("</string>")?;
    let value = xml_unescape(value.trim());
    (!value.is_empty()).then_some(value)
}

/// First token of the `ExecStart=` line of a systemd service unit.
fn systemd_exec_start_program(unit: &str) -> Option<String> {
    let line = unit.lines().map(str::trim).find_map(|l| l.strip_prefix("ExecStart="))?;
    shell_unquote_first_token(line.trim())
}

/// The `PATH` assignment of a systemd service unit's `Environment=` lines.
fn systemd_environment_path(unit: &str) -> Option<String> {
    let value = unit.lines().map(str::trim).find_map(|line| {
        let assignment = line.strip_prefix("Environment=")?.trim().trim_matches('"');
        assignment.strip_prefix("PATH=").map(str::to_string)
    })?;
    (!value.is_empty()).then_some(value)
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

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Inverse of `shell_quote` for the leading token only — enough to recover
/// `argv[0]` from an `ExecStart=` line we wrote ourselves.
fn shell_unquote_first_token(s: &str) -> Option<String> {
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix('\'') {
        let (quoted, _) = rest.split_once('\'')?;
        return Some(quoted.to_string());
    }
    let token = s.split_whitespace().next()?;
    (!token.is_empty()).then(|| token.to_string())
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

    #[test]
    fn shell_quote_wraps_spaces() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("plain"), "plain");
        assert_eq!(shell_quote("/usr/bin/ainb"), "/usr/bin/ainb");
    }

    #[test]
    fn ainb_bin_is_bare_unless_explicitly_overridden() {
        assert_eq!(ainb_bin_from(None), "ainb");
        assert_eq!(ainb_bin_from(Some("")), "ainb");
        assert_eq!(ainb_bin_from(Some("/opt/custom/ainb")), "/opt/custom/ainb");
    }

    /// Regression for the 32-day silent outage: `current_exe()` must never be
    /// frozen into a unit. A cargo-installed `ainb` that later moves to homebrew
    /// left launchd exec'ing a path that no longer existed (exit 78 `EX_CONFIG`).
    #[test]
    fn units_do_not_pin_the_current_executable_path() {
        let meta = AtcMeta::new("tower");
        let exe = std::env::current_exe().expect("current_exe");
        let exe = exe.display().to_string();
        let plist = build_plist(&meta);
        let service = build_systemd_service(&meta);
        assert!(!plist.contains(&exe), "plist pinned current_exe: {plist}");
        assert!(
            !service.contains(&exe),
            "systemd service pinned current_exe: {service}"
        );
        // Sanity: absent an explicit $AINB_BIN override, the emitted argv[0]
        // carries no directory component at all, so there is no cargo/homebrew
        // prefix that can go stale.
        if std::env::var("AINB_BIN").unwrap_or_default().is_empty() {
            for unit in [&plist, &service] {
                let program = unit_program(unit).expect("argv[0]");
                assert!(
                    !program.contains('/'),
                    "argv[0] should be bare, got {program}"
                );
            }
        }
    }

    #[test]
    fn unit_program_reads_argv0_from_both_unit_flavours() {
        let meta = AtcMeta::new("tower");
        let expected = ainb_bin();
        assert_eq!(
            unit_program(&build_plist(&meta)).as_deref(),
            Some(&*expected)
        );
        assert_eq!(
            unit_program(&build_systemd_service(&meta)).as_deref(),
            Some(&*expected)
        );
        assert_eq!(unit_program("not a unit at all"), None);
    }

    #[test]
    fn unit_program_survives_quoting_and_escaping() {
        assert_eq!(
            plist_program("<key>ProgramArguments</key>\n<array>\n<string>/a&amp;b/ainb</string>"),
            Some("/a&b/ainb".into())
        );
        assert_eq!(
            systemd_exec_start_program("[Service]\nExecStart='/a b/ainb' fleet atc heartbeat x\n"),
            Some("/a b/ainb".into())
        );
    }

    #[test]
    fn program_resolution_distinguishes_present_from_moved() {
        assert!(program_resolves_in("/bin/sh", None));
        assert!(!program_resolves_in("/nonexistent/cargo/bin/ainb", None));
        assert!(!program_resolves_in("", None));
        assert!(program_resolves_in("sh", None));
        assert!(!program_resolves_in(
            "ainb-definitely-not-a-real-binary",
            None
        ));
    }

    /// `exists()` would call a directory or a non-executable file healthy; the
    /// scheduler cannot exec either, so neither may pass.
    #[test]
    fn program_resolution_requires_something_executable() {
        let dir = std::env::temp_dir();
        assert!(
            dir.exists(),
            "temp dir should exist for the premise to hold"
        );
        assert!(!program_resolves_in(&dir.display().to_string(), None));

        let not_executable = dir.join("ainb-timer-test-not-executable");
        std::fs::write(&not_executable, b"#!/bin/sh\n").expect("write fixture");
        assert!(!program_resolves_in(
            &not_executable.display().to_string(),
            None
        ));
        let _ = std::fs::remove_file(&not_executable);
    }

    /// A bare `argv[0]` must be judged against the PATH the UNIT carries — the
    /// one the scheduler will use — not against whatever `$PATH` the operator
    /// happens to be running `atc status` with.
    #[test]
    fn bare_program_resolves_against_the_units_own_path() {
        assert!(program_resolves_in("sh", Some("/bin:/usr/bin")));
        assert!(!program_resolves_in("sh", Some("/nonexistent/bin")));
    }

    #[test]
    fn units_carry_the_path_their_bare_argv0_needs() {
        let meta = AtcMeta::new("tower");
        let expected = unit_path_env();
        let service = build_systemd_service(&meta);
        // systemd resolves a bare ExecStart against a fixed compile-time list
        // unless the unit supplies $PATH itself, so the unit must supply it.
        assert!(
            service.contains(&format!("Environment=\"PATH={expected}\"")),
            "systemd service carries no PATH: {service}"
        );
        assert_eq!(unit_search_path(&service).as_deref(), Some(&*expected));
        assert_eq!(
            unit_search_path(&build_plist(&meta)).as_deref(),
            Some(&*expected)
        );
        assert_eq!(unit_search_path("not a unit at all"), None);
    }

    /// The status-line decision: a unit whose program has moved is flagged,
    /// a healthy one is not. Runs over whole units, in both flavours, so the
    /// unit's own PATH is what the bare case is judged against.
    #[test]
    fn status_flags_a_unit_whose_program_is_missing() {
        let meta = AtcMeta::new("tower");
        let plist = build_plist(&meta);
        let service = build_systemd_service(&meta);
        let argv0 = unit_program(&plist).expect("argv[0]");
        // Substitute argv[0] where it actually sits — a bare name is too short
        // to replace safely across a whole unit (the PATH could contain it).
        let repoint = |unit: &str, to: &str| {
            unit.replace(
                &format!("<string>{argv0}</string>"),
                &format!("<string>{to}</string>"),
            )
            .replace(&format!("ExecStart={argv0} "), &format!("ExecStart={to} "))
        };

        for base in [&plist, &service] {
            let stale = repoint(base, "/Users/nobody/.cargo/bin/ainb");
            assert_eq!(
                missing_program(&stale),
                Some("/Users/nobody/.cargo/bin/ainb".into()),
                "moved binary not flagged: {stale}"
            );

            let healthy = repoint(base, "sh");
            assert_eq!(
                missing_program(&healthy),
                None,
                "resolvable binary wrongly flagged: {healthy}"
            );
        }
    }

    /// The systemd regression the bare-argv0 change could have introduced: a
    /// unit whose PATH cannot reach the binary must be flagged, not assumed
    /// healthy because the operator's own shell happens to resolve it.
    #[test]
    fn a_unit_whose_path_cannot_reach_the_binary_is_flagged() {
        let unit = "[Service]\nType=oneshot\nEnvironment=\"PATH=/nonexistent/bin\"\nExecStart=sh fleet atc heartbeat tower\n";
        assert_eq!(missing_program(unit), Some("sh".into()));

        let reachable = "[Service]\nType=oneshot\nEnvironment=\"PATH=/bin:/usr/bin\"\nExecStart=sh fleet atc heartbeat tower\n";
        assert_eq!(missing_program(reachable), None);
    }
}
