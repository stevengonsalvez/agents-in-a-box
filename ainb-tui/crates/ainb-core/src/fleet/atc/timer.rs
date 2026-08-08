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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::fleet::atc::meta::AtcMeta;

/// launchd label / systemd unit stem for an instance.
fn unit_stem(name: &str) -> String {
    format!("com.agentsinabox.atc.{name}")
}

/// A path that is guaranteed present and never moves, used to reach a binary
/// whose location is not.
const SHELL: &str = "/bin/sh";

/// Resolve the `ainb` binary for the timer command.
///
/// Deliberately NOT `current_exe()`: that freezes an absolute path (e.g.
/// `~/.cargo/bin/ainb`) into the unit forever, so a later `brew install ainb`
/// leaves the scheduler exec'ing a path that no longer exists (exit 78
/// `EX_CONFIG`, penalty box, heartbeat silently dead). `ainb` stays a bare name
/// here and is resolved by the shell at every firing, against the `PATH` the
/// unit carries. `$AINB_BIN` is the explicit override for installs that are not
/// on `PATH`.
fn ainb_bin() -> String {
    ainb_bin_from(std::env::var("AINB_BIN").ok().as_deref())
}

fn ainb_bin_from(override_var: Option<&str>) -> String {
    match override_var {
        Some(b) if !b.is_empty() => b.to_string(),
        _ => "ainb".to_string(),
    }
}

/// The `PATH` written into the unit, and therefore the one the shell resolves a
/// bare `ainb` against at firing time.
fn unit_path_env() -> String {
    std::env::var("PATH")
        .unwrap_or_else(|_| "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".into())
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
    let command = [bin, "fleet", "atc", "heartbeat", name]
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    // `exec` so the shell replaces itself with ainb rather than lingering as a
    // parent for the life of the heartbeat.
    vec![SHELL.into(), "-c".into(), format!("exec {command}")]
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
    build_plist_with(&ainb_bin(), &unit_path_env(), meta)
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
    build_systemd_service_with(&ainb_bin(), &unit_path_env(), meta)
}

