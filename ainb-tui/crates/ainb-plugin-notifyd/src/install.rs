//! Install / uninstall / status for the `ainb-hooks` plugin.
//!
//! The bash hook script and Claude plugin manifest are embedded into
//! the `ainb-notifyd` binary at build time via `include_str!`. The
//! [`install`] verb extracts them to known on-disk locations and
//! wires Claude Code, Codex CLI, and GitHub Copilot CLI to call into `notify.sh`.
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
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::paths::Paths;

/// The bash hook script, baked into the binary.
const HOOK_SCRIPT: &str = include_str!("../../../../plugins/ainb-hooks/hooks/notify.sh");

/// The Claude plugin manifest, baked into the binary.
const CLAUDE_PLUGIN_JSON: &str =
    include_str!("../../../../plugins/ainb-hooks/.claude-plugin/plugin.json");

/// The Codex hooks.json merge template (with the `__AINB_HOOK_SCRIPT__`
/// placeholder that gets substituted at install time).
const CODEX_HOOKS_TEMPLATE: &str = include_str!("../../../../plugins/ainb-hooks/codex/hooks.json");

/// The Copilot drop-in template (with the `__AINB_HOOK_SCRIPT__`
/// placeholder substituted at install time). Unlike Codex, this is
/// written verbatim as a standalone file — Copilot loads + combines
/// every `*.json` in `~/.copilot/hooks/`, so ainb owns one file.
const COPILOT_HOOKS_TEMPLATE: &str =
    include_str!("../../../../plugins/ainb-hooks/copilot/hooks.json");

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
    /// GitHub Copilot CLI. Hooks are installed as a standalone drop-in at
    /// `~/.copilot/hooks/ainb.json`; included in `ALL`.
    Copilot,
    /// Catch-all for any agent written by a newer build that this binary
    /// doesn't know. Keeps `install.json` forward-compatible: an unknown
    /// variant no longer hard-fails `serde` deserialization — which used to
    /// reset the record to empty, nag the install prompt on every launch,
    /// and break the "Install" action with `parsing …/install.json`.
    #[serde(other)]
    Unknown,
}

impl Agent {
    /// All known agents this binary can install — used to derive `--all`.
    pub const ALL: &'static [Agent] = &[Agent::Claude, Agent::Codex, Agent::Copilot];

    /// Lowercase name for logs / status output.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Copilot => "copilot",
            Agent::Unknown => "unknown",
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
    /// Resolved path to ainb's standalone Copilot hooks drop-in
    /// (`~/.copilot/hooks/ainb.json`). Set on install; used by uninstall
    /// to remove only the managed file while leaving sibling drop-ins intact.
    #[serde(default)]
    pub copilot_hooks_json: Option<PathBuf>,
    /// The plugin manifest version that was written to disk at install
    /// time. Compared against [`embedded_plugin_version`] on startup to
    /// detect drift after an `ainb` upgrade. `None` for records written
    /// before this field existed (treated as "0.0.0" → always stale).
    #[serde(default)]
    pub plugin_version: Option<String>,
    /// Set when the user explicitly declined the first-run install
    /// prompt ("don't ask again"). Suppresses the offer-to-install
    /// prompt; does not affect the offer-to-update prompt (which only
    /// applies once something is actually installed).
    #[serde(default)]
    pub prompt_dismissed: bool,
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
        let text =
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        Ok(serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?)
    }

    /// Persist to disk under the standard path.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        let p = Self::path(paths);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, text).with_context(|| format!("writing {}", p.display()))?;
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
    std::fs::write(&dest, HOOK_SCRIPT).with_context(|| format!("writing {}", dest.display()))?;
    let mut perms = std::fs::metadata(&dest)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&dest, perms)?;
    Ok(dest)
}

/// Marketplace + plugin identifiers ainb-hooks is published under.
const CLAUDE_PLUGIN_REF: &str = "ainb-hooks@agents-in-a-box";
const CLAUDE_MARKETPLACE: &str = "agents-in-a-box";
/// GitHub source used to register the marketplace on machines that don't
/// already know it (e.g. a brew install with no repo checkout).
const CLAUDE_MARKETPLACE_SOURCE: &str = "stevengonsalvez/agents-in-a-box";

