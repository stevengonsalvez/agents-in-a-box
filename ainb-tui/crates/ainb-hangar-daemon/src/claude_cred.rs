//! Daemon-level `claude` credential resolution (one-time, host-wide).
//!
//! A confined `claude` child cannot authenticate: its credential is the macOS
//! Keychain item `Claude Code-credentials`, and the sandbox denies keychain
//! access (verified: `security find-generic-password` under the daemon's own
//! profile is DENIED). The child also cannot read the operator's `$HOME`, so it
//! finds no `~/.claude` credentials either — a confined run reports
//! `Not logged in · Please run /login` and, worse, **exits 0**.
//!
//! The daemon is NOT sandboxed (it is the parent, running as the operator), so
//! it resolves the credential itself and injects it into the child's env as
//! `CLAUDE_CODE_OAUTH_TOKEN`. The child gets ONE env var and no keychain access:
//! this is deliberately not a keychain grant to the child, which was rejected on
//! security review (a grant lets the agent shell out to `/usr/bin/security` and
//! read *any* always-allow item, e.g. Chrome Safe Storage).
//!
//! # Resolution order
//!
//! ```text
//! 1. $HANGAR_CLAUDE_OAUTH_TOKEN   env override (daemon's own env)
//! 2. SecretBackend(Global, claude.oauth_token)   the stored setup-token
//! 3. none -> claude fails with an actionable message
//! ```
//!
//! The override is read from [`ENV_OVERRIDE`], which is deliberately **NOT** on
//! the runner's `ENV_ALLOWLIST`: the daemon reads it, but it is never inherited
//! by any child. Only the resolved value is injected, and only into a `claude`
//! child ([`keys_for_backend`]) — a claude token has no business in a codex or
//! copilot subprocess even though the injection seam reaches every backend.
//!
//! # Token shape
//!
//! `claude setup-token` mints a long-lived token; the Keychain item's
//! `claudeAiOauth.accessToken` is a short-lived one (measured: 8h TTL, refreshed
//! by claude WRITING BACK to the Keychain, which a confined child cannot do).
//! Both are accepted by `CLAUDE_CODE_OAUTH_TOKEN`; storing the long-lived token
//! is what makes an unattended run survive past the 8h window.
//!
//! Nothing here ever logs the value: it is carried as
//! [`ainb_hangar_secrets::SecretBytes`], whose `Debug` redacts.

use std::collections::HashMap;

use ainb_hangar_secrets::{Scope, SecretBackend, SecretBytes};

use crate::runner::Backend;

/// Build the platform's secret backend for production use.
///
/// macOS resolves to the login-Keychain backend; linux to the Secret Service
/// stub, which reports `NotImplemented` until a real backend lands — in which
/// case [`resolve`] simply reports [`CredSource::None`] and the operator uses
/// [`ENV_OVERRIDE`]. Tests inject `InMemoryBackend` instead of calling this.
#[must_use]
pub fn default_backend() -> std::sync::Arc<dyn SecretBackend + Send + Sync> {
    #[cfg(target_os = "macos")]
    {
        std::sync::Arc::new(ainb_hangar_secrets::MacKeychainBackend::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::sync::Arc::new(ainb_hangar_secrets::LinuxSecretServiceBackend::new())
    }
}

/// The daemon-side env override that wins over the stored secret.
///
/// Namespaced `HANGAR_*` on purpose: it must NOT collide with the child-facing
/// [`CHILD_ENV_VAR`], and it must never be inherited by a provider subprocess
/// (it is absent from the runner's `ENV_ALLOWLIST`).
pub const ENV_OVERRIDE: &str = "HANGAR_CLAUDE_OAUTH_TOKEN";

/// The env var the `claude` CLI reads its OAuth token from. This is the ONLY
/// credential material the confined child receives.
pub const CHILD_ENV_VAR: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// The [`Scope::Global`] secret key holding the operator's claude token.
///
/// Global, not workspace-scoped: the credential is a property of the host's
/// operator, configured ONCE at daemon level. Per-agent credentials were
/// explicitly rejected (an operator would re-enter it for every agent).
pub const SECRET_KEY: &str = "claude.oauth_token";

/// Where a resolved credential came from — for operator-facing status only.
///
/// Carries no secret material, so it is freely loggable and rendered directly by
/// the Settings row and the CLI `status` verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredSource {
    /// Resolved from [`ENV_OVERRIDE`] in the daemon's environment.
    Env,
    /// Resolved from the platform secret store ([`SECRET_KEY`], [`Scope::Global`]).
    Store,
    /// No credential configured — a confined claude run WILL fail to authenticate.
    None,
}

