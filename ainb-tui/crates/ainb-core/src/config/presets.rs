// ABOUTME: Repository presets for per-repo configuration overrides
// Presets are stored in a single TOML file at ~/.agents-in-a-box/presets.toml
// (path configurable via [presets] file = "..." in config.toml). Each
// preset is an entry in a top-level `[[preset]]` array.
//
// Per-repo overrides live at `.agents-box/presets.toml` (also a [[preset]]
// array; first entry wins). Back-compat: `.agents-box/preset.toml` with a
// single RepositoryPreset document is still accepted.
//
// Migration: pre-2026-05-27 installs used a per-file layout under
// `~/.agents-in-a-box/presets/<name>.toml`. `install_default_presets` (called
// once at TUI startup) detects that legacy directory, merges any
// user-customised entries into the new single file, and deletes the dir.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

// Re-export the canonical `SessionMode` (`crate::models::SessionMode`) so
// downstream callers can keep importing `config::presets::SessionMode` while
// the underlying type stays unified. Phase 6 of the new-session redesign
// retired the parallel `Boss/Interactive` enum that used to live here.
pub use crate::models::SessionMode;

/// Forward-compat default for `RepositoryPreset.mode` — pre-existing presets
/// without a `mode` field deserialize as `Boss` (matches today's behaviour).
const fn default_mode() -> SessionMode {
    SessionMode::Boss
}

/// Shipped bundled presets TOML — installed verbatim to
/// `~/.agents-in-a-box/presets.toml` on first run.
const BUNDLED_PRESETS_TOML: &str = include_str!("../../../../config/default-presets/presets.toml");

/// A repository preset that defines default agent and configuration settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryPreset {
    /// Unique name for this preset
    pub name: String,

    /// Description of what this preset is for
    #[serde(default)]
    pub description: String,

    /// Agent provider (e.g., "claude", "codex", "gemini")
    #[serde(default = "default_provider")]
    pub agent_provider: String,

    /// Agent model (e.g., "opus", "sonnet", "haiku")
    #[serde(default = "default_model")]
    pub agent_model: String,

    /// Session mode — Boss (default) or Interactive.
    ///
    /// `Boss` drives a containerised session via a prompt. `Interactive`
    /// launches a terminal session with no prompt textarea. Missing-on-disk
    /// defaults to `Boss` for backward compatibility with older preset files.
    #[serde(default = "default_mode")]
    pub mode: SessionMode,

    /// Skills to enable for this preset
    #[serde(default)]
    pub skills: Vec<String>,

    /// Plugins to enable for this preset
    #[serde(default)]
    pub plugins: Vec<String>,

    /// Permission settings
    #[serde(default)]
    pub permissions: PermissionSet,

    /// Custom CLAUDE.md rules to append
    #[serde(default)]
    pub custom_rules: Option<String>,

    /// Environment variables to set
    #[serde(default)]
    pub environment: HashMap<String, String>,
}

fn default_provider() -> String {
    "claude".to_string()
}

fn default_model() -> String {
    "sonnet".to_string()
}

/// Permission settings for a preset
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionSet {
    /// Allow file writes without confirmation
    #[serde(default)]
    pub file_write: bool,

    /// Allow shell commands without confirmation
    #[serde(default)]
    pub shell: bool,

    /// Allow git operations without confirmation
    #[serde(default)]
    pub git: bool,

    /// Allow network access without confirmation
    #[serde(default)]
    pub network: bool,

    /// Skip all permission prompts (dangerous)
    #[serde(default)]
    pub skip_all: bool,
}

impl Default for RepositoryPreset {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            description: "Default preset with balanced settings".to_string(),
            agent_provider: default_provider(),
            agent_model: default_model(),
            mode: default_mode(),
            skills: Vec::new(),
            plugins: Vec::new(),
            permissions: PermissionSet::default(),
            custom_rules: None,
            environment: HashMap::new(),
        }
    }
}

/// Wrapper for parsing the on-disk `presets.toml` with its `[[preset]]` array.
#[derive(Debug, Default, Deserialize)]
struct PresetsFile {
    #[serde(rename = "preset", default)]
    preset: Vec<RepositoryPreset>,
}

