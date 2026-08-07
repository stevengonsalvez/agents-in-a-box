// ABOUTME: The `ainb` binary a scheduler unit runs — how it is chosen when the
// unit is written, and how it is checked when status is reported.
//
// Shared by every fleet feature that installs a launchd plist or a systemd user
// unit (the ATC heartbeat timer, the phone bridge daemon). All of them hit the
// same failure: freezing `current_exe()` into the unit pins a version-scoped
// path (`~/.cargo/bin/ainb`, a `/opt/homebrew/Cellar/ainb/<version>/libexec`
// directory), the next upgrade deletes it, and the scheduler exits 78
// `EX_CONFIG` into the penalty box while `status` keeps reporting the unit as
// installed because it only ever stat'd the unit FILE. See issue #608.
//
// Two halves, matching the two halves of the fix:
// - generation: [`ainb_bin_from`] + [`unit_path_env`] + [`shell_wrapped_argv`]
//   emit a command whose binary is looked up at every launch.
// - inspection: [`unit_program_health`] parses the real program back out of an
//   installed unit and says whether the scheduler could actually run it.

use std::path::PathBuf;

/// The shell that performs the `PATH` lookup on behalf of the scheduler.
pub const SHELL: &str = "/bin/sh";

/// Resolve the `ainb` binary to write into a unit's command.
///
/// Deliberately NOT `current_exe()`: that freezes an absolute path into the
/// unit forever, so a later `brew upgrade` (or cargo → homebrew move) leaves
/// the scheduler running a path that no longer exists. Bare `ainb` is
/// re-resolved at every launch. `$AINB_BIN` stays as the explicit override for
/// installs that are not on `PATH`.
#[must_use]
pub fn ainb_bin_from(override_var: Option<&str>) -> String {
    match override_var {
        Some(b) if !b.is_empty() => b.to_string(),
        _ => "ainb".to_string(),
    }
}

/// The `PATH` to write into a unit, and therefore the one the shell resolves a
/// bare program against at launch time.
#[must_use]
pub fn unit_path_env() -> String {
    std::env::var("PATH")
        .unwrap_or_else(|_| "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".into())
}

/// Wrap a command as `/bin/sh -c "exec <command>"`.
///
/// `argv[0]` of the resulting unit is the SHELL, not `ainb`, and that is the
/// point. Neither scheduler does the `PATH` lookup a naive reading suggests:
/// launchd resolves `ProgramArguments[0]` against its own job environment and
/// ignores the plist's `EnvironmentVariables` PATH entirely — a bare name there
/// fails to spawn before the process ever starts — and systemd resolves a
/// non-absolute `ExecStart` against a fixed compile-time list that excludes
/// `~/.cargo/bin` and `~/.local/bin`. Going through `/bin/sh -c` moves the
/// lookup somewhere that DOES honour the unit's `PATH`, and does it at every
/// launch rather than freezing it at install time.
///
/// `exec` so the shell replaces itself with `ainb` rather than lingering as a
/// parent — which also keeps launchd's `KeepAlive` watching the real process.
#[must_use]
pub fn shell_wrapped_argv(bin: &str, args: &[&str]) -> Vec<String> {
    let command = std::iter::once(bin)
        .chain(args.iter().copied())
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ");
    vec![SHELL.into(), "-c".into(), format!("exec {command}")]
}

/// What a `status` command needs to know about the program an installed unit
/// will try to run. Every variant is distinct on purpose: an unreadable or
/// unrecognised unit is NOT the same as a healthy one, and reporting it as
/// healthy is the exact false positive this whole check exists to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramHealth {
    /// No unit installed.
    NoUnit,
    /// The unit's program resolves; the unit can run.
    Resolves(String),
    /// The unit's program does not resolve; the unit can never run.
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