/// Outcome of registering the Claude plugin through the `claude` CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeRegister {
    /// `claude plugin install` succeeded, or the plugin was already installed.
    Registered,
    /// The `claude` CLI is not on `PATH` — Claude wiring was skipped.
    ClaudeCliMissing,
    /// The CLI ran but failed; carries a short single-line reason.
    Failed(String),
}

/// Report returned by [`install`]: the on-disk record, plus — when Claude
/// was among the requested agents — the outcome of registering its plugin
/// through the `claude` CLI (`None` when Claude was not requested).
#[derive(Debug, Clone)]
pub struct InstallReport {
    /// The persisted install record (canonical script, codex wiring, etc.).
    pub record: InstallRecord,
    /// Claude marketplace-registration outcome, if Claude was targeted.
    pub claude: Option<ClaudeRegister>,
}

/// First non-empty line of `stderr` (preferred) or `stdout`, trimmed and
/// length-capped — for a tidy one-line status message.
fn first_line(stderr: &[u8], stdout: &[u8]) -> String {
    let pick = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.chars().take(160).collect::<String>())
    };
    pick(stderr).or_else(|| pick(stdout)).unwrap_or_else(|| "unknown error".into())
}

/// Register the ainb-hooks plugin with Claude Code via its `claude plugin`
/// CLI. Modern Claude only loads plugins from a registered marketplace, so
/// dropping files under `~/.claude/plugins/` is inert — registration must
/// go through the CLI. Best-effort + non-interactive; never panics.
fn register_claude_plugin() -> ClaudeRegister {
    // Probe for the CLI by listing marketplaces. A spawn error means
    // `claude` is not on PATH.
    let list = match Command::new("claude").args(["plugin", "marketplace", "list"]).output() {
        Ok(o) => o,
        Err(_) => return ClaudeRegister::ClaudeCliMissing,
    };
    // Register the marketplace only when Claude doesn't already know it —
    // a local-directory source (a dev's repo checkout) must not be
    // clobbered by adding the GitHub one.
    let known = String::from_utf8_lossy(&list.stdout);
    if !known.contains(CLAUDE_MARKETPLACE) {
        let _ = Command::new("claude")
            .args(["plugin", "marketplace", "add", CLAUDE_MARKETPLACE_SOURCE])
            .output();
    }
    // Install (idempotent: an already-installed plugin counts as success).
    match Command::new("claude").args(["plugin", "install", CLAUDE_PLUGIN_REF]).output() {
        Ok(o) if o.status.success() => ClaudeRegister::Registered,
        Ok(o) => {
            let msg = first_line(&o.stderr, &o.stdout);
            if msg.to_lowercase().contains("already") {
                ClaudeRegister::Registered
            } else {
                ClaudeRegister::Failed(msg)
            }
        }
        Err(e) => ClaudeRegister::Failed(e.to_string()),
    }
}

/// Unregister the ainb-hooks plugin from Claude (mirror of
/// [`register_claude_plugin`]). Best-effort; ignores all errors.
fn unregister_claude_plugin() {
    let _ = Command::new("claude").args(["plugin", "uninstall", CLAUDE_PLUGIN_REF]).output();
}

/// Install for one or more agents under the user's real `$HOME`.
///
/// Does the file-based wiring (canonical hook script + Codex managed
/// block + install record) via [`install_under_home`], then — if Claude
/// was requested — registers the Claude plugin through the `claude` CLI
/// and reports the outcome. Idempotent.
pub fn install(paths: &Paths, agents: &[Agent]) -> Result<InstallReport> {
    let home = dirs::home_dir().context("resolving home dir")?;
    let record = install_under_home(paths, &home, agents)?;
    let claude = agents.contains(&Agent::Claude).then(register_claude_plugin);
    Ok(InstallReport { record, claude })
}

