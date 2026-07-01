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
use fs2::FileExt;
use serde_json::Value;

use super::atomic::write_atomic;
use super::hooks;

/// Path to Claude Code's user settings under an explicit `$HOME`.
#[must_use]
pub fn claude_settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

/// Path to the advisory lock guarding the settings.json read-merge-write window.
#[must_use]
pub fn settings_lock_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json.lock")
}

/// Acquire an exclusive advisory lock guarding the settings.json
/// read-merge-write window.
///
/// HONEST SCOPE (H2): this lock only serialises concurrent **ATC installs**
/// (`install_claude_hooks` / `uninstall_claude_hooks` racing each other). It is
/// NOT a cross-tool lock — `settings.json.lock` is referenced only here; reflect
/// and notifyd write `settings.json` WITHOUT taking it. So cross-tool safety
/// (an ATC install not clobbering a concurrent reflect/notifyd rewrite) relies
/// on those installs not overlapping in time, not on this lock. (A shared
/// cross-tool lock or a marketplace-path rearchitecture is a deferred follow-up;
/// do not assume this lock provides it.) Mirrors the inbox lock mechanism.
fn lock_settings(home: &Path) -> Result<std::fs::File> {
    let path = settings_lock_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating settings dir {}", parent.display()))?;
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .context("opening settings.json lock file")?;
    f.lock_exclusive().context("acquiring settings.json lock")?;
    Ok(f)
}

/// Install the ATC lifecycle hooks into `<home>/.claude/settings.json`, pointing
/// every managed event at `hook_script`. Preserves all existing hooks. Returns
/// the settings path written. Idempotent. The read-merge-write is performed
/// under an advisory lock that serialises concurrent ATC installs (NOT a
/// cross-tool lock — see [`lock_settings`]).
pub fn install_claude_hooks(home: &Path, hook_script: &Path) -> Result<PathBuf> {
    let _guard = lock_settings(home)?;
    let path = claude_settings_path(home);
    let existing = read_settings(&path)?;
    let merged = hooks::merge_into(existing, &hook_script.to_string_lossy());
    let bytes = serde_json::to_vec_pretty(&merged).context("serializing settings.json")?;
    write_atomic(&path, &bytes)?;
    Ok(path)
}

/// Strip the ATC lifecycle hooks from `<home>/.claude/settings.json`, leaving
/// every other hook intact. No-op when the file is absent. Idempotent. The
/// read-merge-write is performed under the same advisory lock as install.
pub fn uninstall_claude_hooks(home: &Path) -> Result<()> {
    let _guard = lock_settings(home)?;
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
        for (event, _matcher) in hooks::ATC_EVENTS {
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

    /// Regression: the read-merge-write window is guarded by an advisory lock so
    /// two concurrent ATC installs cannot silently drop each other's merge. (It
    /// is NOT a cross-tool lock — reflect/notifyd don't take it; see
    /// `lock_settings`.) We assert the lock file is created and used by install —
    /// the marker that the locked path was actually taken.
    #[test]
    fn install_takes_the_settings_lock() {
        let home = TempDir::new().unwrap();
        let lock = settings_lock_path(home.path());
        assert!(!lock.exists(), "lock file should not exist before install");
        install_claude_hooks(home.path(), Path::new("/x/notify.sh")).unwrap();
        assert!(
            lock.exists(),
            "install must create + use the settings.json lock"
        );
    }

    /// The settings lock serialises concurrent ATC installs: two threads racing
    /// `install_claude_hooks` against the same settings.json both complete and
    /// the result is a single coherent file with the ATC hooks present (no torn
    /// write, no lost merge). With the advisory lock the read-merge-write is
    /// mutually exclusive. (Cross-tool serialisation with reflect/notifyd is NOT
    /// provided — see `lock_settings`.)
    #[test]
    fn concurrent_installs_serialise_under_the_lock() {
        use std::sync::Arc;
        let home = Arc::new(TempDir::new().unwrap());
        write_reflect_and_notifyd(home.path());

        let mut handles = Vec::new();
        for _ in 0..8 {
            let h = home.clone();
            handles.push(std::thread::spawn(move || {
                install_claude_hooks(h.path(), Path::new("/x/notify.sh")).unwrap();
            }));
        }
        for j in handles {
            j.join().unwrap();
        }

        // The file is coherent: parses, preserves reflect + notifyd, and carries
        // the ATC hooks.
        let v = read(home.path());
        assert_eq!(v["otherUserSetting"], 42);
        assert!(v["hooks"]["SessionEnd"].is_array(), "ATC hooks present");
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
}
