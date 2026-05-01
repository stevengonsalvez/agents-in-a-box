// ABOUTME: CLI presets command for viewing and managing repository presets
//
// Subcommands:
//   list:   Show all presets (built-in + user custom) with descriptions
//   show:   Display full preset details (provider, model, skills, permissions, etc.)
//   create: Create a new custom preset with optional provider/model/description
//   delete: Remove a custom preset (built-ins cannot be deleted)
//   apply:  Write the selected preset to `.agents-box/preset.toml` in the CWD
//
// Built-in presets come from `create_default_presets()` and exist alongside
// any user-defined presets saved by `PresetManager` under
// `~/.agents-in-a-box/presets/`. Built-ins are immutable: creating or
// deleting a preset with a built-in name is rejected with a clear error.

use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use serde::Serialize;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use super::OutputFormat;
use crate::config::presets::{
    PermissionSet, PresetManager, RepositoryPreset, create_default_presets,
};

#[derive(Subcommand)]
pub enum PresetsCommands {
    /// List all available presets (built-in + custom)
    List,
    /// Show full details for a specific preset
    Show {
        /// Preset name
        name: String,
    },
    /// Create a new custom preset
    Create {
        /// Preset name (must be unique and not collide with built-ins)
        name: String,
        /// Agent provider (e.g., claude, codex, gemini)
        #[arg(long)]
        provider: Option<String>,
        /// Model identifier (e.g., sonnet, opus, haiku)
        #[arg(long)]
        model: Option<String>,
        /// Human-readable description
        #[arg(long)]
        description: Option<String>,
    },
    /// Delete a custom preset
    Delete {
        /// Preset name
        name: String,
    },
    /// Apply a preset to the current repository (writes .agents-box/preset.toml)
    Apply {
        /// Preset name
        name: String,
    },
}

/// JSON entry emitted by `presets list --format json`
#[derive(Debug, Serialize)]
struct PresetListEntry {
    name: String,
    description: String,
    provider: String,
    model: String,
    builtin: bool,
}

/// Execute a presets subcommand
#[allow(clippy::unused_async)]
pub async fn execute(command: PresetsCommands, format: OutputFormat) -> Result<()> {
    match command {
        PresetsCommands::List => cmd_list(format),
        PresetsCommands::Show { name } => cmd_show(&name, format),
        PresetsCommands::Create {
            name,
            provider,
            model,
            description,
        } => cmd_create(&name, provider, model, description),
        PresetsCommands::Delete { name } => cmd_delete(&name),
        PresetsCommands::Apply { name } => cmd_apply(&name, &std::env::current_dir()?),
    }
}

// --- List ---

fn cmd_list(format: OutputFormat) -> Result<()> {
    let manager = PresetManager::new().context("Failed to initialize preset manager")?;
    let entries = build_list_entries(&manager);

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&entries)
                .context("Failed to serialize presets as JSON")?;
            println!("{json}");
        }
        OutputFormat::Text | OutputFormat::Csv => {
            println!("Available presets:");
            println!("{}", "\u{2501}".repeat(60));
            for entry in &entries {
                let marker = if entry.builtin {
                    "[built-in]"
                } else {
                    "[custom]  "
                };
                println!(
                    "  {marker} {}  ({}/{})",
                    entry.name, entry.provider, entry.model
                );
                if !entry.description.is_empty() {
                    println!("             {}", entry.description);
                }
            }
            if entries.is_empty() {
                println!("  (no presets found)");
            }
            println!();
            println!("Use 'ainb presets show <name>' to view details.");
        }
    }

    Ok(())
}

fn build_list_entries(manager: &PresetManager) -> Vec<PresetListEntry> {
    let mut entries: Vec<PresetListEntry> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for preset in create_default_presets() {
        seen.insert(preset.name.clone());
        entries.push(to_list_entry(&preset, true));
    }

    let mut customs: Vec<&RepositoryPreset> =
        manager.all().into_iter().filter(|p| !seen.contains(&p.name)).collect();
    customs.sort_by(|a, b| a.name.cmp(&b.name));
    for preset in customs {
        entries.push(to_list_entry(preset, false));
    }

    entries
}

fn to_list_entry(preset: &RepositoryPreset, builtin: bool) -> PresetListEntry {
    PresetListEntry {
        name: preset.name.clone(),
        description: preset.description.clone(),
        provider: preset.agent_provider.clone(),
        model: preset.agent_model.clone(),
        builtin,
    }
}

// --- Show ---