/// Install variant that takes an explicit `$HOME` root. Lets tests
/// stay isolated without mutating the process-wide environment.
pub fn install_under_home(paths: &Paths, home: &Path, agents: &[Agent]) -> Result<InstallRecord> {
    if agents.is_empty() {
        bail!("install: must specify at least one agent");
    }
    paths
        .ensure_base()
        .with_context(|| format!("creating {}", paths.base.display()))?;
    let hook_script = extract_hook_script(paths)?;

    let mut record = InstallRecord::load(paths)?;
    record.hook_script = hook_script.clone();
    // Stamp the manifest version we're writing so a later `ainb`
    // upgrade can detect drift (see `prompt_state`). A fresh install
    // also clears any prior "don't ask again" — the user is opting in.
    record.plugin_version = Some(embedded_plugin_version());
    record.prompt_dismissed = false;
    for agent in agents {
        match agent {
            Agent::Claude => {
                // Claude is wired through its plugin marketplace (see
                // `register_claude_plugin`, invoked by `install`), NOT by
                // dropping a plugin directory — modern Claude Code ignores
                // unregistered dirs. Here we only record the agent and
                // clear any legacy hand-dropped dir reference.
                record.claude_plugin_dir = None;
                push_unique(&mut record.agents, Agent::Claude);
            }
            Agent::Codex => {
                let hooks_json = install_codex(home, &hook_script)?;
                record.codex_hooks_json = Some(hooks_json);
                push_unique(&mut record.agents, Agent::Codex);
            }
            Agent::Copilot => {
                let hooks_json = install_copilot(home, &hook_script)?;
                record.copilot_hooks_json = Some(hooks_json);
                push_unique(&mut record.agents, Agent::Copilot);
            }
            // Unknown agents are owned by a newer build; skip without
            // disturbing an existing record entry.
            Agent::Unknown => {}
        }
    }
    record.save(paths)?;
    Ok(record)
}

/// The plugin manifest version baked into this binary — parsed from
/// the embedded `plugin.json`. Single source of truth: bump the
/// version in `plugins/ainb-hooks/.claude-plugin/plugin.json` and both
/// the install record and the drift check pick it up automatically.
pub fn embedded_plugin_version() -> String {
    serde_json::from_str::<serde_json::Value>(CLAUDE_PLUGIN_JSON)
        .ok()
        .and_then(|v| v.get("version").and_then(|s| s.as_str()).map(str::to_string))
        .unwrap_or_else(|| "0.0.0".to_string())
}

/// Parse a `major.minor.patch` string into a comparable tuple. Any
/// non-numeric suffix on a component (e.g. `-rc1`) is ignored; missing
/// components default to 0. Robust to the `v` prefix.
fn parse_semver(v: &str) -> (u64, u64, u64) {
    let mut parts = v.trim_start_matches('v').split('.').map(|p| {
        p.split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or("0")
            .parse::<u64>()
            .unwrap_or(0)
    });
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// What the TUI should do about the notification hooks on startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallPrompt {
    /// Nothing installed and the user hasn't declined — offer to
    /// install.
    OfferInstall,
    /// Installed, but this binary embeds a newer manifest — offer to
    /// re-install (drift after an `ainb` upgrade).
    OfferUpdate {
        /// Version currently on disk.
        installed: String,
        /// Version this binary would write.
        embedded: String,
    },
    /// Up to date, or the user declined — show nothing.
    None,
}

/// Decide whether the TUI should prompt the user about notification
/// hooks. This is the single entry point the host calls on startup;
/// it reads the install record and compares versions. Never errors —
/// a missing/corrupt record is treated as "not installed".
pub fn prompt_state(paths: &Paths) -> InstallPrompt {
    let record = InstallRecord::load(paths).unwrap_or_default();
    let embedded = embedded_plugin_version();
    if record.agents.is_empty() {
        if record.prompt_dismissed {
            InstallPrompt::None
        } else {
            InstallPrompt::OfferInstall
        }
    } else {
        let installed = record.plugin_version.clone().unwrap_or_else(|| "0.0.0".to_string());
        if parse_semver(&installed) < parse_semver(&embedded) {
            InstallPrompt::OfferUpdate {
                installed,
                embedded,
            }
        } else {
            InstallPrompt::None
        }
    }
}

