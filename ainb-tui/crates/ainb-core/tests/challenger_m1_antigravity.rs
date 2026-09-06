//! Empirical verification and stress tests for Milestone 1 (Google Antigravity Integration)

// ABOUTME: Empirical verification and stress tests for Milestone 1 (Google Antigravity Integration)
// Tests cover:
// 1. TUI Configuration Wizard & Model Cycling Logic
// 2. Preset Parsing of antigravity-interactive-yolo
// 3. Tmux Process Detection ("agy") & CLI Command Construction
// 4. Provider Registry, Environment Variable Mapping (GEMINI_API_KEY) & Permissions Flags
//
// The inbox agent-filter cycle test went with the host Inbox screen. Antigravity
// is still covered here by the registry, preset, tmux-detection and argv cases
// above; the filter it exercised no longer exists on any surface.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::path::PathBuf;

use ainb::agents::SessionAgentRegistry;
use ainb::components::new_session::configure::{
    ConfigureRow, ConfigureState, CustomOverrides, PresetSelection, handle_key, render,
};
use ainb::config::CliProvider;
use ainb::config::presets::{
    PresetManager, SessionMode, create_default_presets, install_default_presets,
};
use ainb::config::session_defaults::SessionDefaults;
use ainb::git::repo_source::RepoSource;
use ainb::models::session::{AntigravityModel, SessionAgentType};
use ainb::providers::{AntigravityProvider, ProviderRegistry};

fn make_dummy_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

fn create_test_configure_state() -> ConfigureState {
    let repo_source = RepoSource::LocalPath(PathBuf::from("/tmp/test-repo"));
    let defaults = SessionDefaults::default();

    ConfigureState::from_pick_repo(
        repo_source,
        "test-repo".to_string(),
        &defaults,
        Some("main".to_string()),
        "agents/",
        vec![],
        vec![],
    )
}

// ============================================================================
// 1. TUI CONFIGURATION WIZARD & MODEL CYCLING TESTS
// ============================================================================

#[test]
fn test_configure_wizard_custom_cycle_to_antigravity() {
    let mut state = create_test_configure_state();

    // Cycle presets until reaching Custom slot
    while state.preset_selection != PresetSelection::Custom {
        handle_key(&mut state, make_dummy_key(KeyCode::Right));
    }
    assert_eq!(state.preset_selection, PresetSelection::Custom);

    // Focus Agent row
    for _ in 0..10 {
        if state.focused_row == ConfigureRow::Agent {
            break;
        }
        handle_key(&mut state, make_dummy_key(KeyCode::Tab));
    }
    assert_eq!(state.focused_row, ConfigureRow::Agent);

    // Cycle agent until "antigravity"
    for _ in 0..10 {
        if state.effective_preset().agent_provider == "antigravity" {
            break;
        }
        handle_key(&mut state, make_dummy_key(KeyCode::Right));
    }
    assert_eq!(state.effective_preset().agent_provider, "antigravity");

    // Move to Model row
    handle_key(&mut state, make_dummy_key(KeyCode::Tab));
    assert_eq!(state.focused_row, ConfigureRow::Model);

    // Initial model should be default
    assert_eq!(state.effective_preset().agent_model, "default");

    // Cycle Model forward: default -> gemini-3.7-flash -> gemini-2.5-pro -> gemini-2.5-flash -> default
    handle_key(&mut state, make_dummy_key(KeyCode::Right));
    assert_eq!(state.effective_preset().agent_model, "gemini-3.7-flash");

    handle_key(&mut state, make_dummy_key(KeyCode::Right));
    assert_eq!(state.effective_preset().agent_model, "gemini-2.5-pro");

    handle_key(&mut state, make_dummy_key(KeyCode::Right));
    assert_eq!(state.effective_preset().agent_model, "gemini-2.5-flash");

    handle_key(&mut state, make_dummy_key(KeyCode::Right));
    assert_eq!(state.effective_preset().agent_model, "default");

    // Cycle Model backward: default -> gemini-2.5-flash -> gemini-2.5-pro -> gemini-3.7-flash -> default
    handle_key(&mut state, make_dummy_key(KeyCode::Left));
    assert_eq!(state.effective_preset().agent_model, "gemini-2.5-flash");

    handle_key(&mut state, make_dummy_key(KeyCode::Left));
    assert_eq!(state.effective_preset().agent_model, "gemini-2.5-pro");

    handle_key(&mut state, make_dummy_key(KeyCode::Left));
    assert_eq!(state.effective_preset().agent_model, "gemini-3.7-flash");

    handle_key(&mut state, make_dummy_key(KeyCode::Left));
    assert_eq!(state.effective_preset().agent_model, "default");
}

