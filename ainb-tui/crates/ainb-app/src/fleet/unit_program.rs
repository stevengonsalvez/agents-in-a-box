// ABOUTME: The `ainb` binary a scheduler unit runs: how it is chosen when the
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

/// Directories a unit can always fall back on, whatever the installing shell
/// happened to have on `PATH`.
const STANDARD_BIN_DIRS: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";

/// Resolve the `ainb` binary to write into a unit's command.
///
/// Deliberately NOT `current_exe()`: that freezes an absolute path into the
/// unit forever, so a later `brew upgrade` (or cargo to homebrew move) leaves
/// the scheduler running a path that no longer exists. Bare `ainb` is
/// re-resolved at every launch. `$AINB_BIN` stays as the explicit override for
/// installs that are not on `PATH`.
///
/// A relative `$AINB_BIN` is made absolute against the installing process's
/// cwd, because the unit will NOT run with that cwd: both schedulers launch
/// from `/`, so `./target/release/ainb` written verbatim can never be found.
#[must_use]
pub fn ainb_bin_from(override_var: Option<&str>) -> String {
    match override_var {
        Some(b) if !b.is_empty() => absolutize_explicit_path(b),
        _ => "ainb".to_string(),
    }
}

/// Leave a bare name alone (the shell resolves it on `PATH`); anchor an
/// explicit but relative path to the cwd so it still means the same file once
/// the scheduler runs it from `/`.
fn absolutize_explicit_path(bin: &str) -> String {
    if !bin.contains('/') {
        return bin.to_string();
    }
    // A tilde reaches us unexpanded when it came from a config file or a
    // quoted assignment rather than the shell. Joining it to the cwd would
    // produce `/cwd/~/bin/ainb`, which cannot exist, so expand it here: we
    // shell-quote the result, so the shell will not expand it later either.
    if let Some(rest) = bin.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).display().to_string();
        }
    }
    let path = std::path::Path::new(bin);
    if path.is_absolute() {
        return bin.to_string();
    }
    std::path::absolute(path).map_or_else(|_| bin.to_string(), |p| p.display().to_string())
}

/// The `PATH` to write into a unit, and therefore the one the shell resolves a
/// bare program against at launch time.
///
/// The installing shell's `PATH` alone is not enough. It can carry entries that
/// vanish (direnv, nix, a `target/debug` used for one `cargo run`), and if the
/// unit inherits only those, the bridge dies the moment they go away: the #608
/// failure shape again, moved from the binary path to the `PATH`. So the
/// standard install directories are always appended as a floor.
/// Relative entries are dropped. The scheduler launches the unit from `/`, so
/// a `.` or `target/debug` inherited from the installing shell means something
/// different there than it did here, and keeping it would let the health check
/// resolve a binary the daemon can never reach. Dropping them also keeps a
/// project-local bin directory from deciding which `ainb` runs at every boot.
#[must_use]
pub fn unit_path_env() -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    let mut out: Vec<&str> = Vec::new();
    for dir in current.split(':').chain(STANDARD_BIN_DIRS.split(':')) {
        if dir.is_empty() || !std::path::Path::new(dir).is_absolute() {
            continue;
        }
        if !out.contains(&dir) {
            out.push(dir);
        }
    }
    out.join(":")
}

/// Wrap a command as `/bin/sh -c "exec <command>"`.
///
/// `argv[0]` of the resulting unit is the SHELL, not `ainb`, and that is the
/// point. Neither scheduler does the `PATH` lookup a naive reading suggests:
/// launchd resolves `ProgramArguments[0]` against its own job environment and
/// ignores the plist's `EnvironmentVariables` PATH entirely (a bare name there
/// fails to spawn before the process ever starts), and systemd resolves a
/// non-absolute `ExecStart` against a fixed compile-time list that excludes
/// `~/.cargo/bin` and `~/.local/bin`. Going through `/bin/sh -c` moves the
/// lookup somewhere that DOES honour the unit's `PATH`, and does it at every
/// launch rather than freezing it at install time.
///
/// `exec` so the shell replaces itself with `ainb` rather than lingering as a
/// parent, which also keeps launchd's `KeepAlive` watching the real process.
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