/// Persist the user's "don't ask again" choice so the offer-to-install
/// prompt never fires again on this machine. Writes a record with no
/// agents and `prompt_dismissed = true`.
pub fn dismiss_prompt(paths: &Paths) -> Result<()> {
    let mut record = InstallRecord::load(paths).unwrap_or_default();
    record.prompt_dismissed = true;
    record.save(paths)
}

fn push_unique<T: PartialEq>(v: &mut Vec<T>, item: T) {
    if !v.contains(&item) {
        v.push(item);
    }
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
    let our_block_text = CODEX_HOOKS_TEMPLATE.replace(
        "__AINB_HOOK_SCRIPT__",
        &hook_script_canonical.to_string_lossy(),
    );
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
    let mut merged_hooks =
        merged.get("hooks").and_then(|v| v.as_object()).cloned().unwrap_or_default();

    for (event, our_entries) in &our_hooks {
        // Drop any pre-existing ainb-managed entries for this event.
        let kept: Vec<serde_json::Value> = merged_hooks
            .get(event)
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter(|entry| !is_ainb_managed_entry(entry)).cloned().collect())
            .unwrap_or_default();
        let mut new_entries = kept;
        if let Some(arr) = our_entries.as_array() {
            for entry in arr {
                let mut tagged = entry.clone();
                if let Some(obj) = tagged.as_object_mut() {
                    obj.insert("_ainb_managed".to_string(), serde_json::Value::Bool(true));
                }
                new_entries.push(tagged);
            }
        }
        merged_hooks.insert(event.clone(), serde_json::Value::Array(new_entries));
    }
    merged.insert("hooks".to_string(), serde_json::Value::Object(merged_hooks));

    let text = serde_json::to_string_pretty(&serde_json::Value::Object(merged))?;
    std::fs::write(&hooks_json, text)
        .with_context(|| format!("writing {}", hooks_json.display()))?;
    info!(hooks_json = %hooks_json.display(), "installed codex hooks");
    Ok(hooks_json)
}

/// Path to the ainb-owned Copilot drop-in file under an explicit home
/// root. Copilot loads every `*.json` in `~/.copilot/hooks/` and
/// combines them, so ainb writes one standalone file here rather than
/// merging into a shared config (the Codex strategy). Parameterised on
/// `home` — never `dirs::home_dir()` — so the temp-HOME tests can
/// redirect it.
fn copilot_dropin(home: &Path) -> PathBuf {
    home.join(".copilot").join("hooks").join("ainb.json")
}

/// Install Copilot hooks: write our drop-in verbatim to
/// `<home>/.copilot/hooks/ainb.json`. Unlike Codex, this is a whole-file
/// write of a standalone drop-in — we never merge into a shared config,
/// and uninstall deletes just this one file (sibling drop-ins from the
/// user or other tools are untouched). Substitutes the
/// `__AINB_HOOK_SCRIPT__` placeholder with the canonical script path.
fn install_copilot(home: &Path, hook_script_canonical: &Path) -> Result<PathBuf> {
    let dropin = copilot_dropin(home);
    if let Some(parent) = dropin.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // JSON-encode the path to escape backslashes, quotes, etc., then strip
    // the surrounding `"…"` to get just the escaped interior for substitution.
    // Round-trip through serde_json afterwards to normalise formatting and
    // prove the final JSON is valid before it lands on disk.
    let path_json = serde_json::to_string(&hook_script_canonical.to_string_lossy())?;
    let path_escaped = path_json.trim_matches('"');
    let resolved = COPILOT_HOOKS_TEMPLATE.replace("__AINB_HOOK_SCRIPT__", path_escaped);
    let value: serde_json::Value = serde_json::from_str(&strip_line_comments(&resolved))
        .context("parsing embedded copilot hooks template")?;
    let text = serde_json::to_string_pretty(&value)?;
    std::fs::write(&dropin, text).with_context(|| format!("writing {}", dropin.display()))?;
    info!(dropin = %dropin.display(), "installed copilot hooks");
    Ok(dropin)
}