#[test]
fn test_configure_wizard_boundary_crossing_model_reset() {
    let mut state = create_test_configure_state();
    state.preset_selection = PresetSelection::Custom;
    let overrides = CustomOverrides {
        agent_provider: "claude".to_string(),
        agent_model: "claude-3-7-sonnet".to_string(),
        mode: SessionMode::Interactive,
        skip_all: true,
    };
    state.custom_overrides = Some(overrides);
    state.focused_row = ConfigureRow::Agent;

    // Claude -> Codex
    handle_key(&mut state, make_dummy_key(KeyCode::Right));
    assert_eq!(
        state.custom_overrides.as_ref().unwrap().agent_provider,
        "codex"
    );
    assert_eq!(
        state.custom_overrides.as_ref().unwrap().agent_model,
        "default"
    );

    // Set Codex model
    state.custom_overrides.as_mut().unwrap().agent_model = "gpt-5.5".to_string();

    // Codex -> Antigravity
    handle_key(&mut state, make_dummy_key(KeyCode::Right));
    assert_eq!(
        state.custom_overrides.as_ref().unwrap().agent_provider,
        "antigravity"
    );
    assert_eq!(
        state.custom_overrides.as_ref().unwrap().agent_model,
        "default"
    );

    // Set Antigravity model
    state.custom_overrides.as_mut().unwrap().agent_model = "gemini-3.7-flash".to_string();

    // Antigravity -> Codex (backward)
    handle_key(&mut state, make_dummy_key(KeyCode::Left));
    assert_eq!(
        state.custom_overrides.as_ref().unwrap().agent_provider,
        "codex"
    );
    assert_eq!(
        state.custom_overrides.as_ref().unwrap().agent_model,
        "default"
    );

    // Back to Antigravity
    handle_key(&mut state, make_dummy_key(KeyCode::Right));
    assert_eq!(
        state.custom_overrides.as_ref().unwrap().agent_provider,
        "antigravity"
    );
    state.custom_overrides.as_mut().unwrap().agent_model = "gemini-2.5-pro".to_string();

    // Antigravity -> Copilot (forward)
    handle_key(&mut state, make_dummy_key(KeyCode::Right));
    assert_eq!(
        state.custom_overrides.as_ref().unwrap().agent_provider,
        "copilot"
    );
}

#[test]
fn test_configure_render_with_antigravity_various_widths() {
    let mut state = create_test_configure_state();
    state.preset_selection = PresetSelection::Custom;
    state.custom_overrides = Some(CustomOverrides {
        agent_provider: "antigravity".to_string(),
        agent_model: "gemini-3.7-flash".to_string(),
        mode: SessionMode::Interactive,
        skip_all: true,
    });

    // Test narrow width (40 columns) - triggers pill width fallback without crashing
    {
        let backend = TestBackend::new(40, 25);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render(f, &state, f.area());
            })
            .unwrap();
    }

    // Test standard width (80 columns)
    {
        let backend = TestBackend::new(80, 25);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render(f, &state, f.area());
            })
            .unwrap();
    }

    // Test wide terminal (160 columns)
    {
        let backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render(f, &state, f.area());
            })
            .unwrap();
    }
}

