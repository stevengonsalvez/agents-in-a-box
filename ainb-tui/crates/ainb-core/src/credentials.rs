// ABOUTME: Secure credential storage using system keychain
// Uses keyring crate for cross-platform support (macOS Keychain, Linux Secret Service)

use anyhow::{Context, Result};
use keyring::Entry;

const SERVICE_NAME: &str = "agents-in-a-box";

/// Credential keys for different secrets
#[derive(Clone, Copy)]
pub enum CredentialKey {
    AnthropicApiKey,
    OpenAiApiKey,
    GeminiApiKey,
    GithubPat,
}

impl CredentialKey {
    fn as_str(&self) -> &'static str {
        match self {
            CredentialKey::AnthropicApiKey => "anthropic_api_key",
            CredentialKey::OpenAiApiKey => "openai_api_key",
            CredentialKey::GeminiApiKey => "gemini_api_key",
            CredentialKey::GithubPat => "github_pat",
        }
    }

    /// Returns the expected prefix for API key validation, if any
    fn expected_prefix(&self) -> Option<&'static str> {
        match self {
            CredentialKey::AnthropicApiKey => Some("sk-ant-"),
            CredentialKey::OpenAiApiKey => Some("sk-"),
            CredentialKey::GeminiApiKey => None, // Gemini keys don't have a strict prefix
            CredentialKey::GithubPat => Some("ghp_"),
        }
    }

    /// Returns the number of characters to show in masked display
    fn mask_visible_chars(&self) -> usize {
        match self {
            CredentialKey::AnthropicApiKey => 12, // Show "sk-ant-xxxxx"
            CredentialKey::OpenAiApiKey => 8,     // Show "sk-xxxxx"
            CredentialKey::GeminiApiKey => 8,
            CredentialKey::GithubPat => 8,
        }
    }

    /// Returns a human-readable name for the credential
    fn display_name(&self) -> &'static str {
        match self {
            CredentialKey::AnthropicApiKey => "Anthropic API key",
            CredentialKey::OpenAiApiKey => "OpenAI API key",
            CredentialKey::GeminiApiKey => "Gemini API key",
            CredentialKey::GithubPat => "GitHub PAT",
        }
    }
}

/// Store a credential in the system keychain
pub fn store_credential(key: CredentialKey, value: &str) -> Result<()> {
    let entry = Entry::new(SERVICE_NAME, key.as_str()).context("Failed to create keyring entry")?;

    entry.set_password(value).context("Failed to store credential in keychain")?;

    tracing::info!("Stored credential: {}", key.as_str());
    Ok(())
}

/// Account name for keychain items addressed by SERVICE alone.
///
/// A `keychain:<service>` reference in config.toml is resolved by
/// [`crate::fleet::bridge::secrets`], which shells out to
/// `security find-generic-password -s <service> -w` — service only, no account.
/// The `keyring` crate always writes both attributes, so items written for those
/// references need one fixed account for the lookup to be unambiguous.
pub(crate) const REFERENCE_ACCOUNT: &str = "ainb";

/// Store a secret under an arbitrary keychain SERVICE name, for a
/// `keychain:<service>` reference held in config.toml.
///
/// Distinct from [`store_credential`], which files everything under one service
/// keyed by account: those are ainb's own well-known credentials, whereas these
/// are addressed by service because that is all the reference syntax carries.
pub fn store_keychain_secret(service: &str, value: &str) -> Result<()> {
    if service.trim().is_empty() {
        anyhow::bail!("keychain service name is empty");
    }
    if value.is_empty() {
        anyhow::bail!("refusing to store an empty secret");
    }
    let entry = Entry::new(service, REFERENCE_ACCOUNT).context("Failed to create keyring entry")?;
    entry.set_password(value).context("Failed to store secret in keychain")?;
    tracing::info!(service, "stored secret in keychain");
    Ok(())
}

/// Retrieve a credential from the system keychain
pub fn get_credential(key: CredentialKey) -> Result<Option<String>> {
    let entry = Entry::new(SERVICE_NAME, key.as_str()).context("Failed to create keyring entry")?;

    match entry.get_password() {
        Ok(password) => {
            tracing::debug!("Retrieved credential: {}", key.as_str());
            Ok(Some(password))
        }
        Err(keyring::Error::NoEntry) => {
            tracing::debug!("No credential found for: {}", key.as_str());
            Ok(None)
        }
        Err(e) => {
            tracing::warn!("Failed to retrieve credential {}: {}", key.as_str(), e);
            Err(anyhow::anyhow!("Failed to retrieve credential: {}", e))
        }
    }
}

/// Delete a credential from the system keychain
pub fn delete_credential(key: CredentialKey) -> Result<()> {
    let entry = Entry::new(SERVICE_NAME, key.as_str()).context("Failed to create keyring entry")?;

    match entry.delete_credential() {
        Ok(()) => {
            tracing::info!("Deleted credential: {}", key.as_str());
            Ok(())
        }
        Err(keyring::Error::NoEntry) => {
            // Already doesn't exist, that's fine
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!("Failed to delete credential: {}", e)),
    }
}

/// Check if a credential exists in the system keychain
pub fn has_credential(key: CredentialKey) -> bool {
    get_credential(key).map(|opt| opt.is_some()).unwrap_or(false)
}

// =============================================================================
// Generic API Key Helpers
// =============================================================================

