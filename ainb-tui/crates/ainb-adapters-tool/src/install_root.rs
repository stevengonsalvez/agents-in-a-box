//! Per-tool install-root resolution (three-tier).
//!
//! Precedence (highest first):
//!
//! 1. `$AINB_TOOL_HOME_<TOOL>` (tool name uppercased; hyphens map
//!    to underscores so `claude-desktop` becomes
//!    `AINB_TOOL_HOME_CLAUDE_DESKTOP`). Tests set this to a per-test
//!    tempdir; users can use it as a per-tool override.
//!
//! 2. The tool's real config dir on disk (default), e.g. `~/.claude`,
//!    `~/.codex`, `~/.aws/amazonq`, `~/Library/Application
//!    Support/Claude` for claude-desktop on macOS. Installs land here
//!    so the deployed skill is actually visible to Claude/Codex — the
//!    same root discovery reads from.
//!
//! 3. `$AINB_HOME/tools/<tool>` — the managed sandbox, used only when
//!    `$HOME` is unresolvable or the tool has no known real dir.
//!
//! Pure function — never creates the directory.

use std::path::PathBuf;

/// Resolve the install root for `tool`.
///
/// Delegates to [`read_root_for`] so writes land in the tool's real
/// config dir by default — the same place discovery reads from. This
/// is what makes an installed skill visible to Claude/Codex instead of
/// stranded in the managed sandbox. Test fixtures still isolate via the
/// `$AINB_TOOL_HOME_<TOOL>` override, which takes precedence.
pub fn install_root_for(tool: &str) -> PathBuf {
    read_root_for(tool)
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

/// Resolve a per-tool path defaulting to the tool's real config dir.
/// Both the class-C discovery walker (read) and [`install_root_for`]
/// (write) route through this so the SkillManager sees, and installs
/// into, the user's actual `~/.claude/skills/...`.
///
/// Precedence:
///
/// 1. `$AINB_TOOL_HOME_<TOOL>` (preserved so test fixtures isolate
///    against tempdirs without env-var surgery).
/// 2. The tool's real config dir (`~/.claude`, `~/.codex`, …).
/// 3. `$AINB_HOME/tools/<tool>` only when `$HOME` is unresolvable or
///    the tool has no known real dir — pure safety fallback.
pub fn read_root_for(tool: &str) -> PathBuf {
    let env_var = env_var_name(tool);
    if let Ok(p) = std::env::var(&env_var) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }

    if let Some(real) = real_home_for(tool) {
        return real;
    }

    ainb_skill_core::default_ainb_home().join("tools").join(tool)
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
    fn unknown_tool_resolves_under_ainb_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("AINB_TOOL_HOME_NOENVTOOL");
        // Unknown tool has no real config dir → managed-sandbox fallback.
        let p = install_root_for("noenvtool");
        assert!(p.ends_with("tools/noenvtool"), "got: {p:?}");
    }

    #[test]
    fn install_defaults_to_real_home_without_env_gate() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HOME", "/tmp/fake-home-for-install");
        std::env::remove_var("AINB_USE_REAL_HOMES");
        std::env::remove_var("AINB_TOOL_HOME_CLAUDE");
        let p = install_root_for("claude");
        assert_eq!(
            p,
            PathBuf::from("/tmp/fake-home-for-install").join(".claude"),
            "installs must default to real home so skills are visible to Claude"
        );
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
    fn install_amazonq_uses_aws_subdir() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HOME", "/tmp/fake-home-for-test");
        std::env::remove_var("AINB_TOOL_HOME_AMAZONQ");
        let p = install_root_for("amazonq");
        assert_eq!(
            p,
            PathBuf::from("/tmp/fake-home-for-test").join(".aws").join("amazonq")
        );
    }

    #[test]
    fn install_unknown_tool_falls_back_to_managed() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("AINB_TOOL_HOME_UNKNOWN_TOOL_X");
        let p = install_root_for("unknown-tool-x");
        assert!(p.ends_with("tools/unknown-tool-x"), "got: {p:?}");
    }

    #[test]
    fn read_root_defaults_to_real_home_without_env_gate() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("HOME", "/tmp/fake-home-for-read-root");
        std::env::remove_var("AINB_USE_REAL_HOMES");
        std::env::remove_var("AINB_TOOL_HOME_CLAUDE");
        let p = read_root_for("claude");
        assert_eq!(
            p,
            PathBuf::from("/tmp/fake-home-for-read-root").join(".claude"),
            "read_root_for must default to real home — that's the whole point"
        );
    }

    #[test]
    fn read_root_env_override_still_wins() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("AINB_TOOL_HOME_CLAUDE", "/tmp/test-isolated-claude");
        std::env::set_var("HOME", "/tmp/fake-home-ignored");
        let p = read_root_for("claude");
        std::env::remove_var("AINB_TOOL_HOME_CLAUDE");
        assert_eq!(
            p,
            PathBuf::from("/tmp/test-isolated-claude"),
            "AINB_TOOL_HOME_<TOOL> must still override read_root_for so test fixtures isolate"
        );
    }
}