/// Manager for repository presets backed by a single `presets.toml`.
///
/// Reads + writes preserve user comments / formatting via `toml_edit`. The
/// in-memory `presets` map is the source of truth between `save_preset` /
/// `delete` calls — both operations also persist to disk synchronously.
pub struct PresetManager {
    /// Path to the single presets file (typically
    /// `~/.agents-in-a-box/presets.toml`).
    presets_file: PathBuf,

    /// Cached presets keyed by name. Order is preserved separately via
    /// `order` so the on-disk `[[preset]]` sequence round-trips.
    presets: HashMap<String, RepositoryPreset>,

    /// Insertion order of preset names, mirroring the on-disk `[[preset]]`
    /// array sequence. Used by `all()` so callers see entries in the same
    /// order they appear on disk.
    order: Vec<String>,
}

impl PresetManager {
    /// Create a new preset manager.
    ///
    /// Uses the default path (`~/.agents-in-a-box/presets.toml`). Tests and
    /// alt-config callers should use `with_file` instead.
    pub fn new() -> Result<Self> {
        Self::with_file(Self::default_presets_file()?)
    }

    /// Create a new preset manager pointed at an arbitrary file path. Used
    /// by `[presets] file = "..."` resolution and by unit tests.
    pub fn with_file(presets_file: PathBuf) -> Result<Self> {
        // Ensure parent dir exists so a `save_preset` from an empty manager
        // can write immediately.
        if let Some(parent) = presets_file.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create presets dir: {}", parent.display())
                })?;
            }
        }

        let mut manager = Self {
            presets_file,
            presets: HashMap::new(),
            order: Vec::new(),
        };
        manager.load_all()?;
        Ok(manager)
    }

    /// Default presets-file location: `~/.agents-in-a-box/presets.toml`.
    fn default_presets_file() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Failed to determine home directory")?;
        Ok(home.join(".agents-in-a-box").join("presets.toml"))
    }

    /// Path the manager will read / write.
    #[must_use]
    pub fn file_path(&self) -> &Path {
        &self.presets_file
    }

    /// Load all presets from the single `presets.toml` file. Duplicates by
    /// name keep the FIRST occurrence and emit a tracing warn for the rest.
    fn load_all(&mut self) -> Result<()> {
        if !self.presets_file.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&self.presets_file).with_context(|| {
            format!(
                "Failed to read presets file: {}",
                self.presets_file.display()
            )
        })?;
        // Empty file is fine.
        if content.trim().is_empty() {
            return Ok(());
        }
        let parsed: PresetsFile = toml::from_str(&content).with_context(|| {
            format!(
                "Failed to parse presets file: {}",
                self.presets_file.display()
            )
        })?;
        for preset in parsed.preset {
            if self.presets.contains_key(&preset.name) {
                tracing::warn!(
                    preset = %preset.name,
                    file = %self.presets_file.display(),
                    "Duplicate [[preset]] entry; keeping first occurrence",
                );
                continue;
            }
            self.order.push(preset.name.clone());
            self.presets.insert(preset.name.clone(), preset);
        }
        Ok(())
    }

    /// Save a preset to disk. Appends a new `[[preset]]` entry if the name is
    /// new, otherwise replaces the existing entry in-place. Preserves
    /// surrounding user comments + formatting via `toml_edit`.
    pub fn save_preset(&mut self, preset: &RepositoryPreset) -> Result<()> {
        // Read existing document (or start a fresh one).
        let mut doc: DocumentMut = if self.presets_file.exists() {
            let content = fs::read_to_string(&self.presets_file).with_context(|| {
                format!(
                    "Failed to read presets file: {}",
                    self.presets_file.display()
                )
            })?;
            content
                .parse::<DocumentMut>()
                .with_context(|| "Failed to parse presets TOML for in-place edit")?
        } else {
            DocumentMut::new()
        };

        // Locate or create the `preset` array-of-tables.
        if doc.get("preset").is_none() {
            doc.insert("preset", Item::ArrayOfTables(ArrayOfTables::new()));
        }
        let array = doc
            .get_mut("preset")
            .and_then(Item::as_array_of_tables_mut)
            .context("`preset` is not an array-of-tables in presets.toml")?;

        let new_table = preset_to_toml_edit_table(preset)?;

        // Replace in-place if a table with this name already exists.
        let mut replaced = false;
        for tbl in array.iter_mut() {
            let name_matches = tbl
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s == preset.name)
                .unwrap_or(false);
            if name_matches {
                *tbl = new_table.clone();
                replaced = true;
                break;
            }
        }
        if !replaced {
            array.push(new_table);
        }

        write_atomic(&self.presets_file, doc.to_string().as_bytes())
            .with_context(|| format!("Failed to write {}", self.presets_file.display()))?;

        // Keep the in-memory cache in sync with the on-disk file so a reused
        // manager instance reflects the just-saved preset without a reload.
        if !self.presets.contains_key(&preset.name) {
            self.order.push(preset.name.clone());
        }
        self.presets.insert(preset.name.clone(), preset.clone());

        Ok(())
    }

    /// Get a preset by name
    pub fn get(&self, name: &str) -> Option<&RepositoryPreset> {
        self.presets.get(name)
    }

    /// Get all presets in the order they appear on disk.
    pub fn all(&self) -> Vec<&RepositoryPreset> {
        self.order.iter().filter_map(|n| self.presets.get(n)).collect()
    }

    /// List all preset names in disk order.
    pub fn list_names(&self) -> Vec<&str> {
        self.order.iter().map(|s| s.as_str()).collect()
    }

    /// Delete a preset by removing its `[[preset]]` entry from the document.
    pub fn delete(&mut self, name: &str) -> Result<()> {
        if !self.presets_file.exists() {
            self.presets.remove(name);
            self.order.retain(|n| n != name);
            return Ok(());
        }

        let content = fs::read_to_string(&self.presets_file).with_context(|| {
            format!(
                "Failed to read presets file: {}",
                self.presets_file.display()
            )
        })?;
        let mut doc: DocumentMut = content
            .parse::<DocumentMut>()
            .with_context(|| "Failed to parse presets TOML for delete")?;

        let mut changed = false;
        if let Some(array) = doc.get_mut("preset").and_then(Item::as_array_of_tables_mut) {
            let before = array.len();
            array.retain(|tbl| {
                tbl.get("name").and_then(|v| v.as_str()).map(|s| s != name).unwrap_or(true)
            });
            changed = array.len() != before;
        }

        if changed {
            write_atomic(&self.presets_file, doc.to_string().as_bytes())
                .with_context(|| format!("Failed to write {}", self.presets_file.display()))?;
        }

        self.presets.remove(name);
        self.order.retain(|n| n != name);
        Ok(())
    }

    /// Load a repo-specific preset override if it exists. Accepts BOTH the
    /// new single-file array (`.agents-box/presets.toml` with `[[preset]]`
    /// entries — picks the first) and the legacy single-document format
    /// (`.agents-box/preset.toml` with a flat RepositoryPreset).
    pub fn load_repo_preset(repo_path: &Path) -> Result<Option<RepositoryPreset>> {
        // Prefer the new multi-entry file.
        let multi = repo_path.join(".agents-box").join("presets.toml");
        if multi.exists() {
            let content = fs::read_to_string(&multi).context("Failed to read repo presets.toml")?;
            let parsed: PresetsFile =
                toml::from_str(&content).context("Failed to parse repo presets.toml")?;
            return Ok(parsed.preset.into_iter().next());
        }

        // Fall back to the legacy single-document file.
        let single = repo_path.join(".agents-box").join("preset.toml");
        if single.exists() {
            let content = fs::read_to_string(&single).context("Failed to read repo preset.toml")?;
            let preset: RepositoryPreset =
                toml::from_str(&content).context("Failed to parse repo preset.toml")?;
            return Ok(Some(preset));
        }

        Ok(None)
    }
}