fn cmd_show(name: &str, format: OutputFormat) -> Result<()> {
    let manager = PresetManager::new().context("Failed to initialize preset manager")?;
    let (preset, builtin) =
        resolve_preset(&manager, name).ok_or_else(|| anyhow!("Preset '{name}' not found"))?;

    match format {
        OutputFormat::Json => {
            let wrapper = serde_json::json!({
                "name": preset.name,
                "builtin": builtin,
                "preset": &preset,
            });
            println!("{}", serde_json::to_string_pretty(&wrapper)?);
        }
        OutputFormat::Text | OutputFormat::Csv => {
            println!("{}", format_preset_text(&preset, builtin));
        }
    }

    Ok(())
}

fn resolve_preset(manager: &PresetManager, name: &str) -> Option<(RepositoryPreset, bool)> {
    if let Some(builtin) = create_default_presets().into_iter().find(|p| p.name == name) {
        return Some((builtin, true));
    }
    manager.get(name).map(|p| (p.clone(), false))
}

fn format_preset_text(preset: &RepositoryPreset, builtin: bool) -> String {
    let origin = if builtin { "built-in" } else { "custom" };
    let mut out = String::new();
    let _ = writeln!(out, "Preset: {} ({origin})", preset.name);
    let _ = writeln!(out, "{}", "\u{2501}".repeat(60));
    if !preset.description.is_empty() {
        let _ = writeln!(out, "Description: {}", preset.description);
    }
    let _ = writeln!(out, "Provider:    {}", preset.agent_provider);
    let _ = writeln!(out, "Model:       {}", preset.agent_model);

    if preset.skills.is_empty() {
        out.push_str("Skills:      (none)\n");
    } else {
        let _ = writeln!(out, "Skills:      {}", preset.skills.join(", "));
    }

    if preset.plugins.is_empty() {
        out.push_str("Plugins:     (none)\n");
    } else {
        let _ = writeln!(out, "Plugins:     {}", preset.plugins.join(", "));
    }

    out.push_str("Permissions:\n");
    out.push_str(&format_permissions(&preset.permissions));

    if let Some(rules) = &preset.custom_rules {
        out.push_str("Custom rules:\n");
        for line in rules.lines() {
            let _ = writeln!(out, "  {line}");
        }
    }

    if !preset.environment.is_empty() {
        out.push_str("Environment:\n");
        let mut keys: Vec<&String> = preset.environment.keys().collect();
        keys.sort();
        for key in keys {
            let _ = writeln!(out, "  {key}={}", preset.environment[key]);
        }
    }

    out
}

fn format_permissions(perms: &PermissionSet) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "  file_write: {}", perms.file_write);
    let _ = writeln!(s, "  shell:      {}", perms.shell);
    let _ = writeln!(s, "  git:        {}", perms.git);
    let _ = writeln!(s, "  network:    {}", perms.network);
    let _ = writeln!(s, "  skip_all:   {}", perms.skip_all);
    s
}

// --- Create ---

fn cmd_create(
    name: &str,
    provider: Option<String>,
    model: Option<String>,
    description: Option<String>,
) -> Result<()> {
    if is_builtin_name(name) {
        return Err(anyhow!(
            "Cannot create preset '{name}': name is reserved for a built-in preset"
        ));
    }

    let manager = PresetManager::new().context("Failed to initialize preset manager")?;
    if manager.get(name).is_some() {
        return Err(anyhow!("Preset '{name}' already exists"));
    }

    let preset = build_new_preset(name, provider, model, description);
    manager.save_preset(&preset).context("Failed to save new preset")?;

    println!("Created preset '{name}'");
    println!("  Provider: {}", preset.agent_provider);
    println!("  Model:    {}", preset.agent_model);
    if !preset.description.is_empty() {
        println!("  Description: {}", preset.description);
    }
    Ok(())
}

fn build_new_preset(
    name: &str,
    provider: Option<String>,
    model: Option<String>,
    description: Option<String>,
) -> RepositoryPreset {
    let defaults = RepositoryPreset::default();
    RepositoryPreset {
        name: name.to_string(),
        description: description.unwrap_or_default(),
        agent_provider: provider.unwrap_or(defaults.agent_provider),
        agent_model: model.unwrap_or(defaults.agent_model),
        skills: Vec::new(),
        plugins: Vec::new(),
        permissions: PermissionSet::default(),
        custom_rules: None,
        environment: std::collections::HashMap::new(),
    }
}

// --- Delete ---

