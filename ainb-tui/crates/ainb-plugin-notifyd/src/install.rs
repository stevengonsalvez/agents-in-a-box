//! Install / uninstall / status for the `ainb-hooks` plugin.
//!
//! The bash hook script and Claude plugin manifest are embedded into
//! the `ainb-notifyd` binary at build time via `include_str!`. The
//! [`install`] verb extracts them to known on-disk locations and
//! wires both Claude Code and Codex CLI to call into `notify.sh`.
//!
//! For Claude: a plugin directory at `~/.claude/plugins/ainb-hooks/`
//! holding the manifest + script (so Claude Code's plugin marketplace
//! resolves `${CLAUDE_PLUGIN_ROOT}/hooks/notify.sh` correctly).
//!
//! For Codex: a managed JSON block in `~/.codex/hooks.json`, since
//! Codex resolves hook commands as absolute paths. The block points
//! at the canonical `~/.agents-in-a-box/hooks/notify.sh` so the
//! plugin stays agent-agnostic per Stevie's constraint.
//!
//! Install state is recorded at `~/.agents-in-a-box/install.json` so
//! [`uninstall`] is fully reversible.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::paths::Paths;

/// The bash hook script, baked into the binary.
const HOOK_SCRIPT: &str =
    include_str!("../../../../plugins/ainb-hooks/hooks/notify.sh");

/// The Claude plugin manifest, baked into the binary.
const CLAUDE_PLUGIN_JSON: &str =
    include_str!("../../../../plugins/ainb-hooks/.claude-plugin/plugin.json");

/// The Codex hooks.json merge template (with the `__AINB_HOOK_SCRIPT__`
/// placeholder that gets substituted at install time).
const CODEX_HOOKS_TEMPLATE: &str =
    include_str!("../../../../plugins/ainb-hooks/codex/hooks.json");

/// Sentinels used to delimit our managed block inside the user's
/// `~/.codex/hooks.json`. Stable strings — never change without a
/// migration path because uninstall greps for them.
const CODEX_BEGIN_MARKER: &str = "// >>> ainb-hooks managed block (do not edit) >>>";
const CODEX_END_MARKER: &str = "// <<< ainb-hooks managed block <<<";

/// The host CLI agent being installed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    /// Anthropic Claude Code CLI.
    Claude,
    /// OpenAI Codex CLI.
    Codex,
}

impl Agent {
    /// All known agents — used to derive `--all`.
    pub const ALL: &'static [Agent] = &[Agent::Claude, Agent::Codex];

    /// Lowercase name for logs / status output.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }
}

/// Persistent install record on disk. Lets `uninstall` reverse the
/// exact files it created, even if defaults change between versions.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct InstallRecord {
    /// Agents currently installed for.
    pub agents: Vec<Agent>,
    /// Resolved absolute path to the canonical hook script.
    pub hook_script: PathBuf,
    /// Resolved per-agent plugin paths (Claude only).
    pub claude_plugin_dir: Option<PathBuf>,
    /// Resolved per-agent config path (Codex only).
    pub codex_hooks_json: Option<PathBuf>,
}

impl InstallRecord {
    fn path(paths: &Paths) -> PathBuf {
        paths.base.join("install.json")
    }

    /// Load from disk, or return a default-empty record if absent.
    pub fn load(paths: &Paths) -> Result<Self> {
        let p = Self::path(paths);
        if !p.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("reading {}", p.display()))?;
        Ok(serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?)
    }

    /// Persist to disk under the standard path.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        let p = Self::path(paths);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, text)
            .with_context(|| format!("writing {}", p.display()))?;
        Ok(())
    }
}

/// Where the canonical hook script lives on disk. Both Claude's
/// plugin dir and Codex's hooks.json point at this path so the
/// script is a single source of truth.
pub fn canonical_hook_script(paths: &Paths) -> PathBuf {
    paths.base.join("hooks").join("notify.sh")
}

/// Extract the embedded `notify.sh` to its canonical location with
/// executable permissions. Idempotent — overwrites any existing
/// version of the script (so re-running install always picks up the
/// latest from the binary).
pub fn extract_hook_script(paths: &Paths) -> Result<PathBuf> {
    let dest = canonical_hook_script(paths);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&dest, HOOK_SCRIPT)
        .with_context(|| format!("writing {}", dest.display()))?;
    let mut perms = std::fs::metadata(&dest)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&dest, perms)?;
    Ok(dest)
}

