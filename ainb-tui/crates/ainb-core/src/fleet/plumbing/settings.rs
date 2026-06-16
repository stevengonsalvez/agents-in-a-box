// ABOUTME: Disk install/uninstall of the ATC lifecycle hooks into Claude's
// `~/.claude/settings.json`, using the pure merge in `hooks.rs`.
//
// This is the read-preserve-modify-write side that touches the filesystem:
// read the user's settings.json (preserving every existing hook — user,
// reflect, notifyd), merge in the ATC managed block (`hooks::merge_into`), and
// write it back atomically. Uninstall strips exactly the ATC block back out.
// Idempotent: re-running install yields byte-identical settings.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use super::atomic::write_atomic;
use super::hooks;

/// Path to Claude Code's user settings under an explicit `$HOME`.
#[must_use]
pub fn claude_settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

/// Install the ATC lifecycle hooks into `<home>/.claude/settings.json`, pointing
/// every managed event at `hook_script`. Preserves all existing hooks. Returns
/// the settings path written. Idempotent.
pub fn install_claude_hooks(home: &Path, hook_script: &Path) -> Result<PathBuf> {
    let path = claude_settings_path(home);
    let existing = read_settings(&path)?;
    let merged = hooks::merge_into(existing, &hook_script.to_string_lossy());
    let bytes = serde_json::to_vec_pretty(&merged).context("serializing settings.json")?;
    write_atomic(&path, &bytes)?;
    Ok(path)
}

/// Strip the ATC lifecycle hooks from `<home>/.claude/settings.json`, leaving
/// every other hook intact. No-op when the file is absent. Idempotent.
pub fn uninstall_claude_hooks(home: &Path) -> Result<()> {
    let path = claude_settings_path(home);
    if !path.exists() {
        return Ok(());
    }
    let existing = read_settings(&path)?;
    let stripped = hooks::strip_from(existing);
    let bytes = serde_json::to_vec_pretty(&stripped).context("serializing settings.json")?;
    write_atomic(&path, &bytes)
}

/// Read + parse settings.json, tolerating absence (→ empty object) and an empty
/// file. A genuinely malformed file is an error (we refuse to clobber it).
fn read_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn write_reflect_and_notifyd(home: &Path) {
        let path = claude_settings_path(home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let settings = json!({
            "hooks": {
                "Stop": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/hooks/stop_reflect.py" }
                    ]},
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "AINB_AGENT=claude /x/notify.sh" }
                    ]}
                ],
                "PreCompact": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/hooks/precompact_reflect.py --auto" }
                    ]}
                ]
            },
            "otherUserSetting": 42
        });
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
    }

    fn read(home: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(claude_settings_path(home)).unwrap()).unwrap()
    }

    #[test]
    fn install_preserves_reflect_notifyd_and_user_settings() {
        let home = TempDir::new().unwrap();
        write_reflect_and_notifyd(home.path());
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        let v = read(home.path());

        // Unrelated user setting survives.
        assert_eq!(v["otherUserSetting"], 42);
        // reflect + notifyd Stop hooks survive.
        let stop: Vec<String> = v["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(str::to_string))
            .collect();
        assert!(stop.iter().any(|c| c.contains("stop_reflect.py")));
        assert!(stop.iter().any(|c| c.contains("notify.sh")));
        // PreCompact (reflect-only, unmanaged) survives.
        assert!(v["hooks"]["PreCompact"].is_array());
        // ATC events added.
        assert!(v["hooks"]["SessionEnd"].is_array());
    }

    #[test]
    fn install_is_idempotent_on_disk() {
        let home = TempDir::new().unwrap();
        write_reflect_and_notifyd(home.path());
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        let first = std::fs::read_to_string(claude_settings_path(home.path())).unwrap();
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        let second = std::fs::read_to_string(claude_settings_path(home.path())).unwrap();
        assert_eq!(first, second, "re-install drifted the file");
    }

    #[test]
    fn install_then_uninstall_restores_non_atc_hooks() {
        let home = TempDir::new().unwrap();
        write_reflect_and_notifyd(home.path());
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        uninstall_claude_hooks(home.path()).unwrap();
        let v = read(home.path());
        // No ATC entries remain.
        for event in hooks::ATC_EVENTS {
            let atc = v["hooks"][event]
                .as_array()
                .map(|a| a.iter().filter(|e| hooks::is_atc_managed(e)).count())
                .unwrap_or(0);
            assert_eq!(atc, 0, "ATC entry survived uninstall on {event}");
        }
        // reflect + notifyd remain.
        let stop: Vec<String> = v["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(str::to_string))
            .collect();
        assert!(stop.iter().any(|c| c.contains("stop_reflect.py")));
        assert!(stop.iter().any(|c| c.contains("notify.sh")));
    }

    #[test]
    fn install_on_fresh_machine_creates_settings() {
        let home = TempDir::new().unwrap();
        let p = install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        assert!(p.exists());
        let v = read(home.path());
        assert!(v["hooks"]["Stop"].is_array());
    }

    #[test]
    fn uninstall_is_noop_when_settings_absent() {
        let home = TempDir::new().unwrap();
        uninstall_claude_hooks(home.path()).unwrap(); // must not error
    }
}