fn build_systemd_service_with(bin: &str, path: &str, meta: &AtcMeta) -> String {
    let argv = heartbeat_argv_with(bin, &meta.name)
        .iter()
        .map(|a| shell_quote(a))
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

/// The unit file path(s) [`install`] writes for `name`, whether or not they
/// exist yet. macOS: the launchd plist. Linux: the systemd `[service, timer]`
/// pair, in that order.
///
/// `install` and [`install_would_change`] both derive their paths from here so
/// "what would repair touch" can never drift from what install actually writes.
pub fn unit_paths(name: &str) -> Result<Vec<PathBuf>> {
    if cfg!(target_os = "macos") {
        Ok(vec![launchd_plist_path(name)?])
    } else {
        Ok(vec![systemd_service_path(name)?, systemd_timer_path(name)?])
    }
}

/// Health of the program the unit [`install`] WOULD write for `meta`, as
/// opposed to the one currently on disk (that is [`installed_program_health`]).
///
/// The unit is built in memory from the CURRENT process env (`$AINB_BIN` /
/// `$PATH`), which is exactly what makes a rewrite a repair: the answer here is
/// "would a freshly written unit be able to fire from this shell".
#[must_use]
pub fn fresh_program_health(meta: &AtcMeta) -> ProgramHealth {
    let unit = if cfg!(target_os = "macos") {
        build_plist(meta)
    } else {
        build_systemd_service(meta)
    };
    unit_program_health(&unit)
}

/// `Some(warning)` when the unit `install` would write names a program that
/// does not resolve, so setup can say so instead of reporting a success that
/// can never fire. Pinning `current_exe()` used to make this true by
/// construction; resolving at firing time does not, so it is checked.
#[must_use]
pub fn install_would_be_unrunnable(meta: &AtcMeta) -> Option<String> {
    match fresh_program_health(meta) {
        ProgramHealth::Missing(p) => Some(format!(
            "'{p}' is not on the PATH the timer will run with, so the heartbeat cannot fire. \
             Install ainb somewhere on PATH, or set AINB_BIN to its full path and re-run setup."
        )),
        _ => None,
    }
}

/// Whether the unit(s) [`install`] would write differ from what is on disk.
/// True when nothing is installed.
///
/// Lets `repair` report honestly whether it actually moved anything instead of
/// claiming a rewrite on every run. It is NOT a reason to skip `install`: the
/// install path also re-loads the job with launchctl/systemctl, and a healthy
/// unit whose job was unloaded is still a dead heartbeat.
#[must_use]
pub fn install_would_change(meta: &AtcMeta) -> bool {
    if cfg!(target_os = "macos") {
        let Ok(plist) = launchd_plist_path(&meta.name) else {
            return true;
        };
        install_would_change_from(read_unit(&plist).as_deref(), &build_plist(meta))
    } else {
        let (Ok(service), Ok(timer)) = (
            systemd_service_path(&meta.name),
            systemd_timer_path(&meta.name),
        ) else {
            return true;
        };
        install_would_change_from(read_unit(&service).as_deref(), &build_systemd_service(meta))
            || install_would_change_from(read_unit(&timer).as_deref(), &build_systemd_timer(meta))
    }
}

/// The pure core of [`install_would_change`], per unit file. `None` means the
/// unit is absent, which always counts as a change.
fn install_would_change_from(existing: Option<&str>, fresh: &str) -> bool {
    existing.is_none_or(|text| text != fresh)
}

fn read_unit(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Whether to actually (un)load the unit with launchctl/systemctl after writing
/// it. Off in containers and CI, where there is no user launchd domain or
/// systemd session bus and the shell-out is a no-op that can still schedule a
/// real job on a developer machine.
fn activation_enabled() -> bool {
    !matches!(
        std::env::var("AINB_TIMER_SKIP_ACTIVATION").as_deref(),
        Ok("1")
    )
}

/// Whether unit activation is being skipped, so callers can avoid reporting a
/// unit that was written but never loaded as a running timer.
#[must_use]
pub fn activation_is_skipped() -> bool {
    !activation_enabled()
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

/// What `atc status` needs to know about the program an installed unit will try
/// to run. Every variant is distinct on purpose: an unreadable or unrecognised
/// unit is NOT the same as a healthy one, and reporting it as healthy is the
/// exact false positive this whole check exists to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramHealth {
    /// No unit installed for this instance.
    NoUnit,
    /// The unit's program resolves; the timer can fire.
    Resolves(String),
    /// The unit's program does not resolve; the timer can never fire.
    Missing(String),
    /// The unit exists but could not be read or understood, so its health is
    /// unknown and must not be reported as good.
    Unreadable(String),
}

impl ProgramHealth {
    /// The program path, when one could be parsed out of the unit.
    #[must_use]
    pub fn program(&self) -> Option<&str> {
        match self {
            Self::Resolves(p) | Self::Missing(p) => Some(p),
            Self::NoUnit | Self::Unreadable(_) => None,
        }
    }

    /// A short note for the status line, when something is wrong.
    #[must_use]
    pub fn problem(&self) -> Option<String> {
        match self {
            Self::Missing(p) => Some(format!("program MISSING ({p})")),
            Self::Unreadable(why) => Some(format!("program UNKNOWN ({why})")),
            Self::NoUnit | Self::Resolves(_) => None,
        }
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
        Ok(text) => unit_program_health(&text),
        Err(e) => ProgramHealth::Unreadable(format!("cannot read {}: {e}", unit.display())),
    }
}

/// Health of the program a unit's text describes.
fn unit_program_health(text: &str) -> ProgramHealth {
    let Some(argv) = unit_argv(text) else {
        return ProgramHealth::Unreadable("unrecognised unit shape".into());
    };
    let Some(program) = program_from_argv(&argv) else {
        return ProgramHealth::Unreadable("no program in unit".into());
    };
    if program_resolves_in(&program, unit_search_path(text).as_deref()) {
        ProgramHealth::Resolves(program)
    } else {
        ProgramHealth::Missing(program)
    }
}

/// The full argv a unit will run: launchd's `ProgramArguments` array, or the
/// tokens of a systemd `ExecStart=` line.
fn unit_argv(text: &str) -> Option<Vec<String>> {
    plist_program_arguments(text).or_else(|| systemd_exec_start_argv(text))
}

/// The binary whose availability actually decides whether the timer can fire.
///
/// Units are written as `/bin/sh -c "exec ainb ..."`, so `argv[0]` is the shell
/// and tells us nothing; the interesting name is the first word of the script.
/// Units written before that change (`ainb ...` directly) are still understood,
/// so a status call against an old unit reports on the right binary.
fn program_from_argv(argv: &[String]) -> Option<String> {
    let first = argv.first()?;
    if is_shell(first) {
        if let Some(script) = argv.iter().skip(1).find(|a| !a.starts_with('-')) {
            let tokens = shell_split(script);
            let head = tokens.iter().find(|t| *t != "exec")?;
            return Some(head.clone());
        }
    }
    Some(first.clone())
}

fn is_shell(program: &str) -> bool {
    matches!(
        std::path::Path::new(program).file_name().and_then(|n| n.to_str()),
        Some("sh" | "bash" | "zsh" | "dash")
    )
}

/// The `PATH` the unit itself carries, which is what the shell resolves a bare
/// program against. NOT the `PATH` of whoever is running `atc status`.
fn unit_search_path(text: &str) -> Option<String> {
    plist_string_after_key(text, "PATH").or_else(|| systemd_environment_path(text))
}

/// Whether a unit's program resolves to something executable. An explicit path
/// is checked directly; a bare name is looked up under `search_path` (the
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

/// Every `<string>` inside the `ProgramArguments` array of a launchd plist.
fn plist_program_arguments(xml: &str) -> Option<Vec<String>> {
    let after_key = xml.split_once("<key>ProgramArguments</key>")?.1;
    let (array, _) = after_key.split_once("<array>")?.1.split_once("</array>")?;
    let argv: Vec<String> = array
        .match_indices("<string>")
        .filter_map(|(i, _)| {
            let rest = &array[i + "<string>".len()..];
            let (value, _) = rest.split_once("</string>")?;
            Some(xml_unescape(value.trim()))
        })
        .collect();
    (!argv.is_empty()).then_some(argv)
}

/// The `<string>` value following `<key>{key}</key>` in a launchd plist.
fn plist_string_after_key(xml: &str, key: &str) -> Option<String> {
    let after = xml.split_once(&format!("<key>{key}</key>"))?.1;
    let open = after.split_once("<string>")?.1;
    let (value, _) = open.split_once("</string>")?;
    let value = xml_unescape(value.trim());
    (!value.is_empty()).then_some(value)
}

/// Tokens of the `ExecStart=` line of a systemd service unit.
fn systemd_exec_start_argv(unit: &str) -> Option<Vec<String>> {
    let line = unit.lines().map(str::trim).find_map(|l| l.strip_prefix("ExecStart="))?;
    let argv = shell_split(line.trim());
    (!argv.is_empty()).then_some(argv)
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
    let paths = unit_paths(&meta.name)?;
    let plist = paths.first().context("no launchd unit path")?.clone();
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent).context("creating LaunchAgents dir")?;
    }
    // Unload any prior version first so the reload picks up changes.
    if activation_enabled() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &plist.display().to_string()])
            .output();
    }
    std::fs::write(&plist, build_plist(meta)).context("writing launchd plist")?;
    if activation_enabled() {
        let _ = std::process::Command::new("launchctl")
            .args(["load", &plist.display().to_string()])
            .output();
    }
    Ok(vec![plist])
}