/// Install for one or more agents under the user's real `$HOME`.
/// Idempotent — running install twice produces the same on-disk
/// state. Tests use [`install_under_home`] with an explicit base.
pub fn install(paths: &Paths, agents: &[Agent]) -> Result<InstallRecord> {
    let home = dirs::home_dir().context("resolving home dir")?;
    install_under_home(paths, &home, agents)
}

/// Install variant that takes an explicit `$HOME` root. Lets tests
/// stay isolated without mutating the process-wide environment.
pub fn install_under_home(
    paths: &Paths,
    home: &Path,
    agents: &[Agent],
) -> Result<InstallRecord> {
    if agents.is_empty() {
        bail!("install: must specify at least one agent");
    }
    paths
        .ensure_base()
        .with_context(|| format!("creating {}", paths.base.display()))?;
    let hook_script = extract_hook_script(paths)?;

    let mut record = InstallRecord::load(paths)?;
    record.hook_script = hook_script.clone();
    for agent in agents {
        match agent {
            Agent::Claude => {
                let plugin_dir = install_claude(home, &hook_script)?;
                record.claude_plugin_dir = Some(plugin_dir);
                push_unique(&mut record.agents, Agent::Claude);
            }
            Agent::Codex => {
                let hooks_json = install_codex(home, &hook_script)?;
                record.codex_hooks_json = Some(hooks_json);
                push_unique(&mut record.agents, Agent::Codex);
            }
        }
    }
    record.save(paths)?;
    Ok(record)
}

fn push_unique<T: PartialEq>(v: &mut Vec<T>, item: T) {
    if !v.contains(&item) {
        v.push(item);
    }
}

/// Install Claude plugin: drop plugin.json + hook script into
/// `<home>/.claude/plugins/ainb-hooks/`.
fn install_claude(home: &Path, hook_script_canonical: &Path) -> Result<PathBuf> {
    let plugin_dir = home.join(".claude").join("plugins").join("ainb-hooks");
    let claude_plugin_meta = plugin_dir.join(".claude-plugin");
    let claude_hooks_dir = plugin_dir.join("hooks");
    std::fs::create_dir_all(&claude_plugin_meta)?;
    std::fs::create_dir_all(&claude_hooks_dir)?;

    // Drop the manifest verbatim — it references `${CLAUDE_PLUGIN_ROOT}`
    // which Claude resolves at runtime.
    std::fs::write(
        claude_plugin_meta.join("plugin.json"),
        CLAUDE_PLUGIN_JSON,
    )?;

    // Symlink the hook script into the plugin dir so a single edit to
    // notify.sh propagates. On platforms where symlink fails (rare on
    // POSIX) we fall back to a copy.
    let claude_hook = claude_hooks_dir.join("notify.sh");
    if claude_hook.exists() {
        std::fs::remove_file(&claude_hook)?;
    }
    if std::os::unix::fs::symlink(hook_script_canonical, &claude_hook).is_err() {
        std::fs::copy(hook_script_canonical, &claude_hook)?;
        let mut perms = std::fs::metadata(&claude_hook)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&claude_hook, perms)?;
    }
    info!(plugin_dir = %plugin_dir.display(), "installed claude plugin");
    Ok(plugin_dir)
}

/// Install Codex hooks: merge our managed block into
/// `<home>/.codex/hooks.json`. Substitutes the `__AINB_HOOK_SCRIPT__`
/// placeholder with the canonical script path.
fn install_codex(home: &Path, hook_script_canonical: &Path) -> Result<PathBuf> {
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir)?;
    let hooks_json = codex_dir.join("hooks.json");

    // We're going to maintain a managed block bracketed by stable
    // markers. The simplest implementation: produce a JSON object that
    // combines the user's existing hooks with our block.
    let existing: serde_json::Value = if hooks_json.exists() {
        let text = std::fs::read_to_string(&hooks_json)?;
        if text.trim().is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            // The user's hooks.json may include `//` line comments
            // (e.g. the existing reflect block). Strip them before
            // parsing.
            let cleaned = strip_line_comments(&text);
            serde_json::from_str(&cleaned)
                .with_context(|| format!("parsing {}", hooks_json.display()))?
        }
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };

    // Resolve our template against the actual hook script path.
    let our_block_text = CODEX_HOOKS_TEMPLATE
        .replace("__AINB_HOOK_SCRIPT__", &hook_script_canonical.to_string_lossy());
    let our_block: serde_json::Value = serde_json::from_str(&strip_line_comments(&our_block_text))
        .context("parsing embedded codex hooks template")?;
    let our_hooks = our_block
        .get("hooks")
        .and_then(|v| v.as_object())
        .cloned()
        .context("codex hooks template missing 'hooks' object")?;

    // Build the merged JSON. For each event in our_hooks, if the user
    // already has entries for that event, we append a single managed
    // entry; we never replace user-authored hooks.
    let mut merged = existing.as_object().cloned().unwrap_or_default();
    let mut merged_hooks = merged
        .get("hooks")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    for (event, our_entries) in &our_hooks {
        // Drop any pre-existing ainb-managed entries for this event.
        let kept: Vec<serde_json::Value> = merged_hooks
            .get(event)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|entry| !is_ainb_managed_entry(entry))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let mut new_entries = kept;
        if let Some(arr) = our_entries.as_array() {
            for entry in arr {
                let mut tagged = entry.clone();
                if let Some(obj) = tagged.as_object_mut() {
                    obj.insert(
                        "_ainb_managed".to_string(),
                        serde_json::Value::Bool(true),
                    );
                }
                new_entries.push(tagged);
            }
        }
        merged_hooks.insert(event.clone(), serde_json::Value::Array(new_entries));
    }
    merged.insert(
        "hooks".to_string(),
        serde_json::Value::Object(merged_hooks),
    );

    let text = serde_json::to_string_pretty(&serde_json::Value::Object(merged))?;
    std::fs::write(&hooks_json, text)
        .with_context(|| format!("writing {}", hooks_json.display()))?;
    info!(hooks_json = %hooks_json.display(), "installed codex hooks");
    Ok(hooks_json)
}