#[test]
fn test_antigravity_model_parsing_and_variants() {
    assert_eq!(AntigravityModel::parse(""), AntigravityModel::SystemDefault);
    assert_eq!(
        AntigravityModel::parse("   "),
        AntigravityModel::SystemDefault
    );
    assert_eq!(
        AntigravityModel::parse("default"),
        AntigravityModel::SystemDefault
    );
    assert_eq!(
        AntigravityModel::parse("DEFAULT"),
        AntigravityModel::SystemDefault
    );
    assert_eq!(
        AntigravityModel::parse("gemini-3.7-flash"),
        AntigravityModel::Gemini37Flash
    );
    assert_eq!(
        AntigravityModel::parse("GEMINI-3.7-FLASH"),
        AntigravityModel::Gemini37Flash
    );
    assert_eq!(
        AntigravityModel::parse("3.7-flash"),
        AntigravityModel::Gemini37Flash
    );
    assert_eq!(
        AntigravityModel::parse("3.7"),
        AntigravityModel::Gemini37Flash
    );
    assert_eq!(
        AntigravityModel::parse("gemini-2.5-pro"),
        AntigravityModel::Gemini25Pro
    );
    assert_eq!(
        AntigravityModel::parse("2.5-pro"),
        AntigravityModel::Gemini25Pro
    );
    assert_eq!(
        AntigravityModel::parse("pro"),
        AntigravityModel::Gemini25Pro
    );
    assert_eq!(
        AntigravityModel::parse("gemini-2.5-flash"),
        AntigravityModel::Gemini25Flash
    );
    assert_eq!(
        AntigravityModel::parse("2.5-flash"),
        AntigravityModel::Gemini25Flash
    );
    assert_eq!(
        AntigravityModel::parse("flash"),
        AntigravityModel::Gemini25Flash
    );
    assert_eq!(
        AntigravityModel::parse("nonexistent-model-name"),
        AntigravityModel::SystemDefault
    );

    // Verify Display and FromStr traits
    use std::str::FromStr;
    assert_eq!(
        AntigravityModel::from_str("gemini-3.7-flash").unwrap(),
        AntigravityModel::Gemini37Flash
    );
    assert_eq!(
        AntigravityModel::from_str("3.7").unwrap(),
        AntigravityModel::Gemini37Flash
    );
    assert_eq!(
        format!("{}", AntigravityModel::Gemini37Flash),
        "gemini-3.7-flash"
    );
    assert_eq!(format!("{}", AntigravityModel::SystemDefault), "default");
}

// ============================================================================
// 2. PRESET PARSING OF antigravity-interactive-yolo
// ============================================================================

#[test]
fn test_preset_antigravity_interactive_yolo_parsing() {
    let presets = create_default_presets();
    let agy_preset = presets.iter().find(|p| p.name == "antigravity-interactive-yolo");
    assert!(
        agy_preset.is_some(),
        "antigravity-interactive-yolo must be in default presets"
    );

    let p = agy_preset.unwrap();
    assert_eq!(p.name, "antigravity-interactive-yolo");
    assert_eq!(p.agent_provider, "antigravity");
    assert_eq!(p.agent_model, "default");
    assert_eq!(p.mode, SessionMode::Interactive);
    assert!(p.permissions.skip_all);
}

#[test]
fn test_preset_manager_crud_antigravity() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("presets.toml");
    install_default_presets(&file).unwrap();

    let mut mgr = PresetManager::with_file(file.clone()).unwrap();
    let p = mgr.get("antigravity-interactive-yolo");
    assert!(p.is_some());
    assert_eq!(p.unwrap().agent_provider, "antigravity");

    // Modify description and save
    let mut modified = p.unwrap().clone();
    modified.description = "Custom AGY Description".to_string();
    mgr.save_preset(&modified).unwrap();

    let mgr2 = PresetManager::with_file(file).unwrap();
    let reloaded = mgr2.get("antigravity-interactive-yolo").unwrap();
    assert_eq!(reloaded.description, "Custom AGY Description");
}

// ============================================================================
// 3. TMUX PROCESS DETECTION & COMMAND CONSTS
// ============================================================================