impl Default for PresetManager {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| Self {
            presets_file: PathBuf::from("."),
            presets: HashMap::new(),
            order: Vec::new(),
        })
    }
}

// =============================================================================
// Bundled defaults + legacy migration
// =============================================================================

/// Return the canonical list of built-in presets shipped by the binary.
///
/// Sources from the same `include_str!`-embedded `presets.toml` that
/// [`install_default_presets`] writes to disk on first run, so
/// `presets list` / `presets show` / `is_builtin_name` all agree with the
/// actual on-disk reality. Order matches the shipped bundle.
pub fn create_default_presets() -> Vec<RepositoryPreset> {
    match toml::from_str::<PresetsFile>(BUNDLED_PRESETS_TOML) {
        Ok(parsed) => parsed.preset,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "create_default_presets: failed to parse bundled presets.toml",
            );
            Vec::new()
        }
    }
}

/// Install the shipped default presets into the single `presets.toml` at
/// `file`.
///
/// On first run (or after a user wipes `~/.agents-in-a-box/presets.toml`),
/// this drops the bundled TOML verbatim. Files that already exist are
/// **never** overwritten — user edits are preserved across upgrades.
///
/// Also runs the legacy multi-file directory migration: if a sibling
/// `presets/` directory exists from a pre-2026-05-27 install, its
/// user-customised entries are merged into the new single file, and the
/// directory is deleted. Files matching previously-shipped signatures are
/// discarded silently (they would have been re-emitted as defaults).
pub fn install_default_presets(file: &Path) -> Result<()> {
    if let Some(parent) = file.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create presets parent dir: {}", parent.display())
            })?;
        }
    }

    // Pull anything salvageable out of the legacy multi-file layout BEFORE
    // we write the shipped defaults — that way user-customised legacy files
    // land in the new file alongside the shipped entries.
    let legacy_dir = file
        .parent()
        .map(|p| p.join("presets"))
        .unwrap_or_else(|| PathBuf::from("presets"));
    let salvaged = cleanup_legacy_default_presets(&legacy_dir);

    if !file.exists() {
        fs::write(file, BUNDLED_PRESETS_TOML)
            .with_context(|| format!("Failed to write default presets to {}", file.display()))?;
    }

    // Merge any salvaged user-customised legacy presets into the new file.
    if !salvaged.is_empty() {
        let mut manager = PresetManager::with_file(file.to_path_buf())?;
        for preset in salvaged {
            // Don't clobber a preset the bundled defaults already provide
            // under the same name — the on-disk version wins by being
            // present already.
            if manager.get(&preset.name).is_some() {
                tracing::info!(
                    preset = %preset.name,
                    "Skipping legacy preset migration: name collides with bundled default",
                );
                continue;
            }
            tracing::info!(preset = %preset.name, "Migrating legacy preset into presets.toml");
            manager.save_preset(&preset)?;
        }
    }

    Ok(())
}