fn is_ainb_managed_entry(entry: &serde_json::Value) -> bool {
    entry.get("_ainb_managed").and_then(|v| v.as_bool()).unwrap_or(false)
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
                // Mirror of install: unregister the marketplace plugin via
                // the `claude` CLI (best-effort).
                unregister_claude_plugin();
                // Legacy cleanup: remove a hand-dropped plugin dir left by
                // older installs, if one is still recorded / present.
                if let Some(dir) = record.claude_plugin_dir.take() {
                    if dir.exists() {
                        std::fs::remove_dir_all(&dir).with_context(|| {
                            format!("removing legacy claude plugin dir {}", dir.display())
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
            Agent::Copilot => {
                // We own a whole standalone drop-in file, so uninstall
                // is a single-file delete — never `remove_dir_all`, which
                // would clobber sibling drop-ins from the user or other
                // tools in `~/.copilot/hooks/`.
                if let Some(dropin) = record.copilot_hooks_json.take() {
                    if dropin.exists() {
                        std::fs::remove_file(&dropin).with_context(|| {
                            format!("removing copilot drop-in {}", dropin.display())
                        })?;
                    }
                }
                record.agents.retain(|a| *a != Agent::Copilot);
            }
            // Unknown agents are owned by a newer build; leave record intact.
            Agent::Unknown => {}
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
    let mut value: serde_json::Value = serde_json::from_str(&strip_line_comments(&text))
        .with_context(|| format!("parsing {}", hooks_json.display()))?;
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
        hooks_obj.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(false));
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
    fn install_json_with_copilot_and_unknown_agents_parses() {
        // Regression: an install.json written by a newer, Copilot-capable
        // build (or any future agent) must not hard-fail deserialization.
        // A failed parse used to reset the record to empty -> re-offer the
        // install prompt on every launch, and break "Install" with
        // `parsing .../install.json`.
        let json = r#"{
            "agents": ["claude", "codex", "copilot", "gemini"],
            "hook_script": "/home/u/.agents-in-a-box/hooks/ainb-notify.sh",
            "codex_hooks_json": "/home/u/.codex/hooks.json",
            "copilot_hooks_json": "/home/u/.copilot/hooks/ainb.json",
            "plugin_version": "0.2.0",
            "prompt_dismissed": false
        }"#;
        let rec: InstallRecord = serde_json::from_str(json).expect("record must parse");
        assert!(rec.agents.contains(&Agent::Claude));
        assert!(rec.agents.contains(&Agent::Codex));
        assert!(rec.agents.contains(&Agent::Copilot));
        // The unknown "gemini" maps to the catch-all instead of erroring.
        assert!(rec.agents.contains(&Agent::Unknown));
        // Non-empty agents => prompt_state does NOT offer install every launch.
        assert!(!rec.agents.is_empty());
        // Copilot path is preserved on the record.
        assert_eq!(
            rec.copilot_hooks_json.as_deref(),
            Some(Path::new("/home/u/.copilot/hooks/ainb.json"))
        );
        // Copilot survives a save/load round-trip (no data loss).
        let reser = serde_json::to_string(&rec).unwrap();
        let again: InstallRecord = serde_json::from_str(&reser).unwrap();
        assert!(again.agents.contains(&Agent::Copilot));
    }

    #[test]
    fn extract_hook_script_writes_executable_file() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let dest = extract_hook_script(&p).unwrap();
        assert!(dest.exists());
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "script must be executable: mode={mode:o}"
        );
        let content = std::fs::read_to_string(&dest).unwrap();
        assert!(content.contains("ainb-hooks"));
    }

    #[test]
    fn install_under_home_records_claude_without_dropping_a_dir() {
        // Claude is wired via the marketplace CLI (done in `install`),
        // not by dropping a plugin dir — so install_under_home must record
        // the agent but leave `claude_plugin_dir` unset and create nothing
        // under ~/.claude/plugins/.
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let record = install_under_home(&p, dir.path(), &[Agent::Claude]).unwrap();
        assert!(record.agents.contains(&Agent::Claude));
        assert!(
            record.claude_plugin_dir.is_none(),
            "must not drop a plugin dir"
        );
        assert!(
            !dir.path().join(".claude/plugins/ainb-hooks").exists(),
            "no hand-dropped plugin dir should be created"
        );
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
        assert!(
            text.contains("AINB_AGENT=codex"),
            "managed block missing: {text}"
        );
        assert!(
            text.contains("notify.sh"),
            "managed block lacks script: {text}"
        );
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
    fn install_copilot_writes_native_dropin_file() {
        // `--copilot` writes a STANDALONE drop-in at
        // ~/.copilot/hooks/ainb.json in Copilot's native format: a flat
        // per-event array (NOT Codex's two-level {matcher, hooks:[]}),
        // camelCase events, top-level version:1, timeoutSec (not timeout),
        // and AINB_AGENT=copilot with the placeholder substituted away.
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let record = install_under_home(&p, dir.path(), &[Agent::Copilot]).unwrap();

        let dropin = dir.path().join(".copilot/hooks/ainb.json");
        assert_eq!(record.copilot_hooks_json.as_deref(), Some(dropin.as_path()));
        assert!(dropin.exists(), "drop-in file must exist at {}", dropin.display());

        let raw = std::fs::read_to_string(&dropin).unwrap();
        assert!(
            !raw.contains("__AINB_HOOK_SCRIPT__"),
            "placeholder survived: {raw}"
        );
        assert!(
            raw.contains(&record.hook_script.to_string_lossy().to_string()),
            "drop-in lacks resolved script path: {raw}"
        );

        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["version"], serde_json::json!(1), "missing version:1: {raw}");
        let hooks = v["hooks"].as_object().expect("hooks object");

        for event in ["notification", "agentStop"] {
            let arr = hooks
                .get(event)
                .and_then(|e| e.as_array())
                .unwrap_or_else(|| panic!("event {event} missing or not an array: {raw}"));
            assert_eq!(arr.len(), 1, "expected 1 entry for {event}: {raw}");
            let entry = &arr[0];
            // FLAT entry: type/command/timeoutSec live directly on the
            // entry (not nested under a `hooks` array with a `matcher`).
            assert_eq!(entry["type"], serde_json::json!("command"), "{raw}");
            assert!(
                entry.get("hooks").is_none(),
                "copilot entries must be flat, not nested {{matcher, hooks:[]}}: {raw}"
            );
            assert!(
                entry.get("matcher").is_none(),
                "copilot entries carry no matcher: {raw}"
            );
            assert_eq!(entry["timeoutSec"], serde_json::json!(5), "{raw}");
            assert!(
                entry.get("timeout").is_none(),
                "copilot uses timeoutSec, not timeout: {raw}"
            );
            let cmd = entry["command"].as_str().expect("command string");
            assert!(cmd.contains("AINB_AGENT=copilot"), "command not tagged for copilot: {cmd}");
            assert!(cmd.contains("notify.sh"), "command lacks script: {cmd}");
        }
        assert!(hooks.get("Notification").is_none(), "leaked Codex event: {raw}");
        assert!(hooks.get("Stop").is_none(), "leaked Codex event: {raw}");
    }

    #[test]
    fn uninstall_copilot_removes_dropin_only() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let hooks_dir = dir.path().join(".copilot/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let sibling = hooks_dir.join("other.json");
        std::fs::write(&sibling, r#"{"version":1,"hooks":{}}"#).unwrap();

        install_under_home(&p, dir.path(), &[Agent::Copilot]).unwrap();
        let dropin = hooks_dir.join("ainb.json");
        assert!(dropin.exists());
        assert!(sibling.exists(), "install must not touch sibling drop-ins");

        uninstall(&p, &[Agent::Copilot]).unwrap();
        assert!(!dropin.exists(), "our drop-in must be removed");
        assert!(sibling.exists(), "uninstall must not touch sibling drop-ins");
        assert!(hooks_dir.exists(), "uninstall must not remove the hooks dir");

        let record = InstallRecord::load(&p).unwrap();
        assert!(!record.agents.contains(&Agent::Copilot));
    }

    #[test]
    fn install_copilot_overwrites_its_own_dropin_idempotently() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        install_under_home(&p, dir.path(), &[Agent::Copilot]).unwrap();
        install_under_home(&p, dir.path(), &[Agent::Copilot]).unwrap();
        let dropin = dir.path().join(".copilot/hooks/ainb.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dropin).unwrap()).unwrap();
        assert_eq!(v["hooks"]["notification"].as_array().unwrap().len(), 1);
        assert_eq!(v["hooks"]["agentStop"].as_array().unwrap().len(), 1);
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
        assert!(names.contains(&"copilot"));
        for r in &rows {
            assert!(!r.installed);
            assert!(!r.socket_ok);
        }
    }

    #[test]
    fn embedded_plugin_version_is_parsed_from_manifest() {
        // Must match the version in
        // plugins/ainb-hooks/.claude-plugin/plugin.json.
        let v = embedded_plugin_version();
        assert!(!v.is_empty() && v != "0.0.0", "got {v:?}");
        // It parses as a semver tuple.
        assert!(parse_semver(&v) > (0, 0, 0));
    }

    #[test]
    fn parse_semver_handles_components_and_suffixes() {
        assert_eq!(parse_semver("0.2.0"), (0, 2, 0));
        assert_eq!(parse_semver("v1.2.3"), (1, 2, 3));
        assert_eq!(parse_semver("0.10.0"), (0, 10, 0));
        assert!(
            parse_semver("0.9.0") < parse_semver("0.10.0"),
            "numeric, not lexical"
        );
        assert_eq!(parse_semver("1.0.0-rc1"), (1, 0, 0));
        assert_eq!(parse_semver("garbage"), (0, 0, 0));
    }

    #[test]
    fn prompt_state_offers_install_when_nothing_installed() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        assert_eq!(prompt_state(&p), InstallPrompt::OfferInstall);
    }

    #[test]
    fn prompt_state_silent_after_dismiss() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        dismiss_prompt(&p).unwrap();
        assert_eq!(prompt_state(&p), InstallPrompt::None);
    }

    #[test]
    fn prompt_state_silent_when_up_to_date() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        let mut rec = InstallRecord::default();
        rec.agents.push(Agent::Claude);
        rec.plugin_version = Some(embedded_plugin_version());
        rec.save(&p).unwrap();
        assert_eq!(prompt_state(&p), InstallPrompt::None);
    }

    #[test]
    fn prompt_state_offers_update_on_version_drift() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        // Installed an older version than the binary embeds.
        let mut rec = InstallRecord::default();
        rec.agents.push(Agent::Claude);
        rec.plugin_version = Some("0.0.1".to_string());
        rec.save(&p).unwrap();
        match prompt_state(&p) {
            InstallPrompt::OfferUpdate {
                installed,
                embedded,
            } => {
                assert_eq!(installed, "0.0.1");
                assert_eq!(embedded, embedded_plugin_version());
            }
            other => panic!("expected OfferUpdate, got {other:?}"),
        }
    }

    #[test]
    fn prompt_state_treats_missing_version_as_stale() {
        // Records written before the version field existed parse with
        // plugin_version = None → treated as 0.0.0 → offer update.
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        std::fs::write(
            p.base.join("install.json"),
            r#"{"agents":["claude"],"hook_script":"/x/notify.sh"}"#,
        )
        .unwrap();
        assert!(matches!(
            prompt_state(&p),
            InstallPrompt::OfferUpdate { .. }
        ));
    }

    #[test]
    fn install_then_prompt_state_is_none() {
        // End-to-end: install stamps the current version, so an
        // immediate prompt_state is silent.
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        install_under_home(&p, dir.path(), &[Agent::Claude]).unwrap();
        assert_eq!(prompt_state(&p), InstallPrompt::None);
    }
}
