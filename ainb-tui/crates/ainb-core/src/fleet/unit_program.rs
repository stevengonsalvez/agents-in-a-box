// ABOUTME: The `ainb` binary a scheduler unit runs — how it is chosen when the
// unit is written, and how it is checked when status is reported.
//
// Shared by every fleet feature that installs a launchd plist or a systemd user
// unit (the ATC heartbeat timer, the phone bridge daemon). All of them hit the
// same failure: freezing `current_exe()` into the unit pins a version-scoped
// path (`~/.cargo/bin/ainb`, `/opt/homebrew/Cellar/ainb/0.0.0-uninstalled/libexec/ainb`),
// the next upgrade deletes it, and the scheduler exits 78 `EX_CONFIG` into the
// penalty box while `status` keeps reporting the unit as installed because it
// only ever stat'd the unit FILE. See issue #608.
//
// Two halves, matching the two halves of the fix:
// - generation: [`ainb_bin_from`] + [`unit_path_env`] emit a bare `argv[0]`
//   resolved at fire time against the `PATH` the unit carries.
// - inspection: [`missing_program`] parses `argv[0]` back out of an installed
//   unit and says whether the scheduler could actually exec it.

use std::path::PathBuf;

/// Resolve the `ainb` binary to write as a unit's `argv[0]`.
///
/// Deliberately NOT `current_exe()`: that freezes an absolute path into the
/// unit forever, so a later `brew upgrade` (or cargo → homebrew move) leaves
/// the scheduler exec'ing a path that no longer exists. Bare `ainb`
/// re-resolves at every launch against the `PATH` the unit already carries.
/// `$AINB_BIN` stays as the explicit override for installs that are not on
/// `PATH`.
#[must_use]
pub fn ainb_bin_from(override_var: Option<&str>) -> String {
    match override_var {
        Some(b) if !b.is_empty() => b.to_string(),
        _ => "ainb".to_string(),
    }
}

/// The `PATH` to write into a unit, and therefore the one a bare `argv[0]` is
/// resolved against at launch time.
#[must_use]
pub fn unit_path_env() -> String {
    std::env::var("PATH")
        .unwrap_or_else(|_| "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".into())
}

/// Extract `argv[0]` from a unit's text — launchd plist XML or a systemd
/// service unit. Returns `None` when the shape is not recognised.
#[must_use]
pub fn unit_program(text: &str) -> Option<String> {
    plist_program(text).or_else(|| systemd_exec_start_program(text))
}

/// The `PATH` the unit itself carries, which is what the scheduler resolves a
/// bare `argv[0]` against — NOT the `PATH` of whoever is running `status`.
#[must_use]
pub fn unit_search_path(text: &str) -> Option<String> {
    plist_string_after_key(text, "PATH").or_else(|| systemd_environment_path(text))
}

/// `Some(program)` when the unit's `argv[0]` does not resolve to an executable
/// under the unit's own `PATH` — i.e. the unit is installed but can never run.
/// `None` when it resolves, or when the text carries no recognisable `argv[0]`.
#[must_use]
pub fn missing_program(unit_text: &str) -> Option<String> {
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
#[must_use]
pub fn program_resolves_in(program: &str, search_path: Option<&str>) -> bool {
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

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Recover `argv[0]` from an `ExecStart=` line we wrote ourselves. Both quote
/// styles are accepted because the fleet unit writers disagree: the ATC timer
/// shell-quotes with `'`, the bridge with `"`.
fn shell_unquote_first_token(s: &str) -> Option<String> {
    let s = s.trim_start();
    for quote in ['\'', '"'] {
        if let Some(rest) = s.strip_prefix(quote) {
            let (quoted, _) = rest.split_once(quote)?;
            return Some(quoted.to_string());
        }
    }
    let token = s.split_whitespace().next()?;
    (!token.is_empty()).then(|| token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ainb_bin_is_bare_unless_explicitly_overridden() {
        assert_eq!(ainb_bin_from(None), "ainb");
        assert_eq!(ainb_bin_from(Some("")), "ainb");
        assert_eq!(ainb_bin_from(Some("/opt/custom/ainb")), "/opt/custom/ainb");
    }

    #[test]
    fn unit_program_reads_argv0_from_both_unit_flavours() {
        assert_eq!(
            unit_program("<key>ProgramArguments</key>\n<array>\n\t<string>ainb</string>"),
            Some("ainb".into())
        );
        assert_eq!(
            unit_program("[Service]\nExecStart=ainb fleet bridge run\n"),
            Some("ainb".into())
        );
        assert_eq!(unit_program("not a unit at all"), None);
    }

    #[test]
    fn unit_program_survives_quoting_and_escaping() {
        assert_eq!(
            plist_program("<key>ProgramArguments</key>\n<array>\n<string>/a&amp;b/ainb</string>"),
            Some("/a&b/ainb".into())
        );
        // Both writers' quote styles: the ATC timer emits `'`, the bridge `"`.
        assert_eq!(
            systemd_exec_start_program("[Service]\nExecStart='/a b/ainb' fleet atc heartbeat x\n"),
            Some("/a b/ainb".into())
        );
        assert_eq!(
            systemd_exec_start_program("[Service]\nExecStart=\"/a b/ainb\" fleet bridge run\n"),
            Some("/a b/ainb".into())
        );
    }

    #[test]
    fn program_resolution_distinguishes_present_from_moved() {
        assert!(program_resolves_in("/bin/sh", None));
        assert!(!program_resolves_in(
            "/opt/homebrew/Cellar/ainb/0.0.0-uninstalled/libexec/ainb",
            None
        ));
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

        let not_executable = dir.join("ainb-unit-program-test-not-executable");
        std::fs::write(&not_executable, b"#!/bin/sh\n").expect("write fixture");
        assert!(!program_resolves_in(
            &not_executable.display().to_string(),
            None
        ));
        let _ = std::fs::remove_file(&not_executable);
    }

    /// A bare `argv[0]` must be judged against the PATH the UNIT carries — the
    /// one the scheduler will use — not against whatever `$PATH` the operator
    /// happens to be running `status` with.
    #[test]
    fn bare_program_resolves_against_the_units_own_path() {
        assert!(program_resolves_in("sh", Some("/bin:/usr/bin")));
        assert!(!program_resolves_in("sh", Some("/nonexistent/bin")));

        let unreachable =
            "[Service]\nEnvironment=\"PATH=/nonexistent/bin\"\nExecStart=sh fleet bridge run\n";
        assert_eq!(missing_program(unreachable), Some("sh".into()));
        let reachable =
            "[Service]\nEnvironment=\"PATH=/bin:/usr/bin\"\nExecStart=sh fleet bridge run\n";
        assert_eq!(missing_program(reachable), None);
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