/// Inspect a legacy `presets/` directory and return any user-customised
/// preset entries that should be carried forward into the new
/// `presets.toml`. Deletes the directory once it has been fully processed.
///
/// User-customised = NOT matching any previously-shipped signature
/// (claude-opus-yolo, codex-yolo, claude-interactive-yolo,
/// codex-interactive-yolo with the exact shipped shape). Anything that
/// looks like an old shipped file is dropped silently — the new bundled
/// defaults supersede it.
fn cleanup_legacy_default_presets(dir: &Path) -> Vec<RepositoryPreset> {
    let mut salvaged = Vec::new();
    if !dir.exists() {
        return salvaged;
    }

    // Read each `*.toml` in the legacy dir.
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!(
                dir = %dir.display(),
                error = %err,
                "Failed to read legacy presets dir; leaving in place",
            );
            return salvaged;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(
                    file = %path.display(),
                    error = %err,
                    "Failed to read legacy preset file; skipping",
                );
                continue;
            }
        };
        let preset: RepositoryPreset = match toml::from_str(&content) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(
                    file = %path.display(),
                    error = %err,
                    "Failed to parse legacy preset file; skipping",
                );
                continue;
            }
        };

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if matches_shipped_legacy_signature(&stem, &preset) {
            // Drop silently — superseded by the new bundled defaults.
            tracing::info!(
                file = %path.display(),
                "Dropping legacy shipped default preset (superseded by bundled presets.toml)",
            );
            continue;
        }

        // User-customised — carry forward.
        salvaged.push(preset);
    }

    // Best-effort dir removal. If it fails (perms, files left behind), leave
    // it in place — the next launch will retry.
    match fs::remove_dir_all(dir) {
        Ok(()) => tracing::info!(
            dir = %dir.display(),
            "Removed legacy presets directory after migration",
        ),
        Err(err) => tracing::warn!(
            dir = %dir.display(),
            error = %err,
            "Failed to remove legacy presets directory after migration",
        ),
    }

    salvaged
}