/// Health of the program a unit's text describes.
#[must_use]
pub fn unit_program_health(text: &str) -> ProgramHealth {
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

/// The binary whose availability actually decides whether the unit can run.
///
/// Units are written as `/bin/sh -c "exec ainb …"`, so `argv[0]` is the shell
/// and always resolves — reporting on it would certify every broken unit as
/// healthy. Look through the wrapper to the command it runs. Units written
/// before the wrapper existed (a direct absolute path) still report on that
/// path, which is what makes an already-installed stale unit detectable.
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
/// program against. NOT the `PATH` of whoever is running the status command.
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
/// `$AINB_BIN` report healthy while the scheduler cannot run it.
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

/// The argv of a systemd service unit's `ExecStart=` line.
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

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Inverse of [`shell_quote`]: split a command line into argv.
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

/// Quote a token for a `/bin/sh -c` command line.
#[must_use]
pub fn shell_quote(s: &str) -> String {
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

    /// A Cellar path for a version that cannot be installed, so the "moved
    /// binary" assertions never depend on what this host happens to have.
    const GONE: &str = "/opt/homebrew/Cellar/ainb/0.0.0-uninstalled/libexec/ainb";

    #[test]
    fn ainb_bin_is_bare_unless_explicitly_overridden() {
        assert_eq!(ainb_bin_from(None), "ainb");
        assert_eq!(ainb_bin_from(Some("")), "ainb");
        assert_eq!(ainb_bin_from(Some("/opt/custom/ainb")), "/opt/custom/ainb");
    }

    /// launchd will not PATH-search `ProgramArguments[0]`, so the unit must
    /// hand it an absolute `/bin/sh` and let the shell do the lookup.
    #[test]
    fn shell_wrapper_puts_an_absolute_shell_at_argv0() {
        let argv = shell_wrapped_argv("ainb", &["fleet", "bridge", "run"]);
        assert_eq!(argv[0], "/bin/sh");
        assert_eq!(argv[1], "-c");
        assert_eq!(argv[2], "exec ainb fleet bridge run");
        assert!(
            std::path::Path::new(&argv[0]).is_absolute(),
            "argv[0] must be absolute or launchd cannot spawn it"
        );
    }

    #[test]
    fn program_from_argv_sees_through_the_shell_wrapper() {
        let argv = shell_wrapped_argv("ainb", &["fleet", "bridge", "run"]);
        assert_eq!(program_from_argv(&argv).as_deref(), Some("ainb"));

        let overridden = shell_wrapped_argv("/opt/custom/ainb", &["fleet", "bridge", "run"]);
        assert_eq!(
            program_from_argv(&overridden).as_deref(),
            Some("/opt/custom/ainb")
        );
    }

    /// Reporting on the wrapper's `argv[0]` would certify every broken unit as
    /// healthy, since `/bin/sh` always resolves.
    #[test]
    fn shell_wrapper_does_not_mask_a_missing_binary() {
        let argv = shell_wrapped_argv(GONE, &["fleet", "bridge", "run"]);
        let program = program_from_argv(&argv).expect("program");
        assert_eq!(program, GONE);
        assert!(!program_resolves_in(&program, None));
    }

    /// Units written before the wrapper existed must still be judged on the
    /// right binary — that is what makes an already-installed stale unit
    /// detectable rather than silently unrecognised.
    #[test]
    fn program_from_argv_understands_legacy_direct_units() {
        let legacy: Vec<String> =
            [GONE, "fleet", "bridge", "run"].iter().map(|s| (*s).to_string()).collect();
        assert_eq!(program_from_argv(&legacy).as_deref(), Some(GONE));
    }

    /// `shell_split` must be the true inverse of `shell_quote`, including its
    /// `'\''` escaping, or a path with an apostrophe reports as MISSING while
    /// the service is running perfectly well.
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
            shell_split("exec ainb fleet bridge run"),
            vec!["exec", "ainb", "fleet", "bridge", "run"]
        );
    }

    #[test]
    fn unit_argv_reads_both_unit_flavours() {
        let plist = "<key>ProgramArguments</key>\n<array>\n\t<string>/bin/sh</string>\n\t\
                     <string>-c</string>\n\t<string>exec ainb fleet bridge run</string>\n</array>";
        assert_eq!(
            unit_argv(plist),
            Some(vec![
                "/bin/sh".into(),
                "-c".into(),
                "exec ainb fleet bridge run".into()
            ])
        );
        assert_eq!(
            unit_argv("[Service]\nExecStart=ainb fleet bridge run\n"),
            Some(vec![
                "ainb".into(),
                "fleet".into(),
                "bridge".into(),
                "run".into()
            ])
        );
        assert_eq!(unit_argv("not a unit at all"), None);
    }

    #[test]
    fn program_resolution_distinguishes_present_from_moved() {
        assert!(program_resolves_in("/bin/sh", None));
        assert!(!program_resolves_in(GONE, None));
        assert!(!program_resolves_in("", None));
        assert!(program_resolves_in("sh", None));
        assert!(!program_resolves_in(
            "ainb-definitely-not-a-real-binary",
            None
        ));
    }

    /// `exists()` would call a directory or a non-executable file healthy; the
    /// scheduler cannot run either, so neither may pass.
    #[test]
    fn program_resolution_requires_something_executable() {
        let dir = std::env::temp_dir();
        assert!(
            dir.exists(),
            "temp dir should exist for the premise to hold"
        );
        assert!(!program_resolves_in(&dir.display().to_string(), None));

        let not_executable = dir.join("ainb-unit-program-test-not-executable");
        std::fs::write(&not_executable, b"#!/bin/sh\n").expect("write fixture");
        assert!(!program_resolves_in(
            &not_executable.display().to_string(),
            None
        ));
        let _ = std::fs::remove_file(&not_executable);
    }

    /// A bare program must be judged against the PATH the UNIT carries — the
    /// one the shell will use — not against whatever `$PATH` the operator
    /// happens to be running the status command with.
    #[test]
    fn bare_program_resolves_against_the_units_own_path() {
        assert!(program_resolves_in("sh", Some("/bin:/usr/bin")));
        assert!(!program_resolves_in("sh", Some("/nonexistent/bin")));

        // `ls`, not `sh`: a shell at argv[0] is treated as a wrapper and
        // unwrapped, so it cannot stand in for an ordinary program here.
        let unreachable =
            "[Service]\nEnvironment=\"PATH=/nonexistent/bin\"\nExecStart=ls fleet bridge run\n";
        assert_eq!(
            unit_program_health(unreachable),
            ProgramHealth::Missing("ls".into())
        );
        let reachable =
            "[Service]\nEnvironment=\"PATH=/bin:/usr/bin\"\nExecStart=ls fleet bridge run\n";
        assert_eq!(
            unit_program_health(reachable),
            ProgramHealth::Resolves("ls".into())
        );
    }

    /// An unparseable unit must not be reported as healthy — that is the same
    /// false positive the whole check exists to remove.
    #[test]
    fn an_unrecognised_unit_is_unknown_not_healthy() {
        let health = unit_program_health("this is not a unit");
        assert!(matches!(health, ProgramHealth::Unreadable(_)));
        assert!(health.problem().unwrap().contains("UNKNOWN"));
        assert_eq!(health.program(), None);
    }

    #[test]
    fn health_reports_a_problem_only_when_there_is_one() {
        assert_eq!(ProgramHealth::NoUnit.problem(), None);
        assert_eq!(ProgramHealth::Resolves("ainb".into()).problem(), None);
        assert_eq!(
            ProgramHealth::Missing("/gone/ainb".into()).problem().as_deref(),
            Some("program MISSING (/gone/ainb)")
        );
    }

    #[test]
    fn unit_search_path_reads_both_unit_flavours() {
        assert_eq!(
            unit_search_path("<key>PATH</key>\n\t<string>/bin:/usr/bin</string>").as_deref(),
            Some("/bin:/usr/bin")
        );
        assert_eq!(
            unit_search_path("[Service]\nEnvironment=\"PATH=/bin\"\n").as_deref(),
            Some("/bin")
        );
        assert_eq!(unit_search_path("not a unit at all"), None);
    }
}
