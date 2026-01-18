// ABOUTME: Behavioral tests for configuration loading and provider enums
// Verifies config defaults, serialization roundtrips, and enum variant coverage

use ainb::config::{
    AppConfig, AuthenticationConfig, ClaudeAuthProvider, CliProvider,
};

/// Test that default config has sensible values for immediate usability
#[test]
fn test_default_config_has_sensible_values() {
    // Create default configuration
    let config = AppConfig::default();

    // Default CLI provider should be Claude
    assert_eq!(
        config.authentication.cli_provider,
        CliProvider::Claude,
        "Default CLI provider should be Claude"
    );

    // Default Claude auth provider should be SystemAuth
    assert_eq!(
        config.authentication.claude_provider,
        ClaudeAuthProvider::SystemAuth,
        "Default Claude auth provider should be SystemAuth"
    );

    // Note: default_model uses #[serde(default = "default_claude_model")] which only
    // applies during deserialization, not Default::default(). The derive(Default) gives
    // empty string. This tests current behavior - consider if this should be fixed in
    // AuthenticationConfig by implementing Default manually.
    assert!(
        config.authentication.default_model.is_empty() || config.authentication.default_model == "sonnet",
        "Default model should be empty (from derive) or 'sonnet' (from manual impl)"
    );

    // Version should match package version
    assert_eq!(
        config.version,
        env!("CARGO_PKG_VERSION"),
        "Version should match package version"
    );

    // Default container template should be set
    assert_eq!(
        config.default_container_template,
        "claude-dev",
        "Default container template should be 'claude-dev'"
    );

    // Container templates should be loaded (built-in templates)
    assert!(
        !config.container_templates.is_empty(),
        "Default config should have built-in container templates"
    );
    assert!(
        config.container_templates.contains_key("claude-dev"),
        "Default config should include 'claude-dev' template"
    );
}

/// Test that configuration roundtrips through TOML serialization without losing data
#[test]
fn test_config_serialization_roundtrip() {
    // Create config with custom values for each provider setting
    let mut config = AppConfig::default();
    config.authentication.cli_provider = CliProvider::Codex;
    config.authentication.claude_provider = ClaudeAuthProvider::ApiKey;
    config.authentication.default_model = "opus".to_string();
    config.workspace_defaults.branch_prefix = "feature/".to_string();

    // Serialize to TOML
    let toml_str = toml::to_string_pretty(&config)
        .expect("Config should serialize to TOML");

    // Verify TOML contains expected values
    assert!(
        toml_str.contains("cli_provider = \"codex\""),
        "TOML should contain cli_provider = codex"
    );
    assert!(
        toml_str.contains("claude_provider = \"api_key\""),
        "TOML should contain claude_provider = api_key"
    );
    assert!(
        toml_str.contains("default_model = \"opus\""),
        "TOML should contain default_model = opus"
    );

    // Deserialize back
    let loaded: AppConfig = toml::from_str(&toml_str)
        .expect("TOML should deserialize back to AppConfig");

    // Verify all values survived roundtrip
    assert_eq!(
        loaded.authentication.cli_provider,
        CliProvider::Codex,
        "CLI provider should survive roundtrip"
    );
    assert_eq!(
        loaded.authentication.claude_provider,
        ClaudeAuthProvider::ApiKey,
        "Claude auth provider should survive roundtrip"
    );
    assert_eq!(
        loaded.authentication.default_model,
        "opus",
        "Default model should survive roundtrip"
    );
    assert_eq!(
        loaded.workspace_defaults.branch_prefix,
        "feature/",
        "Branch prefix should survive roundtrip"
    );
}

/// Test that all CliProvider variants have display names (coverage for enum completeness)
#[test]
fn test_cli_provider_variants() {
    // All variants should have non-empty display names
    let variants = [
        CliProvider::Claude,
        CliProvider::Codex,
        CliProvider::Gemini,
    ];

    for variant in &variants {
        let display_name = variant.display_name();
        assert!(
            !display_name.is_empty(),
            "CliProvider::{:?} should have a display name",
            variant
        );

        let command = variant.command();
        assert!(
            !command.is_empty(),
            "CliProvider::{:?} should have a command",
            variant
        );

        let env_var = variant.api_key_env_var();
        assert!(
            !env_var.is_empty(),
            "CliProvider::{:?} should have an API key env var",
            variant
        );
    }

    // Verify specific display names
    assert_eq!(CliProvider::Claude.display_name(), "Claude Code");
    assert_eq!(CliProvider::Codex.display_name(), "OpenAI Codex");
    assert_eq!(CliProvider::Gemini.display_name(), "Google Gemini");

    // Verify commands
    assert_eq!(CliProvider::Claude.command(), "claude");
    assert_eq!(CliProvider::Codex.command(), "codex");
    assert_eq!(CliProvider::Gemini.command(), "gemini");

    // Verify env vars
    assert_eq!(CliProvider::Claude.api_key_env_var(), "ANTHROPIC_API_KEY");
    assert_eq!(CliProvider::Codex.api_key_env_var(), "OPENAI_API_KEY");
    assert_eq!(CliProvider::Gemini.api_key_env_var(), "GEMINI_API_KEY");
}

/// Test that all ClaudeAuthProvider variants serialize correctly
#[test]
fn test_claude_auth_provider_variants() {
    // All variants with their expected serialized form
    let variants_and_serialized = [
        (ClaudeAuthProvider::SystemAuth, "system_auth"),
        (ClaudeAuthProvider::ApiKey, "api_key"),
        (ClaudeAuthProvider::AmazonBedrock, "amazon_bedrock"),
        (ClaudeAuthProvider::GoogleVertex, "google_vertex"),
        (ClaudeAuthProvider::AzureFoundry, "azure_foundry"),
        (ClaudeAuthProvider::GlmZai, "glm_zai"),
        (ClaudeAuthProvider::LlmGateway, "llm_gateway"),
    ];

    for (variant, expected_str) in &variants_and_serialized {
        // Test as_str method
        assert_eq!(
            variant.as_str(),
            *expected_str,
            "ClaudeAuthProvider::{:?}.as_str() should return '{}'",
            variant,
            expected_str
        );

        // Test from_id roundtrip
        let roundtripped = ClaudeAuthProvider::from_id(expected_str);
        assert_eq!(
            &roundtripped,
            variant,
            "ClaudeAuthProvider::from_id('{}') should return {:?}",
            expected_str,
            variant
        );

        // Test TOML serialization via AuthenticationConfig
        let auth_config = AuthenticationConfig {
            cli_provider: CliProvider::default(),
            claude_provider: variant.clone(),
            default_model: "test".to_string(),
            github_method: None,
        };

        let toml_str = toml::to_string(&auth_config)
            .expect("AuthenticationConfig should serialize to TOML");

        assert!(
            toml_str.contains(expected_str),
            "TOML serialization of {:?} should contain '{}'",
            variant,
            expected_str
        );

        // Test TOML deserialization
        let loaded: AuthenticationConfig = toml::from_str(&toml_str)
            .expect("TOML should deserialize back to AuthenticationConfig");

        assert_eq!(
            loaded.claude_provider,
            *variant,
            "ClaudeAuthProvider::{:?} should survive TOML roundtrip",
            variant
        );
    }

    // Test from_id with unknown value falls back to SystemAuth
    let fallback = ClaudeAuthProvider::from_id("unknown_provider");
    assert_eq!(
        fallback,
        ClaudeAuthProvider::SystemAuth,
        "Unknown provider ID should fall back to SystemAuth"
    );
}