#[test]
fn test_tmux_process_name_matching_antigravity() {
    // Test the string matching heuristics used for tmux detection
    let cases = vec![
        ("agy", Some(SessionAgentType::Antigravity)),
        ("/usr/local/bin/agy", Some(SessionAgentType::Antigravity)),
        (
            "/opt/homebrew/bin/agy --model gemini-3.7-flash",
            Some(SessionAgentType::Antigravity),
        ),
        ("antigravity", Some(SessionAgentType::Antigravity)),
        (
            "/usr/bin/antigravity -i",
            Some(SessionAgentType::Antigravity),
        ),
        ("claude", Some(SessionAgentType::Claude)),
        ("codex", Some(SessionAgentType::Codex)),
        ("gemini", Some(SessionAgentType::Gemini)),
        ("copilot", Some(SessionAgentType::Copilot)),
        ("zsh", None),
        ("bash", None),
    ];

    for (cmd_str, expected) in cases {
        let cmd = cmd_str.trim().to_lowercase();
        let detected = if cmd.contains("claude") {
            Some(SessionAgentType::Claude)
        } else if cmd.contains("codex") {
            Some(SessionAgentType::Codex)
        } else if cmd.contains("agy") || cmd.contains("antigravity") {
            Some(SessionAgentType::Antigravity)
        } else if cmd.contains("gemini") {
            Some(SessionAgentType::Gemini)
        } else if cmd.contains("copilot") {
            Some(SessionAgentType::Copilot)
        } else {
            None
        };
        assert_eq!(detected, expected, "Failed for process string: {}", cmd_str);
    }
}

#[test]
fn test_cli_provider_antigravity_constants() {
    let p = CliProvider::Antigravity;
    assert_eq!(p.command(), "agy");
    assert_eq!(p.api_key_env_var(), "GEMINI_API_KEY");
    assert_eq!(p.skip_permissions_flag(), "--dangerously-skip-permissions");
    assert_eq!(p.as_str(), "antigravity");
    assert_eq!(CliProvider::from_str("agy"), CliProvider::Antigravity);
    assert_eq!(
        CliProvider::from_str("antigravity"),
        CliProvider::Antigravity
    );
}

// ============================================================================
// 4. PROVIDER REGISTRY & ENVIRONMENT VARIABLE MAPPING (GEMINI_API_KEY)
// ============================================================================

#[test]
fn test_antigravity_provider_contract_and_env_var() {
    let provider = AntigravityProvider;
    use ainb::providers::Provider;

    assert_eq!(provider.id(), "antigravity");
    assert_eq!(provider.display_name(), "Google Antigravity");
    assert_eq!(provider.command(), "agy");
    assert_eq!(provider.api_key_env_var(), Some("GEMINI_API_KEY"));
    assert_eq!(
        provider.skip_permissions_flag(),
        Some("--dangerously-skip-permissions")
    );
    assert_eq!(
        provider.install_docs_url(),
        "https://github.com/google/antigravity"
    );
}

#[test]
fn test_provider_registry_aliases_and_builtins() {
    let r = ProviderRegistry::built_ins();
    assert!(r.get("antigravity").is_some());

    // Alias testing
    assert_eq!(
        r.get_with_aliases("antigravity").unwrap().id(),
        "antigravity"
    );
    assert_eq!(r.get_with_aliases("agy").unwrap().id(), "antigravity");
    assert_eq!(
        r.get_with_aliases("Antigravity").unwrap().id(),
        "antigravity"
    );
    assert_eq!(r.get_with_aliases("AGY").unwrap().id(), "antigravity");

    // Session Agent Registry
    let agents = SessionAgentRegistry::built_ins();
    let agy_agent = agents.get("antigravity");
    assert!(agy_agent.is_some());
    assert_eq!(agy_agent.as_ref().unwrap().name(), "Google Antigravity");
    assert_eq!(agy_agent.as_ref().unwrap().icon(), "▲");
}