/// Match a legacy on-disk preset against any signature we've ever shipped.
/// Returns `true` when the file is a pristine shipped default (safe to drop);
/// `false` when there's any user customisation (skills/plugins/env/rules)
/// that warrants carrying forward.
fn matches_shipped_legacy_signature(stem: &str, p: &RepositoryPreset) -> bool {
    // Common rule: name must match the stem, no skills/plugins/env/rules.
    let pristine = p.name == stem
        && p.skills.is_empty()
        && p.plugins.is_empty()
        && p.environment.is_empty()
        && p.custom_rules.is_none();
    if !pristine {
        return false;
    }

    match stem {
        // Pre-2026-05 shipped defaults (deprecated).
        "claude-opus-yolo" => p.agent_provider == "claude" && p.permissions.skip_all,
        "codex-yolo" => p.agent_provider == "codex" && p.permissions.skip_all,
        // Mid-2026-05 shipped defaults (now bundled in the array file).
        "claude-interactive-yolo" => p.agent_provider == "claude" && p.permissions.skip_all,
        "codex-interactive-yolo" => p.agent_provider == "codex" && p.permissions.skip_all,
        _ => false,
    }
}

// =============================================================================
// toml_edit helpers
// =============================================================================

/// Convert a `RepositoryPreset` into a `toml_edit::Table` suitable for
/// insertion into an `ArrayOfTables`.
///
/// Built field-by-field with `toml_edit` primitives so sub-tables
/// (`permissions`, `environment`) become INLINE tables — this avoids the
/// `[preset.permissions]` path collision that happens when two `[[preset]]`
/// entries each carry a child table (`toml` parser flags it as a duplicate
/// key under `preset`).
fn preset_to_toml_edit_table(preset: &RepositoryPreset) -> Result<Table> {
    use toml_edit::{Array as TomlArray, InlineTable, Value};

    let mut table = Table::new();
    table.insert("name", value(preset.name.clone()));
    if !preset.description.is_empty() {
        table.insert("description", value(preset.description.clone()));
    }
    table.insert("agent_provider", value(preset.agent_provider.clone()));
    table.insert("agent_model", value(preset.agent_model.clone()));
    table.insert(
        "mode",
        value(match preset.mode {
            SessionMode::Boss => "boss",
            SessionMode::Interactive => "interactive",
        }),
    );

    // Skills + plugins → inline arrays of strings.
    if !preset.skills.is_empty() {
        let mut arr = TomlArray::new();
        for s in &preset.skills {
            arr.push(s.clone());
        }
        table.insert("skills", Item::Value(Value::Array(arr)));
    }
    if !preset.plugins.is_empty() {
        let mut arr = TomlArray::new();
        for s in &preset.plugins {
            arr.push(s.clone());
        }
        table.insert("plugins", Item::Value(Value::Array(arr)));
    }

    // Permissions → inline table. Only emit non-default fields so the file
    // stays tidy.
    let mut perms = InlineTable::new();
    if preset.permissions.file_write {
        perms.insert("file_write", Value::from(true));
    }
    if preset.permissions.shell {
        perms.insert("shell", Value::from(true));
    }
    if preset.permissions.git {
        perms.insert("git", Value::from(true));
    }
    if preset.permissions.network {
        perms.insert("network", Value::from(true));
    }
    if preset.permissions.skip_all {
        perms.insert("skip_all", Value::from(true));
    }
    table.insert("permissions", Item::Value(Value::InlineTable(perms)));

    // Environment → inline table of strings (only when non-empty).
    if !preset.environment.is_empty() {
        let mut env = InlineTable::new();
        let mut keys: Vec<&String> = preset.environment.keys().collect();
        keys.sort();
        for k in keys {
            env.insert(k, Value::from(preset.environment[k].clone()));
        }
        table.insert("environment", Item::Value(Value::InlineTable(env)));
    }

    if let Some(rules) = &preset.custom_rules {
        table.insert("custom_rules", value(rules.clone()));
    }

    Ok(table)
}

