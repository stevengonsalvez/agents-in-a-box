//! Per-tool install-root resolution (three-tier).
//!
//! Precedence (highest first):
//!
//! 1. `$AINB_TOOL_HOME_<TOOL>` (tool name uppercased; hyphens map
//!    to underscores so `claude-desktop` becomes
//!    `AINB_TOOL_HOME_CLAUDE_DESKTOP`). Tests set this to a per-test
//!    tempdir; users can use it as a per-tool override.
//!
//! 2. `$AINB_USE_REAL_HOMES=1` → the tool's real config dir on disk,
//!    e.g. `~/.claude`, `~/.codex`, `~/.aws/amazonq`,
//!    `~/Library/Application Support/Claude` for claude-desktop on
//!    macOS. This is the **production opt-in** the spec gates behind
//!    a config flag; required after `toolkit/bootstrap.js` retired
//!    in P9 — without it, ainb only writes to the managed sandbox.
//!
//! 3. `$AINB_HOME/tools/<tool>` (default) — the managed sandbox.
//!    Keeps state isolated under the ainb home so accidental
//!    `ainb skill install` invocations don't trample the user's
//!    actual tool dirs.
//!
//! Pure function — never creates the directory.

use std::path::PathBuf;

/// Resolve the install root for `tool` using the precedence above.
pub fn install_root_for(tool: &str) -> PathBuf {
    let env_var = env_var_name(tool);
    if let Ok(p) = std::env::var(&env_var) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }

    if real_homes_enabled() {
        if let Some(real) = real_home_for(tool) {
            return real;
        }
    }

    ainb_skill_core::default_ainb_home().join("tools").join(tool)
}

/// Translate a tool name into its env-var override (e.g.
/// `claude-desktop` → `AINB_TOOL_HOME_CLAUDE_DESKTOP`). Hyphens
/// become underscores because POSIX shell `export` rejects names
/// with `-`.
pub fn env_var_name(tool: &str) -> String {
    format!(
        "AINB_TOOL_HOME_{}",
        tool.to_ascii_uppercase().replace('-', "_")
    )
}

fn real_homes_enabled() -> bool {
    matches!(
        std::env::var("AINB_USE_REAL_HOMES").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Best-effort per-tool real-home mapping. Returns `None` when the
/// home directory can't be determined (e.g. no `$HOME` set), in
/// which case the caller falls back to the managed sandbox.
fn real_home_for(tool: &str) -> Option<PathBuf> {
    let home = home_dir()?;
    let suffix: PathBuf = match tool {
        "claude" => PathBuf::from(".claude"),
        "codex" => PathBuf::from(".codex"),
        "copilot" => PathBuf::from(".copilot"),
        "gemini" => PathBuf::from(".gemini"),
        "cursor" => PathBuf::from(".cursor"),
        "amazonq" => PathBuf::from(".aws").join("amazonq"),
        "claude-desktop" => {
            #[cfg(target_os = "macos")]
            {
                PathBuf::from("Library").join("Application Support").join("Claude")
            }
            #[cfg(not(target_os = "macos"))]
            {
                PathBuf::from(".config").join("Claude")
            }
        }
        "cline" => PathBuf::from(".cline"),
        "roo" => PathBuf::from(".roo"),
        _ => return None,
    };
    Some(home.join(suffix))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises env mutation across the unit tests so parallel
    /// runs (cargo's default) don't trample each other.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn env_override_wins() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("AINB_TOOL_HOME_TESTTOOL", "/tmp/ainb-test-tool-root");
        let p = install_root_for("testtool");
        std::env::remove_var("AINB_TOOL_HOME_TESTTOOL");
        assert_eq!(p, PathBuf::from("/tmp/ainb-test-tool-root"));
    }

    #[test]
    fn default_resolves_under_ainb_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("AINB_TOOL_HOME_NOENVTOOL");
        std::env::remove_var("AINB_USE_REAL_HOMES");
        let p = install_root_for("noenvtool");
        assert!(p.ends_with("tools/noenvtool"), "got: {p:?}");
    }

    #[test]
    fn env_var_name_translates_hyphen_to_underscore() {
        assert_eq!(env_var_name("claude"), "AINB_TOOL_HOME_CLAUDE");
        assert_eq!(
            env_var_name("claude-desktop"),
            "AINB_TOOL_HOME_CLAUDE_DESKTOP"
        );
    }

    #[test]
    fn real_homes_opt_in_routes_to_real_dir() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HOME", "/tmp/fake-home-for-test");
        std::env::set_var("AINB_USE_REAL_HOMES", "1");
        std::env::remove_var("AINB_TOOL_HOME_CLAUDE");
        let p = install_root_for("claude");
        std::env::remove_var("AINB_USE_REAL_HOMES");
        assert_eq!(p, PathBuf::from("/tmp/fake-home-for-test").join(".claude"));
    }

    #[test]
    fn real_homes_amazonq_uses_aws_subdir() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HOME", "/tmp/fake-home-for-test");
        std::env::set_var("AINB_USE_REAL_HOMES", "1");
        std::env::remove_var("AINB_TOOL_HOME_AMAZONQ");
        let p = install_root_for("amazonq");
        std::env::remove_var("AINB_USE_REAL_HOMES");
        assert_eq!(
            p,
            PathBuf::from("/tmp/fake-home-for-test").join(".aws").join("amazonq")
        );
    }

    #[test]
    fn real_homes_for_unknown_tool_falls_back_to_managed() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("AINB_USE_REAL_HOMES", "1");
        std::env::remove_var("AINB_TOOL_HOME_UNKNOWN_TOOL_X");
        let p = install_root_for("unknown-tool-x");
        std::env::remove_var("AINB_USE_REAL_HOMES");
        assert!(p.ends_with("tools/unknown-tool-x"), "got: {p:?}");
    }
}