/// Health of the program the unit file at `unit` would run.
///
/// The single place that turns "a unit path" into a verdict, so the ATC timer
/// and the phone bridge cannot drift into reporting different health for the
/// same class of broken unit.
#[must_use]
pub fn unit_program_health_at(unit: &std::path::Path) -> ProgramHealth {
    if !unit.exists() {
        return ProgramHealth::NoUnit;
    }
    match std::fs::read_to_string(unit) {
        Ok(text) => unit_program_health(&text),
        Err(e) => ProgramHealth::Unreadable(format!("cannot read {}: {e}", unit.display())),
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
/// and always resolves, so reporting on it would certify every broken unit as
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
        // A relative path is judged against the CALLER's cwd by `which`, but
        // the scheduler launches from `/`, so it can never mean the same file.
        // Report it unresolvable rather than letting the verdict depend on
        // which directory the operator happened to run `status` from.
        if !std::path::Path::new(program).is_absolute() {
            return false;
        }
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
///
/// Both quote styles are honoured. We only ever emit `'`, but an installed
/// unit may predate that: earlier bridge code wrapped whitespace arguments in
/// `"`, and systemd honours `"` in `ExecStart` too. Misparsing one of those
/// makes `status` report a running service as MISSING.
fn shell_split(s: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            // `'` may open anywhere, because `shell_quote` emits `'\''` to
            // escape an apostrophe mid-token. `"` opens only at a token
            // boundary: we never emit it, and a legacy unit could carry one as
            // an ordinary character inside a path.
            '\'' if quote.is_none() => {
                quote = Some(c);
                has_token = true;
            }
            '"' if quote.is_none() && !has_token => {
                quote = Some(c);
                has_token = true;
            }
            c if quote == Some(c) => {
                quote = None;
                has_token = true;
            }
            '\\' if quote.is_none() => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                    has_token = true;
                }
            }
            c if c.is_whitespace() && quote.is_none() => {
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
    /// right binary. That is what makes an already-installed stale unit
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

    /// A bare program must be judged against the PATH the UNIT carries (the
    /// one the shell will use), not against whatever `$PATH` the operator
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

    /// An unparseable unit must not be reported as healthy. That is the same
    /// false positive the whole check exists to remove.
    #[test]
    fn an_unrecognised_unit_is_unknown_not_healthy() {
        for junk in ["", "this is not a unit", "<plist><dict></dict></plist>"] {
            let health = unit_program_health(junk);
            assert!(
                matches!(health, ProgramHealth::Unreadable(_)),
                "junk unit reported as {health:?}"
            );
            assert!(health.problem().unwrap().contains("UNKNOWN"));
            assert_eq!(health.program(), None);
        }
        assert!(
            ProgramHealth::Unreadable("boom".into())
                .problem()
                .is_some_and(|p| p.contains("UNKNOWN"))
        );
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

    /// The scheduler launches from `/`, so a relative `$AINB_BIN` written
    /// verbatim can never be found. Anchor it at install time instead.
    #[test]
    fn a_relative_binary_override_is_made_absolute() {
        let cwd = std::env::current_dir().expect("cwd");
        let resolved = ainb_bin_from(Some("./target/release/ainb"));
        assert!(
            std::path::Path::new(&resolved).is_absolute(),
            "relative override left relative: {resolved}"
        );
        assert!(resolved.starts_with(&cwd.display().to_string()));
        assert!(resolved.ends_with("target/release/ainb"));
        // A bare name is NOT a path; the shell resolves it on PATH.
        assert_eq!(ainb_bin_from(Some("ainb")), "ainb");
        assert_eq!(ainb_bin_from(Some("/opt/custom/ainb")), "/opt/custom/ainb");
    }

    /// A relative program in an already-installed unit must read as broken
    /// regardless of where `status` is run from, because the scheduler runs
    /// the unit from `/` and can never resolve it.
    #[test]
    fn a_relative_program_never_reports_healthy() {
        // `target/debug/ainb` may well exist relative to the test's cwd; the
        // point is that this must not make it count as resolvable.
        assert!(!program_resolves_in("target/debug/ainb", None));
        assert!(!program_resolves_in("./ainb", Some("/bin:/usr/bin")));
        // The absolute form is still judged on its merits.
        assert!(program_resolves_in("/bin/sh", None));
    }

    /// The unit's PATH must not be only what the installing shell happened to
    /// have: a direnv/nix/`target/debug` entry that later vanishes would
    /// strand the unit, which is #608 one level up.
    #[test]
    fn unit_path_always_includes_the_standard_dirs() {
        let path = unit_path_env();
        for dir in STANDARD_BIN_DIRS.split(':') {
            assert!(
                path.split(':').any(|d| d == dir),
                "unit PATH is missing the {dir} floor: {path}"
            );
        }
        // No duplicates, so the entry order stays meaningful.
        let dirs: Vec<&str> = path.split(':').collect();
        let mut unique = dirs.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(dirs.len(), unique.len(), "duplicate PATH entries: {path}");
    }

    /// Units written before this change double-quoted whitespace arguments,
    /// and systemd honours `"` too. Misparsing one reports a running service
    /// as MISSING and sends the operator to reinstall it for nothing.
    #[test]
    fn shell_split_handles_legacy_double_quoted_units() {
        assert_eq!(
            systemd_exec_start_argv(
                "[Service]\nExecStart=\"/opt/my apps/ainb\" fleet bridge run\n"
            ),
            Some(vec![
                "/opt/my apps/ainb".into(),
                "fleet".into(),
                "bridge".into(),
                "run".into()
            ])
        );
        assert_eq!(
            program_from_argv(
                &unit_argv("[Service]\nExecStart=\"/opt/my apps/ainb\" fleet bridge run\n")
                    .expect("argv")
            )
            .as_deref(),
            Some("/opt/my apps/ainb")
        );
    }

    /// An unexpanded tilde must not be glued to the cwd: `/cwd/~/bin/ainb`
    /// cannot exist, and we shell-quote the result so no later expansion saves
    /// it.
    #[test]
    fn a_tilde_override_is_expanded_not_prefixed() {
        let resolved = ainb_bin_from(Some("~/bin/ainb"));
        assert!(!resolved.contains('~'), "tilde survived: {resolved}");
        assert!(std::path::Path::new(&resolved).is_absolute());
        assert!(resolved.ends_with("bin/ainb"));
        if let Some(home) = dirs::home_dir() {
            assert!(resolved.starts_with(&home.display().to_string()));
        }
    }

    /// A relative PATH entry means something different from `/`, which is
    /// where the scheduler launches. Keeping it would let the health check
    /// resolve a binary the daemon can never reach, and would let a
    /// project-local bin dir decide which ainb runs at boot.
    #[test]
    fn unit_path_drops_relative_entries() {
        let path = unit_path_env();
        for dir in path.split(':') {
            assert!(
                std::path::Path::new(dir).is_absolute(),
                "relative entry {dir} survived into the unit PATH: {path}"
            );
        }
    }

    /// `"` opens a quote only at a token boundary. Legacy units double-quoted
    /// whitespace arguments, but a `"` inside a path was passed through raw,
    /// and `shell_quote` never emits one.
    #[test]
    fn a_literal_double_quote_inside_a_token_is_not_a_quote() {
        assert_eq!(
            shell_split("/opt/wi\"rd/ainb fleet bridge run"),
            vec!["/opt/wi\"rd/ainb", "fleet", "bridge", "run"]
        );
        // ...while a leading one still opens, which is what legacy units used.
        assert_eq!(
            shell_split("\"/opt/my apps/ainb\" fleet bridge run"),
            vec!["/opt/my apps/ainb", "fleet", "bridge", "run"]
        );
    }

    #[test]
    fn unit_program_health_at_separates_absent_from_unreadable() {
        let dir = std::env::temp_dir();
        let absent = dir.join("ainb-unit-program-test-absent.plist");
        let _ = std::fs::remove_file(&absent);
        assert_eq!(unit_program_health_at(&absent), ProgramHealth::NoUnit);

        let junk = dir.join("ainb-unit-program-test-junk.plist");
        std::fs::write(&junk, b"not a unit").expect("write fixture");
        assert!(matches!(
            unit_program_health_at(&junk),
            ProgramHealth::Unreadable(_)
        ));
        let _ = std::fs::remove_file(&junk);
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