fn teardown_launchd(name: &str) -> Result<Vec<PathBuf>> {
    let plist = launchd_plist_path(name)?;
    let mut removed = Vec::new();
    if plist.exists() {
        // Deliberately NOT gated on `activation_enabled()`. Skipping the load
        // is safe (nothing starts), but skipping the UNLOAD while deleting the
        // file strands a loaded job with no unit left to unload it with, and
        // the heartbeat keeps firing with no supported way to stop it. The
        // shell-out is already best effort, so attempting it costs nothing.
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
    let paths = unit_paths(&meta.name)?;
    let service = paths.first().context("no systemd service path")?.clone();
    let timer = paths.get(1).context("no systemd timer path")?.clone();
    std::fs::write(&service, build_systemd_service(meta)).context("writing systemd service")?;
    std::fs::write(&timer, build_systemd_timer(meta)).context("writing systemd timer")?;
    let stem = unit_stem(&meta.name);
    if activation_enabled() {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output();
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", &format!("{stem}.timer")])
            .output();
    }
    Ok(vec![service, timer])
}

fn teardown_systemd(name: &str) -> Result<Vec<PathBuf>> {
    let stem = unit_stem(name);
    // Deliberately NOT gated on `activation_enabled()`, for the same reason as
    // the launchd path: deleting the unit while leaving the timer enabled
    // strands a firing job with nothing left to disable it with.
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
    if activation_enabled() {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output();
    }
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

/// Inverse of `shell_quote`: split a command line into argv.
///
/// Must handle `shell_quote`'s `'\''` escaping, so a path containing an
/// apostrophe round-trips instead of being truncated at the quote.
fn shell_split(s: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut in_quotes = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            '\\' if !in_quotes => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                    has_token = true;
                }
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    argv.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        argv.push(current);
    }
    argv
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

    #[test]
    fn program_from_argv_sees_through_the_shell_wrapper() {
        let argv = heartbeat_argv_with("ainb", "tower");
        assert_eq!(program_from_argv(&argv).as_deref(), Some("ainb"));

        let overridden = heartbeat_argv_with("/opt/custom/ainb", "tower");
        assert_eq!(
            program_from_argv(&overridden).as_deref(),
            Some("/opt/custom/ainb")
        );
    }

    /// Units written before the shell wrapper existed must still be judged on
    /// the right binary, not silently reported as unrecognised.
    #[test]
    fn program_from_argv_understands_legacy_direct_units() {
        let legacy: Vec<String> = ["/Users/old/.cargo/bin/ainb", "fleet", "atc", "heartbeat"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(
            program_from_argv(&legacy).as_deref(),
            Some("/Users/old/.cargo/bin/ainb")
        );
    }

    /// `shell_split` must be the true inverse of `shell_quote`, including its
    /// `'\''` escaping, or a path with an apostrophe reports as MISSING while
    /// the timer is firing perfectly well.
    #[test]
    fn shell_split_round_trips_shell_quote() {
        for original in [
            "/home/obrien/bin/ainb",
            "/home/o'brien/bin/ainb",
            "/opt/my apps/ainb",
            "plain",
        ] {
            let quoted = shell_quote(original);
            assert_eq!(
                shell_split(&quoted),
                vec![original.to_string()],
                "round trip failed for {original}"
            );
        }
        assert_eq!(
            shell_split("exec ainb fleet atc heartbeat tower"),
            vec!["exec", "ainb", "fleet", "atc", "heartbeat", "tower"]
        );
    }

    /// The apostrophe case end to end, through both unit flavours.
    #[test]
    fn a_path_with_an_apostrophe_survives_the_round_trip() {
        let bin = "/home/o'brien/bin/ainb";
        for unit in [plist_for(bin, "/usr/bin"), service_for(bin, "/usr/bin")] {
            let argv = unit_argv(&unit).expect("argv");
            assert_eq!(
                program_from_argv(&argv).as_deref(),
                Some(bin),
                "apostrophe path mangled in: {unit}"
            );
        }
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
    /// shell cannot exec either, so neither may pass.
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

    /// A bare program must be judged against the PATH the UNIT carries, which
    /// is what the shell will use, not against whatever `$PATH` the operator
    /// happens to be running `atc status` with.
    #[test]
    fn bare_program_resolves_against_the_units_own_path() {
        assert!(program_resolves_in("sh", Some("/bin:/usr/bin")));
        assert!(!program_resolves_in("sh", Some("/nonexistent/bin")));
    }

    #[test]
    fn units_carry_the_path_their_program_needs() {
        let path = "/opt/homebrew/bin:/usr/bin:/bin";
        let service = service_for("ainb", path);
        assert!(
            service.contains(&format!("Environment=\"PATH={path}\"")),
            "systemd service carries no PATH: {service}"
        );
        assert_eq!(unit_search_path(&service).as_deref(), Some(path));
        assert_eq!(
            unit_search_path(&plist_for("ainb", path)).as_deref(),
            Some(path)
        );
        assert_eq!(unit_search_path("not a unit at all"), None);
    }

    /// The status-line decision, over whole units in both flavours: a unit
    /// whose program has moved is flagged, a healthy one is not.
    #[test]
    fn status_flags_a_unit_whose_program_is_missing() {
        let moved = "/Users/nobody/.cargo/bin/ainb";
        for unit in [plist_for(moved, "/usr/bin"), service_for(moved, "/usr/bin")] {
            assert_eq!(
                unit_program_health(&unit),
                ProgramHealth::Missing(moved.to_string()),
                "moved binary not flagged: {unit}"
            );
        }
        for unit in [
            plist_for("sh", "/bin:/usr/bin"),
            service_for("sh", "/bin:/usr/bin"),
        ] {
            assert_eq!(
                unit_program_health(&unit),
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
            unit_program_health(&plist_for("sh", "/nonexistent/bin")),
            ProgramHealth::Missing("sh".into())
        );
        assert_eq!(
            unit_program_health(&service_for("sh", "/nonexistent/bin")),
            ProgramHealth::Missing("sh".into())
        );
    }

    /// An unreadable or unrecognised unit is NOT healthy. Collapsing it into
    /// the healthy case is the original false positive, reintroduced.
    #[test]
    fn an_unparsable_unit_is_never_reported_healthy() {
        for junk in ["", "not a unit at all", "<plist><dict></dict></plist>"] {
            let health = unit_program_health(junk);
            assert!(
                matches!(health, ProgramHealth::Unreadable(_)),
                "junk unit reported as {health:?}"
            );
            assert!(
                health.problem().is_some(),
                "no problem surfaced for junk unit"
            );
            assert!(health.program().is_none());
        }
    }

    /// `repair` reports `changed` from this, so it has to be honest in all
    /// three states. An implementation that answered "a unit file exists"
    /// fails the identical-bytes case; a hardcoded `false` fails the absent and
    /// stale cases; a hardcoded `true` fails the identical-bytes case, which is
    /// what makes `result: repaired, changed: false` a claim worth printing.
    #[test]
    fn install_would_change_is_true_only_when_the_bytes_actually_differ() {
        let fresh = plist_for("ainb", "/new:/bin");

        // Nothing installed: install would create the unit.
        assert!(install_would_change_from(None, &fresh));

        // Byte-identical: install would rewrite the same bytes.
        assert!(!install_would_change_from(Some(&fresh), &fresh));

        // Stale PATH baked into the on-disk unit: exactly the repair case.
        assert!(install_would_change_from(
            Some(&plist_for("ainb", "/old:/bin")),
            &fresh
        ));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn unit_paths_names_the_launchd_plist() {
        let paths = unit_paths("x").expect("unit paths");
        assert_eq!(paths.len(), 1, "macOS installs one unit: {paths:?}");
        assert!(
            paths[0].ends_with("com.agentsinabox.atc.x.plist"),
            "unexpected plist path: {:?}",
            paths[0]
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn unit_paths_names_the_systemd_service_and_timer() {
        let paths = unit_paths("x").expect("unit paths");
        assert_eq!(paths.len(), 2, "systemd installs a pair: {paths:?}");
        assert!(
            paths[0].ends_with("com.agentsinabox.atc.x.service"),
            "unexpected service path: {:?}",
            paths[0]
        );
        assert!(
            paths[1].ends_with("com.agentsinabox.atc.x.timer"),
            "unexpected timer path: {:?}",
            paths[1]
        );
    }

    /// The pre-write gate `repair` refuses on. It must judge the unit that
    /// WOULD be written, not the one on disk.
    ///
    /// Asserting agreement with `install_would_be_unrunnable` proves nothing,
    /// because that function is defined as a match on this one: the two move
    /// together for every variant, so such a test passes even if this reads the
    /// wrong unit entirely. The load-bearing property is that it never consults
    /// the on-disk unit, and the cheapest way to pin that is an instance which
    /// HAS no unit on disk: `installed_program_health` must answer `NoUnit`
    /// there, so anything else proves the fresh unit was built in memory.
    #[test]
    fn fresh_program_health_ignores_the_unit_on_disk() {
        let meta = AtcMeta::new("ainb-timer-test-instance-that-is-never-installed");
        assert_eq!(
            installed_program_health(&meta.name),
            ProgramHealth::NoUnit,
            "premise: this instance must have no unit on disk"
        );
        let fresh = fresh_program_health(&meta);
        assert_ne!(
            fresh,
            ProgramHealth::NoUnit,
            "fresh_program_health read the on-disk unit instead of the one install would write"
        );
        // And it judges a real program: the built unit always names one.
        assert!(
            matches!(
                fresh,
                ProgramHealth::Resolves(_) | ProgramHealth::Missing(_)
            ),
            "expected a verdict on a built unit, got {fresh:?}"
        );
    }

    #[test]
    fn program_health_renders_a_status_note_only_when_something_is_wrong() {
        assert_eq!(ProgramHealth::NoUnit.problem(), None);
        assert_eq!(ProgramHealth::Resolves("ainb".into()).problem(), None);
        assert_eq!(
            ProgramHealth::Missing("/gone/ainb".into()).problem().as_deref(),
            Some("program MISSING (/gone/ainb)")
        );
        assert!(
            ProgramHealth::Unreadable("boom".into())
                .problem()
                .is_some_and(|p| p.contains("UNKNOWN"))
        );
    }
}