/// Store an API key with validation based on credential type
fn store_api_key(key: CredentialKey, api_key: &str) -> Result<()> {
    if api_key.is_empty() {
        return Err(anyhow::anyhow!("API key cannot be empty"));
    }

    if let Some(prefix) = key.expected_prefix() {
        if !api_key.starts_with(prefix) {
            tracing::warn!(
                "{} doesn't start with '{}' - may be invalid",
                key.display_name(),
                prefix
            );
        }
    }

    store_credential(key, api_key)
}

/// Get masked display of any credential by key (for UI). Public wrapper over
/// the internal helper so callers can render a key without a per-key function.
pub fn get_credential_masked(key: CredentialKey) -> String {
    get_api_key_masked(key)
}

/// Get masked display of an API key (for UI)
fn get_api_key_masked(key: CredentialKey) -> String {
    match get_credential(key) {
        Ok(Some(value)) => {
            let visible = key.mask_visible_chars();
            if value.len() > visible {
                format!("{}••••••••", &value[..visible])
            } else {
                "••••••••".to_string()
            }
        }
        _ => "Not configured".to_string(),
    }
}

// =============================================================================
// Anthropic API Key (convenience wrappers)
// =============================================================================

pub fn store_anthropic_api_key(api_key: &str) -> Result<()> {
    store_api_key(CredentialKey::AnthropicApiKey, api_key)
}

pub fn get_anthropic_api_key() -> Result<Option<String>> {
    get_credential(CredentialKey::AnthropicApiKey)
}

pub fn has_anthropic_api_key() -> bool {
    has_credential(CredentialKey::AnthropicApiKey)
}

pub fn delete_anthropic_api_key() -> Result<()> {
    delete_credential(CredentialKey::AnthropicApiKey)
}

pub fn get_anthropic_api_key_masked() -> String {
    get_api_key_masked(CredentialKey::AnthropicApiKey)
}

// =============================================================================
// OpenAI API Key (convenience wrappers)
// =============================================================================

pub fn store_openai_api_key(api_key: &str) -> Result<()> {
    store_api_key(CredentialKey::OpenAiApiKey, api_key)
}

pub fn get_openai_api_key() -> Result<Option<String>> {
    get_credential(CredentialKey::OpenAiApiKey)
}

pub fn has_openai_api_key() -> bool {
    has_credential(CredentialKey::OpenAiApiKey)
}

pub fn delete_openai_api_key() -> Result<()> {
    delete_credential(CredentialKey::OpenAiApiKey)
}

pub fn get_openai_api_key_masked() -> String {
    get_api_key_masked(CredentialKey::OpenAiApiKey)
}

// =============================================================================
// Gemini API Key (convenience wrappers)
// =============================================================================

pub fn store_gemini_api_key(api_key: &str) -> Result<()> {
    store_api_key(CredentialKey::GeminiApiKey, api_key)
}

pub fn get_gemini_api_key() -> Result<Option<String>> {
    get_credential(CredentialKey::GeminiApiKey)
}

pub fn has_gemini_api_key() -> bool {
    has_credential(CredentialKey::GeminiApiKey)
}

pub fn delete_gemini_api_key() -> Result<()> {
    delete_credential(CredentialKey::GeminiApiKey)
}

pub fn get_gemini_api_key_masked() -> String {
    get_api_key_masked(CredentialKey::GeminiApiKey)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests interact with the real system keychain
    // They should be run manually, not in CI

    #[test]
    #[ignore] // Run with: cargo test -- --ignored
    fn test_store_and_retrieve() {
        let test_key = "sk-ant-test-key-12345";

        // Store
        store_anthropic_api_key(test_key).expect("Failed to store");

        // Retrieve
        let retrieved = get_anthropic_api_key().expect("Failed to get");
        assert_eq!(retrieved, Some(test_key.to_string()));

        // Check exists
        assert!(has_anthropic_api_key());

        // Delete
        delete_anthropic_api_key().expect("Failed to delete");

        // Verify deleted
        assert!(!has_anthropic_api_key());
    }
}

#[cfg(test)]
mod keychain_reference_tests {
    use super::*;

    /// Round-trip a `keychain:<service>` reference: write it the way the
    /// settings screen's Ctrl+K does, read it back the way
    /// [`crate::fleet::bridge::secrets`] does.
    ///
    /// `#[ignore]` because it writes a real item to the developer's login
    /// keychain — and because macOS has no default keychain at all when `HOME`
    /// is redirected, which is exactly what every tripwire and CI job does, so
    /// an always-on version would fail there for reasons unrelated to the code.
    /// Run it deliberately:
    ///
    /// ```text
    /// cargo test -p ainb --lib keychain_reference_round_trips -- --ignored
    /// security delete-generic-password -s ainb-test-secret-round-trip
    /// ```
    #[test]
    #[ignore = "writes to the real login keychain; run manually"]
    fn keychain_reference_round_trips() {
        let service = "ainb-test-secret-round-trip";
        store_keychain_secret(service, "xoxb-round-trip").expect("store");
        let resolved =
            crate::fleet::bridge::secrets::resolve_secret(&format!("keychain:{service}"));
        assert_eq!(resolved, "xoxb-round-trip");

        let entry = Entry::new(service, REFERENCE_ACCOUNT).unwrap();
        entry.delete_credential().expect("clean up the test item");
    }

    #[test]
    fn an_empty_secret_is_refused_rather_than_stored() {
        assert!(store_keychain_secret("ainb-test-never-written", "").is_err());
        assert!(store_keychain_secret("  ", "value").is_err());
    }
}