fn cmd_delete(name: &str) -> Result<()> {
    if is_builtin_name(name) {
        return Err(anyhow!(
            "Cannot delete preset '{name}': built-in presets are immutable"
        ));
    }

    let mut manager = PresetManager::new().context("Failed to initialize preset manager")?;
    if manager.get(name).is_none() {
        return Err(anyhow!("Preset '{name}' not found"));
    }

    manager.delete(name).context("Failed to delete preset")?;
    println!("Deleted preset '{name}'");
    Ok(())
}

// --- Apply ---

fn cmd_apply(name: &str, cwd: &Path) -> Result<()> {
    let manager = PresetManager::new().context("Failed to initialize preset manager")?;
    let (preset, _builtin) =
        resolve_preset(&manager, name).ok_or_else(|| anyhow!("Preset '{name}' not found"))?;

    let target_path = apply_preset_to_dir(&preset, cwd)?;
    println!("Applied preset '{name}' to {}", target_path.display());
    Ok(())
}

fn apply_preset_to_dir(preset: &RepositoryPreset, cwd: &Path) -> Result<PathBuf> {
    let dir = cwd.join(".agents-box");
    fs::create_dir_all(&dir).context("Failed to create .agents-box directory")?;

    let path = dir.join("preset.toml");
    let content = toml::to_string_pretty(preset).context("Failed to serialize preset to TOML")?;
    fs::write(&path, content).context("Failed to write preset.toml")?;
    Ok(path)
}

// --- Helpers ---