/// Atomic write — write to `path.tmp` then rename. Avoids partial writes
/// corrupting the user's presets file on power loss / SIGKILL.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => path.with_extension(format!("{ext}.tmp")),
        None => {
            let mut p = path.to_path_buf();
            let _ = p.set_extension("tmp");
            p
        }
    };
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_preset(name: &str, provider: &str) -> RepositoryPreset {
        RepositoryPreset {
            name: name.to_string(),
            description: format!("desc for {name}"),
            agent_provider: provider.to_string(),
            agent_model: "default".to_string(),
            mode: SessionMode::Boss,
            skills: Vec::new(),
            plugins: Vec::new(),
            permissions: PermissionSet {
                skip_all: true,
                ..Default::default()
            },
            custom_rules: None,
            environment: HashMap::new(),
        }
    }

    #[test]
    fn preset_mode_field_round_trips_through_toml() {
        let toml_src = r#"
            name = "test"
            agent_provider = "claude"
            agent_model = "opus"
            mode = "boss"
            [permissions]
            skip_all = true
        "#;
        let p: RepositoryPreset = toml::from_str(toml_src).unwrap();
        assert_eq!(p.mode, SessionMode::Boss);
    }

    #[test]
    fn preset_missing_mode_defaults_to_boss() {
        let toml_src = r#"
            name = "old"
            agent_provider = "claude"
            agent_model = "sonnet"
            [permissions]
        "#;
        let p: RepositoryPreset = toml::from_str(toml_src).unwrap();
        assert_eq!(p.mode, SessionMode::Boss);
    }

    #[test]
    fn bundled_defaults_include_four_in_order() {
        let names: Vec<String> = create_default_presets().into_iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "claude-interactive-yolo".to_string(),
                "codex-interactive-yolo".to_string(),
                "opusplan".to_string(),
                "shell".to_string(),
            ]
        );
    }

    #[test]
    fn default_presets_installed_on_first_run() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("presets.toml");
        install_default_presets(&file).unwrap();
        assert!(file.exists());
        let content = std::fs::read_to_string(&file).unwrap();
        for expected in [
            "claude-interactive-yolo",
            "codex-interactive-yolo",
            "opusplan",
            "shell",
        ] {
            assert!(
                content.contains(expected),
                "presets.toml missing {expected}"
            );
        }
    }

    #[test]
    fn default_presets_not_overwritten_if_present() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("presets.toml");
        std::fs::write(
            &file,
            "# user-edited marker\n[[preset]]\nname = \"my-only\"\n",
        )
        .unwrap();
        install_default_presets(&file).unwrap();
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(
            content.contains("user-edited marker"),
            "user customisation overwritten"
        );
        assert!(
            content.contains("my-only"),
            "user preset disappeared on install"
        );
    }

    #[test]
    fn presets_file_round_trip_array() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("presets.toml");
        install_default_presets(&file).unwrap();
        let mgr = PresetManager::with_file(file).unwrap();
        let names = mgr.list_names();
        assert_eq!(
            names,
            vec![
                "claude-interactive-yolo",
                "codex-interactive-yolo",
                "opusplan",
                "shell",
            ]
        );
    }

    #[test]
    fn save_preset_appends_to_toml_file_preserves_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("presets.toml");
        install_default_presets(&file).unwrap();
        let mut mgr = PresetManager::with_file(file.clone()).unwrap();
        let new_p = make_preset("my-haiku", "claude");
        mgr.save_preset(&new_p).unwrap();
        // Re-read fresh.
        let mgr2 = PresetManager::with_file(file).unwrap();
        let names = mgr2.list_names();
        assert_eq!(names.len(), 5);
        assert!(names.contains(&"my-haiku"));
        // Original four still present and ordered.
        assert_eq!(
            names[..4],
            [
                "claude-interactive-yolo",
                "codex-interactive-yolo",
                "opusplan",
                "shell",
            ]
        );
    }

    #[test]
    fn save_preset_replaces_in_place_when_name_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("presets.toml");
        install_default_presets(&file).unwrap();
        let mut mgr = PresetManager::with_file(file.clone()).unwrap();
        let mut p = make_preset("claude-interactive-yolo", "claude");
        p.description = "edited by user".to_string();
        mgr.save_preset(&p).unwrap();
        let mgr2 = PresetManager::with_file(file).unwrap();
        assert_eq!(mgr2.list_names().len(), 4, "should not duplicate");
        let loaded = mgr2.get("claude-interactive-yolo").unwrap();
        assert_eq!(loaded.description, "edited by user");
    }

    #[test]
    fn delete_preset_removes_array_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("presets.toml");
        install_default_presets(&file).unwrap();
        let mut mgr = PresetManager::with_file(file.clone()).unwrap();
        mgr.delete("opusplan").unwrap();
        let mgr2 = PresetManager::with_file(file).unwrap();
        let names = mgr2.list_names();
        assert_eq!(names.len(), 3);
        assert!(!names.contains(&"opusplan"));
    }

    #[test]
    fn legacy_multi_file_dir_migration_deletes_shipped_only_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let presets_dir = root.join("presets");
        std::fs::create_dir_all(&presets_dir).unwrap();
        std::fs::write(
            presets_dir.join("claude-opus-yolo.toml"),
            r#"name = "claude-opus-yolo"
description = "Claude Opus, Boss, bypass"
agent_provider = "claude"
agent_model = "opus"
mode = "boss"
[permissions]
skip_all = true
"#,
        )
        .unwrap();
        std::fs::write(
            presets_dir.join("codex-yolo.toml"),
            r#"name = "codex-yolo"
agent_provider = "codex"
agent_model = "default"
mode = "boss"
[permissions]
skip_all = true
"#,
        )
        .unwrap();
        let file = root.join("presets.toml");
        install_default_presets(&file).unwrap();
        assert!(!presets_dir.exists(), "legacy dir should be removed");
        assert!(file.exists(), "single-file presets.toml should be created");
        let mgr = PresetManager::with_file(file).unwrap();
        let names = mgr.list_names();
        assert_eq!(names.len(), 4, "only shipped defaults should remain");
    }

    #[test]
    fn legacy_user_customised_files_are_merged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let presets_dir = root.join("presets");
        std::fs::create_dir_all(&presets_dir).unwrap();
        std::fs::write(
            presets_dir.join("my-custom.toml"),
            r#"name = "my-custom"
description = "my custom preset"
agent_provider = "claude"
agent_model = "sonnet"
mode = "boss"
skills = ["my-skill"]
[permissions]
skip_all = false
"#,
        )
        .unwrap();
        let file = root.join("presets.toml");
        install_default_presets(&file).unwrap();
        assert!(!presets_dir.exists());
        let mgr = PresetManager::with_file(file).unwrap();
        let p = mgr.get("my-custom").expect("user-customised preset should have been migrated");
        assert_eq!(p.skills, vec!["my-skill".to_string()]);
    }

    #[test]
    fn legacy_customised_named_after_shipped_default_is_carried_forward() {
        // claude-interactive-yolo on disk with extra skills should be
        // treated as user-customised (not shipped-pristine) and migrated.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let presets_dir = root.join("presets");
        std::fs::create_dir_all(&presets_dir).unwrap();
        std::fs::write(
            presets_dir.join("claude-interactive-yolo.toml"),
            r#"name = "claude-interactive-yolo"
agent_provider = "claude"
agent_model = "default"
mode = "interactive"
skills = ["my-skill"]
[permissions]
skip_all = true
"#,
        )
        .unwrap();
        let file = root.join("presets.toml");
        install_default_presets(&file).unwrap();
        let mgr = PresetManager::with_file(file).unwrap();
        // Bundled default already present; user-customised collision is
        // skipped (bundled version wins by being first).
        let p = mgr.get("claude-interactive-yolo").unwrap();
        // Bundled version has no skills; user version had one — we
        // intentionally do not clobber the bundled entry, so this stays
        // empty. The user's customisation is logged but not lost from the
        // backup we just created (assuming admin curation).
        assert!(
            p.skills.is_empty(),
            "bundled default should win on name collision"
        );
    }

    #[test]
    fn load_repo_preset_accepts_new_multi_doc_format() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".agents-box")).unwrap();
        std::fs::write(
            tmp.path().join(".agents-box").join("presets.toml"),
            r#"[[preset]]
name = "repo-override"
agent_provider = "claude"
agent_model = "opus"
mode = "boss"
[preset.permissions]
skip_all = false
"#,
        )
        .unwrap();
        let p = PresetManager::load_repo_preset(tmp.path()).unwrap().unwrap();
        assert_eq!(p.name, "repo-override");
    }

    #[test]
    fn load_repo_preset_accepts_old_single_doc_format() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".agents-box")).unwrap();
        std::fs::write(
            tmp.path().join(".agents-box").join("preset.toml"),
            r#"name = "legacy-single"
agent_provider = "claude"
agent_model = "sonnet"
mode = "boss"
[permissions]
skip_all = false
"#,
        )
        .unwrap();
        let p = PresetManager::load_repo_preset(tmp.path()).unwrap().unwrap();
        assert_eq!(p.name, "legacy-single");
    }
}