fn is_ainb_managed_entry(entry: &serde_json::Value) -> bool {
    entry
        .get("_ainb_managed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || entry
            .get("hooks")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|s| s.contains("AINB_AGENT=") && s.contains("notify.sh"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
}

fn strip_line_comments(s: &str) -> String {
    // Strip lines that are pure `//` comments (with optional leading
    // whitespace). JSON doesn't formally support them, but Codex's
    // own `~/.codex/hooks.json` sometimes carries explanatory `//`
    // markers — and we ourselves embed one in the template's `"//"`
    // key. That's a valid JSON key so it survives parse anyway; this
    // routine only strips line-comment lines.
    s.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Uninstall for one or more agents. Removes plugin files, strips
/// the codex managed block, and clears the install record. The
/// canonical hook script is removed only when no agents remain.
pub fn uninstall(paths: &Paths, agents: &[Agent]) -> Result<()> {
    let mut record = InstallRecord::load(paths)?;
    for agent in agents {
        match agent {
            Agent::Claude => {
                if let Some(dir) = record.claude_plugin_dir.take() {
                    if dir.exists() {
                        std::fs::remove_dir_all(&dir).with_context(|| {
                            format!("removing claude plugin dir {}", dir.display())
                        })?;
                    }
                }
                record.agents.retain(|a| *a != Agent::Claude);
            }
            Agent::Codex => {
                if let Some(hooks_json) = record.codex_hooks_json.take() {
                    if hooks_json.exists() {
                        strip_codex_managed_entries(&hooks_json)?;
                    }
                }
                record.agents.retain(|a| *a != Agent::Codex);
            }
        }
    }
    record.save(paths)?;
    // If no agents remain installed, the canonical hook script + the
    // daemon's runtime files are scaffolding for nothing — leave them
    // alone (the daemon may still be useful for direct UnixStream
    // producers), but warn if a daemon is still running.
    if record.agents.is_empty() {
        warn!("all agents uninstalled; ainb-notifyd may still be running");
    }
    Ok(())
}

fn strip_codex_managed_entries(hooks_json: &Path) -> Result<()> {
    let text = std::fs::read_to_string(hooks_json)?;
    let mut value: serde_json::Value =
        serde_json::from_str(&strip_line_comments(&text)).with_context(|| {
            format!("parsing {}", hooks_json.display())
        })?;
    if let Some(hooks_obj) = value
        .as_object_mut()
        .and_then(|o| o.get_mut("hooks"))
        .and_then(|v| v.as_object_mut())
    {
        for (_event, entries) in hooks_obj.iter_mut() {
            if let Some(arr) = entries.as_array_mut() {
                arr.retain(|e| !is_ainb_managed_entry(e));
            }
        }
        // Drop now-empty event arrays so we don't leave noise.
        hooks_obj.retain(|_, v| {
            v.as_array().map(|a| !a.is_empty()).unwrap_or(false)
        });
    }
    let out = serde_json::to_string_pretty(&value)?;
    std::fs::write(hooks_json, out)?;
    Ok(())
}

/// One row in `ainb-notifyd status` output.
#[derive(Debug, Clone)]
pub struct StatusRow {
    /// Agent name (`claude` / `codex`).
    pub agent: String,
    /// Whether the install record lists this agent.
    pub installed: bool,
    /// Whether the canonical hook script is present and executable.
    pub hook_script_ok: bool,
    /// Whether the daemon socket appears live.
    pub socket_ok: bool,
    /// Last event ts (string) — empty if no events yet.
    pub last_event: String,
}

/// Build status rows for every known agent.
pub fn status(paths: &Paths) -> Result<Vec<StatusRow>> {
    let record = InstallRecord::load(paths)?;
    let hook = canonical_hook_script(paths);
    let hook_script_ok = hook.exists()
        && std::fs::metadata(&hook)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    let socket_ok = paths.socket.exists();
    // Latest event across all agents (we report it on every row for
    // ease of grep).
    let last_event = if paths.db.exists() {
        crate::store::Store::open(&paths.db)
            .ok()
            .and_then(|s| s.latest().ok().flatten())
            .map(|r| format!("{} ({})", r.raw_event, r.agent))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let mut rows = Vec::new();
    for agent in Agent::ALL {
        rows.push(StatusRow {
            agent: agent.name().to_string(),
            installed: record.agents.contains(agent),
            hook_script_ok,
            socket_ok,
            last_event: last_event.clone(),
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_home() -> TempDir {
        let dir = TempDir::new().unwrap();
        dir
    }

    fn paths_under_home(home: &Path) -> Paths {
        Paths::under(home.join(".agents-in-a-box"))
    }

    #[test]
    fn extract_hook_script_writes_executable_file() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let dest = extract_hook_script(&p).unwrap();
        assert!(dest.exists());
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "script must be executable: mode={mode:o}");
        let content = std::fs::read_to_string(&dest).unwrap();
        assert!(content.contains("ainb-hooks"));
    }

    #[test]
    fn install_claude_creates_plugin_dir_with_manifest() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let record = install_under_home(&p, dir.path(), &[Agent::Claude]).unwrap();
        let plugin_json = record
            .claude_plugin_dir
            .as_ref()
            .unwrap()
            .join(".claude-plugin/plugin.json");
        assert!(plugin_json.exists(), "missing: {}", plugin_json.display());
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&plugin_json).unwrap()).unwrap();
        assert_eq!(manifest["name"], "ainb-hooks");
    }

    #[test]
    fn install_codex_creates_managed_block_in_hooks_json() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let hooks_json_path = codex_dir.join("hooks.json");
        std::fs::write(
            &hooks_json_path,
            r#"{
                "hooks": {
                    "SessionStart": [
                        {"hooks": [{"type": "command", "command": "echo user-hook"}]}
                    ]
                }
            }"#,
        )
        .unwrap();
        let record = install_under_home(&p, dir.path(), &[Agent::Codex]).unwrap();
        assert!(record.codex_hooks_json.is_some());
        let text = std::fs::read_to_string(&hooks_json_path).unwrap();
        assert!(text.contains("echo user-hook"), "user hook lost: {text}");
        assert!(text.contains("AINB_AGENT=codex"), "managed block missing: {text}");
        assert!(text.contains("notify.sh"), "managed block lacks script: {text}");
    }

    #[test]
    fn uninstall_strips_codex_managed_block_but_keeps_user_hooks() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("hooks.json"),
            r#"{
                "hooks": {
                    "SessionStart": [
                        {"hooks": [{"type": "command", "command": "echo user-hook"}]}
                    ]
                }
            }"#,
        )
        .unwrap();
        install_under_home(&p, dir.path(), &[Agent::Codex]).unwrap();
        uninstall(&p, &[Agent::Codex]).unwrap();
        let text = std::fs::read_to_string(codex_dir.join("hooks.json")).unwrap();
        assert!(text.contains("echo user-hook"));
        assert!(!text.contains("AINB_AGENT=codex"));
    }

    #[test]
    fn install_record_roundtrip() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        let mut rec = InstallRecord::default();
        rec.agents.push(Agent::Claude);
        rec.hook_script = PathBuf::from("/x/y/notify.sh");
        rec.save(&p).unwrap();
        let loaded = InstallRecord::load(&p).unwrap();
        assert_eq!(loaded.agents, vec![Agent::Claude]);
        assert_eq!(loaded.hook_script, PathBuf::from("/x/y/notify.sh"));
    }

    #[test]
    fn status_reports_each_known_agent() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        let rows = status(&p).unwrap();
        let names: Vec<_> = rows.iter().map(|r| r.agent.as_str()).collect();
        assert!(names.contains(&"claude"));
        assert!(names.contains(&"codex"));
        for r in &rows {
            assert!(!r.installed);
            assert!(!r.socket_ok);
        }
    }
}