fn is_builtin_name(name: &str) -> bool {
    create_default_presets().iter().any(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // --- is_builtin_name ---

    #[test]
    fn test_is_builtin_name_matches_defaults() {
        assert!(is_builtin_name("rust-backend"));
        assert!(is_builtin_name("typescript-frontend"));
        assert!(is_builtin_name("fast-iteration"));
    }

    #[test]
    fn test_is_builtin_name_rejects_custom() {
        assert!(!is_builtin_name("my-custom-preset"));
        assert!(!is_builtin_name(""));
        assert!(!is_builtin_name("Rust-Backend"));
    }

    // --- create_default_presets surface ---

    #[test]
    fn test_default_presets_contain_expected_names() {
        let names: Vec<String> = create_default_presets().into_iter().map(|p| p.name).collect();
        assert!(names.contains(&"rust-backend".to_string()));
        assert!(names.contains(&"typescript-frontend".to_string()));
        assert!(names.contains(&"fast-iteration".to_string()));
    }

    // --- build_new_preset ---

    #[test]
    fn test_build_new_preset_uses_defaults_when_empty() {
        let p = build_new_preset("myp", None, None, None);
        assert_eq!(p.name, "myp");
        assert_eq!(p.agent_provider, "claude");
        assert_eq!(p.agent_model, "sonnet");
        assert_eq!(p.description, "");
        assert!(p.skills.is_empty());
    }

    #[test]
    fn test_build_new_preset_respects_overrides() {
        let p = build_new_preset(
            "myp",
            Some("codex".to_string()),
            Some("opus".to_string()),
            Some("hello".to_string()),
        );
        assert_eq!(p.agent_provider, "codex");
        assert_eq!(p.agent_model, "opus");
        assert_eq!(p.description, "hello");
    }

    // --- apply_preset_to_dir round trip ---

    #[test]
    fn test_apply_preset_writes_toml_file() {
        let dir = tempdir().unwrap();
        let preset = build_new_preset("scratch", None, None, Some("testing".to_string()));

        let path = apply_preset_to_dir(&preset, dir.path()).unwrap();

        assert!(path.ends_with(".agents-box/preset.toml"));
        assert!(path.exists());

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("name = \"scratch\""));
        assert!(content.contains("description = \"testing\""));
    }

    #[test]
    fn test_apply_preset_round_trip_via_load_repo_preset() {
        let dir = tempdir().unwrap();
        let original =
            create_default_presets().into_iter().find(|p| p.name == "rust-backend").unwrap();

        apply_preset_to_dir(&original, dir.path()).unwrap();

        let loaded = PresetManager::load_repo_preset(dir.path())
            .unwrap()
            .expect("preset.toml should be loadable");
        assert_eq!(loaded.name, "rust-backend");
        assert_eq!(loaded.agent_provider, original.agent_provider);
        assert_eq!(loaded.agent_model, original.agent_model);
        assert_eq!(loaded.skills, original.skills);
        assert_eq!(
            loaded.permissions.file_write,
            original.permissions.file_write
        );
    }

    #[test]
    fn test_apply_preset_creates_agents_box_dir() {
        let dir = tempdir().unwrap();
        assert!(!dir.path().join(".agents-box").exists());

        let preset = build_new_preset("scratch", None, None, None);
        apply_preset_to_dir(&preset, dir.path()).unwrap();

        assert!(dir.path().join(".agents-box").is_dir());
        assert!(dir.path().join(".agents-box").join("preset.toml").is_file());
    }

    #[test]
    fn test_apply_preset_overwrites_existing() {
        let dir = tempdir().unwrap();

        let first = build_new_preset("v1", None, None, Some("first".to_string()));
        apply_preset_to_dir(&first, dir.path()).unwrap();

        let second = build_new_preset("v2", None, None, Some("second".to_string()));
        apply_preset_to_dir(&second, dir.path()).unwrap();

        let loaded = PresetManager::load_repo_preset(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.name, "v2");
        assert_eq!(loaded.description, "second");
    }

    // --- format_preset_text ---

    #[test]
    fn test_format_preset_text_mentions_builtin_tag() {
        let preset =
            create_default_presets().into_iter().find(|p| p.name == "rust-backend").unwrap();
        let text = format_preset_text(&preset, true);
        assert!(text.contains("rust-backend"));
        assert!(text.contains("built-in"));
        assert!(text.contains("Provider:"));
        assert!(text.contains("Model:"));
        assert!(text.contains("Permissions:"));
    }

    #[test]
    fn test_format_preset_text_custom_tag() {
        let preset = build_new_preset("myp", None, None, Some("mine".to_string()));
        let text = format_preset_text(&preset, false);
        assert!(text.contains("custom"));
        assert!(text.contains("mine"));
    }

    #[test]
    fn test_format_preset_text_handles_empty_skills() {
        let preset = build_new_preset("empty", None, None, None);
        let text = format_preset_text(&preset, false);
        assert!(text.contains("Skills:      (none)"));
        assert!(text.contains("Plugins:     (none)"));
    }

    // --- format_permissions ---

    #[test]
    fn test_format_permissions_renders_all_fields() {
        let perms = PermissionSet {
            file_write: true,
            shell: true,
            git: true,
            network: false,
            skip_all: false,
        };
        let text = format_permissions(&perms);
        assert!(text.contains("file_write: true"));
        assert!(text.contains("shell:      true"));
        assert!(text.contains("git:        true"));
        assert!(text.contains("network:    false"));
        assert!(text.contains("skip_all:   false"));
    }

    // --- PresetListEntry JSON serialization ---

    #[test]
    fn test_list_entry_json_schema() {
        let entry = PresetListEntry {
            name: "rust-backend".to_string(),
            description: "desc".to_string(),
            provider: "claude".to_string(),
            model: "sonnet".to_string(),
            builtin: true,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"name\":\"rust-backend\""));
        assert!(json.contains("\"builtin\":true"));
        assert!(json.contains("\"provider\":\"claude\""));
    }

    // --- to_list_entry ---

    #[test]
    fn test_to_list_entry_copies_fields() {
        let preset = RepositoryPreset {
            name: "n".to_string(),
            description: "d".to_string(),
            agent_provider: "codex".to_string(),
            agent_model: "opus".to_string(),
            skills: Vec::new(),
            plugins: Vec::new(),
            permissions: PermissionSet::default(),
            custom_rules: None,
            environment: std::collections::HashMap::new(),
        };
        let entry = to_list_entry(&preset, false);
        assert_eq!(entry.name, "n");
        assert_eq!(entry.description, "d");
        assert_eq!(entry.provider, "codex");
        assert_eq!(entry.model, "opus");
        assert!(!entry.builtin);
    }

    // --- resolve_preset with built-ins (no manager interference) ---

    #[test]
    fn test_resolve_preset_finds_builtin() {
        // PresetManager::new() loads real ~/.agents-in-a-box/presets,
        // but resolve_preset checks built-ins first so this is stable.
        if let Ok(manager) = PresetManager::new() {
            let (preset, builtin) = resolve_preset(&manager, "rust-backend").unwrap();
            assert!(builtin);
            assert_eq!(preset.name, "rust-backend");
        }
    }

    #[test]
    fn test_resolve_preset_missing_returns_none() {
        if let Ok(manager) = PresetManager::new() {
            assert!(resolve_preset(&manager, "definitely-not-a-real-preset-xyz").is_none());
        }
    }
}