impl CredSource {
    /// A short operator-facing label (`env override` / `configured` / `not set`).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Env => "env override",
            Self::Store => "configured",
            Self::None => "not set",
        }
    }

    /// Whether a credential is present at all.
    #[must_use]
    pub const fn is_configured(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Resolve the claude credential, preferring the env override over the store.
///
/// `daemon_env` is the daemon's own environment (NOT the child's). A store fault
/// (locked keychain, unimplemented platform) is treated as "absent" rather than
/// propagated: a credential lookup must never wedge dispatch, and the resulting
/// claude failure is loud and actionable on its own.
///
/// Returns the source alongside the material so a caller can render status
/// without touching the secret.
#[must_use]
pub fn resolve<S: std::hash::BuildHasher>(
    backend: &dyn SecretBackend,
    daemon_env: &HashMap<String, String, S>,
) -> (CredSource, Option<SecretBytes>) {
    if let Some(v) = daemon_env.get(ENV_OVERRIDE).filter(|v| !v.trim().is_empty()) {
        return (
            CredSource::Env,
            Some(SecretBytes::from(v.trim().as_bytes())),
        );
    }
    match backend.get(&Scope::Global, SECRET_KEY) {
        Ok(Some(bytes)) if !bytes.as_bytes().is_empty() => (CredSource::Store, Some(bytes)),
        _ => (CredSource::None, None),
    }
}

/// Report the configured source without materialising the secret into a caller.
#[must_use]
pub fn status<S: std::hash::BuildHasher>(
    backend: &dyn SecretBackend,
    daemon_env: &HashMap<String, String, S>,
) -> CredSource {
    resolve(backend, daemon_env).0
}

/// Store the operator's token (the one-time setup write).
///
/// # Errors
///
/// Returns the backend's [`ainb_hangar_secrets::SecretError`] when the platform
/// store rejects the write (locked, denied, or unimplemented).
pub fn store(backend: &dyn SecretBackend, token: &SecretBytes) -> ainb_hangar_secrets::Result<()> {
    backend.put(&Scope::Global, SECRET_KEY, token.as_bytes())
}

/// Remove the stored token. Idempotent.
///
/// # Errors
///
/// Returns the backend's [`ainb_hangar_secrets::SecretError`] on a store fault.
pub fn clear(backend: &dyn SecretBackend) -> ainb_hangar_secrets::Result<()> {
    backend.delete(&Scope::Global, SECRET_KEY)
}

/// The env var name (`sk-ant-oat…`) prefix that identifies a claude OAuth token
/// in `claude setup-token` output. Used to pick the token line out of the
/// interactive command's stdout.
const OAUTH_TOKEN_PREFIX: &str = "sk-ant-oat";

/// Extract the OAuth token from `claude setup-token` stdout.
///
/// `setup-token` prints human prose interspersed with the token; the token is
/// the last whitespace-delimited word starting with [`OAUTH_TOKEN_PREFIX`].
/// Pure + testable so the fragile parsing is covered without minting a real
/// token. Returns `None` when no token-shaped word is present (the caller then
/// reports a failed setup rather than storing garbage).
#[must_use]
pub fn extract_setup_token(stdout: &str) -> Option<String> {
    stdout
        .split_whitespace()
        .rev()
        .find(|w| w.starts_with(OAUTH_TOKEN_PREFIX) && w.len() > OAUTH_TOKEN_PREFIX.len())
        .map(str::to_string)
}

/// Convenience wrappers over the platform-default backend, so a CLI/TUI caller
/// needs no direct `ainb-hangar-secrets` dependency.
pub mod default {
    use super::{CredSource, SecretBytes, clear, default_backend, resolve, status, store};

    /// The configured credential source, resolved from the default backend and
    /// the process env.
    #[must_use]
    pub fn source() -> CredSource {
        let env: std::collections::HashMap<String, String> = std::env::vars().collect();
        status(default_backend().as_ref(), &env)
    }

    /// Whether resolving a credential succeeds AND yields non-empty material.
    #[must_use]
    pub fn is_present() -> bool {
        let env: std::collections::HashMap<String, String> = std::env::vars().collect();
        resolve(default_backend().as_ref(), &env).1.is_some()
    }

    /// Store `token` in the default backend. Bytes, never a `String` on argv.
    ///
    /// # Errors
    ///
    /// Propagates the backend's store fault (locked/denied/unimplemented).
    pub fn store_token(token: &[u8]) -> ainb_hangar_secrets::Result<()> {
        store(default_backend().as_ref(), &SecretBytes::from(token))
    }

    /// Remove the stored token from the default backend. Idempotent.
    ///
    /// # Errors
    ///
    /// Propagates the backend's store fault.
    pub fn clear_token() -> ainb_hangar_secrets::Result<()> {
        clear(default_backend().as_ref())
    }
}

/// The credential env pairs to inject for `backend_kind`, for the
/// `build_task_env` keychain seam.
///
/// **Claude only.** The seam injects on top of the ambient policy and reaches
/// every provider, so gating here is what keeps a claude token out of a codex /
/// copilot child. An unresolvable credential yields no pairs — claude then fails
/// loudly rather than the daemon silently substituting something.
#[must_use]
pub fn keys_for_backend<S: std::hash::BuildHasher>(
    kind: Backend,
    secrets: &dyn SecretBackend,
    daemon_env: &HashMap<String, String, S>,
) -> Vec<(String, String)> {
    if kind != Backend::Claude {
        return Vec::new();
    }
    let (_src, token) = resolve(secrets, daemon_env);
    token
        .map(|t| {
            vec![(
                CHILD_ENV_VAR.to_string(),
                String::from_utf8_lossy(t.as_bytes()).into_owned(),
            )]
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_secrets::InMemoryBackend;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    #[test]
    fn env_override_wins_over_the_store() {
        let b = InMemoryBackend::new();
        b.put(&Scope::Global, SECRET_KEY, b"stored").unwrap();
        let (src, tok) = resolve(&b, &env(&[(ENV_OVERRIDE, "from-env")]));
        assert_eq!(src, CredSource::Env);
        assert_eq!(tok.unwrap().as_bytes(), b"from-env");
    }

    #[test]
    fn falls_back_to_the_store_then_to_none() {
        let b = InMemoryBackend::new();
        assert_eq!(status(&b, &env(&[])), CredSource::None);
        store(&b, &SecretBytes::from(b"stored".as_slice())).unwrap();
        assert_eq!(status(&b, &env(&[])), CredSource::Store);
        clear(&b).unwrap();
        assert_eq!(status(&b, &env(&[])), CredSource::None);
    }

    #[test]
    fn blank_env_override_is_not_a_credential() {
        let b = InMemoryBackend::new();
        assert_eq!(status(&b, &env(&[(ENV_OVERRIDE, "   ")])), CredSource::None);
    }

    #[test]
    fn only_the_claude_backend_receives_the_token() {
        let b = InMemoryBackend::new();
        store(&b, &SecretBytes::from(b"tok".as_slice())).unwrap();
        let e = env(&[]);
        let claude = keys_for_backend(Backend::Claude, &b, &e);
        assert_eq!(claude, vec![(CHILD_ENV_VAR.to_string(), "tok".to_string())]);
        // A claude credential must never reach another provider's subprocess.
        assert!(keys_for_backend(Backend::Codex, &b, &e).is_empty());
        assert!(keys_for_backend(Backend::Copilot, &b, &e).is_empty());
    }

    #[test]
    fn no_credential_injects_nothing() {
        let b = InMemoryBackend::new();
        assert!(keys_for_backend(Backend::Claude, &b, &env(&[])).is_empty());
    }

    #[test]
    fn extract_setup_token_picks_the_token_word() {
        let out = "Visit https://claude.ai/oauth to authorize.\nYour token:\n  sk-ant-oat01-ABCdef123\nDone.";
        assert_eq!(
            extract_setup_token(out).as_deref(),
            Some("sk-ant-oat01-ABCdef123")
        );
        // No token-shaped word -> None (never store prose).
        assert_eq!(extract_setup_token("authorization was cancelled"), None);
        // The bare prefix with no body is not a token.
        assert_eq!(extract_setup_token("sk-ant-oat"), None);
    }

    #[test]
    fn secret_material_never_appears_in_debug_output() {
        let (_s, tok) = resolve(&InMemoryBackend::new(), &env(&[(ENV_OVERRIDE, "hunter2")]));
        let rendered = format!("{:?}", tok.unwrap());
        assert!(
            !rendered.contains("hunter2"),
            "secret leaked into Debug: {rendered}"
        );
    }
}
