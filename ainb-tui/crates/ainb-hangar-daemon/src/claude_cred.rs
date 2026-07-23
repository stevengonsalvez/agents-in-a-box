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
//! 2. /usr/bin/security → the SYSTEM claude login item ("Claude Code-credentials")
//! 3. SecretBackend(Global, claude.oauth_token)   the legacy stored setup-token
//! 4. none -> claude fails with an actionable message
//! ```
//!
//! The override is read from [`ENV_OVERRIDE`], which is deliberately **NOT** on
//! the runner's `ENV_ALLOWLIST`: the daemon reads it, but it is never inherited
//! by any child. Only the resolved value is injected, and only into a `claude`
//! child ([`keys_for_backend`]) — a claude token has no business in a codex or
//! copilot subprocess even though the injection seam reaches every backend.
//!
//! # Why the system item is read by SHELLING OUT to `/usr/bin/security`
//!
//! Step 2 uses the operator's EXISTING `claude` login — the OAuth item Claude
//! Code itself stores under the generic-password service `Claude Code-credentials`
//! — so no separate setup step is needed. It is read by spawning
//! `/usr/bin/security find-generic-password -s "Claude Code-credentials" -w`
//! rather than by an in-process `SecItemCopyMatching`, and that choice is the
//! whole fix for the recurring password prompt.
//!
//! A macOS Keychain ACL grants trust to the *requesting binary* by its code
//! signature. The daemon is an unsigned dev binary whose code hash changes on
//! every `just dev` / release rebuild, so it is never a stable trusted app: an
//! in-process read prompts for the login-keychain password on EVERY launch, and
//! "Always Allow" never sticks (the next rebuild is a different binary). By
//! reading through `/usr/bin/security` — Apple-signed with a stable designated
//! requirement — the ACL trust attaches to `security`, so a single "Always Allow"
//! sticks FOREVER, across daemon rebuilds and `brew upgrade`s alike. One prompt
//! ever, for both `just dev` and an installed `ainb` (measured: once trusted, the
//! read returns in ~0.02s with no prompt).
//!
//! The read is bounded off the async worker by the caller (`resolve_cred_env`'s
//! 5s timeout in `run_loop`), so a still-untrusted first read that raises the GUI
//! prompt cannot wedge dispatch: on timeout the daemon proceeds tokenless and
//! claude fails loudly rather than the task hanging at `running`.
//!
//! # Token shape
//!
//! The `Claude Code-credentials` item's value is a JSON blob
//! `{"claudeAiOauth":{"accessToken":"sk-ant-oat…",…}}`; the daemon extracts
//! `claudeAiOauth.accessToken` (see [`extract_access_token`]). That access token
//! is short-lived (measured: 8h TTL, refreshed by claude WRITING BACK to the
//! Keychain), but the daemon re-reads it fresh on every dispatch, so an expiring
//! token self-heals as long as the operator's `claude` login is current. A
//! long-lived `claude setup-token` value in [`ENV_OVERRIDE`] (step 1) still wins
//! for a fully unattended host. Both shapes are accepted by
//! `CLAUDE_CODE_OAUTH_TOKEN`.
//!
//! Nothing here ever logs the value: it is carried as
//! [`ainb_hangar_secrets::SecretBytes`], whose `Debug` redacts.

use std::collections::HashMap;

use ainb_hangar_secrets::{Scope, SecretBackend, SecretBytes};

/// Re-export so a CLI/TUI caller can hold token material in the zeroizing
/// [`SecretBytes`] newtype without a direct `ainb-hangar-secrets` dependency.
pub use ainb_hangar_secrets::SecretBytes as TokenBytes;

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
    /// Resolved from the operator's SYSTEM `claude` login via `/usr/bin/security`
    /// (the [`SYSTEM_CLAUDE_KEYCHAIN_SERVICE`] item).
    System,
    /// Resolved from the platform secret store ([`SECRET_KEY`], [`Scope::Global`]).
    Store,
    /// No credential configured — a confined claude run WILL fail to authenticate.
    None,
}

impl CredSource {
    /// A short operator-facing label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Env => "env override",
            Self::System => "system claude login",
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

/// Resolve the claude credential: env override, then the SYSTEM claude login,
/// then the legacy store.
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
    resolve_with(backend, daemon_env, read_system_claude_oauth)
}

/// [`resolve`] with an injectable system-login reader, so unit tests exercise the
/// chain WITHOUT shelling out to `/usr/bin/security` (which would read the test
/// machine's real login).
///
/// The chain is env override → `read_system` → legacy store → none. `read_system`
/// is lazy (`FnOnce`): the env override short-circuits before it is ever called,
/// so the common unattended-token case does no subprocess work.
pub(crate) fn resolve_with<S, F>(
    backend: &dyn SecretBackend,
    daemon_env: &HashMap<String, String, S>,
    read_system: F,
) -> (CredSource, Option<SecretBytes>)
where
    S: std::hash::BuildHasher,
    F: FnOnce() -> Option<SecretBytes>,
{
    if let Some(v) = daemon_env.get(ENV_OVERRIDE).filter(|v| !v.trim().is_empty()) {
        return (
            CredSource::Env,
            Some(SecretBytes::from(v.trim().as_bytes())),
        );
    }
    if let Some(tok) = read_system().filter(|t| !t.as_bytes().is_empty()) {
        return (CredSource::System, Some(tok));
    }
    match backend.get(&Scope::Global, SECRET_KEY) {
        Ok(Some(bytes)) if !bytes.as_bytes().is_empty() => (CredSource::Store, Some(bytes)),
        _ => (CredSource::None, None),
    }
}

/// The generic-password service name Claude Code stores the operator's login in.
///
/// Probed against Claude Code 2.1.x on macOS: the item's value is a JSON blob
/// `{"claudeAiOauth":{"accessToken":…}}`.
pub const SYSTEM_CLAUDE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Read the operator's system `claude` OAuth access token by shelling out to
/// `/usr/bin/security` (macOS only; `None` elsewhere).
///
/// Shelling out — rather than an in-process `SecItemCopyMatching` — is deliberate:
/// the Apple-signed, stable `security` binary is what the Keychain ACL trusts, so
/// a one-time "Always Allow" sticks across daemon rebuilds. See the module docs.
///
/// Best-effort: a non-zero exit (operator not logged in), non-UTF-8 output, or a
/// value with no extractable token all yield `None`, and the resolver falls
/// through to the legacy store. Never logs the value.
///
/// Gated `not(test)`: a unit test must NEVER shell out to the real
/// `/usr/bin/security` (it would read the developer's own login and make the
/// test machine-dependent). The chain is exercised through the injectable
/// [`resolve_with`] / [`keys_for_backend_with`] readers, the parsing through
/// [`extract_access_token`], and the REAL subprocess by the `live_e2e`
/// integration test (which links this lib without `cfg(test)`).
#[must_use]
fn read_system_claude_oauth() -> Option<SecretBytes> {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        let out = std::process::Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-s",
                SYSTEM_CLAUDE_KEYCHAIN_SERVICE,
                "-w",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let raw = String::from_utf8(out.stdout).ok()?;
        let token = extract_access_token(&raw)?;
        // The system access token has an ~8h TTL and the confined child cannot
        // refresh it (claude refreshes by writing BACK to the Keychain). If it is
        // already expired, still inject it — claude is the authority and clock
        // skew shouldn't cause a false skip — but log a CLEAR, actionable hint so
        // the ensuing auth failure is diagnosable rather than mysterious.
        warn_if_system_token_expired(&raw);
        Some(SecretBytes::from(token.as_bytes()))
    }
    #[cfg(not(all(target_os = "macos", not(test))))]
    {
        None
    }
}

/// Log an actionable warning when the system claude token is already expired.
///
/// Split out (and taking the raw value, not the token) so the reader stays
/// linear; the expiry math itself is covered by [`expires_at_ms`].
#[cfg(all(target_os = "macos", not(test)))]
fn warn_if_system_token_expired(raw: &str) {
    let Some(expires_at) = expires_at_ms(raw) else {
        return;
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    if now_ms > 0 && expires_at < now_ms {
        let ago_h = (now_ms - expires_at) / 3_600_000;
        tracing::warn!(
            expired_hours_ago = ago_h,
            "system claude login token has expired; the dispatch will fail to \
             authenticate. Open Claude Code to refresh your login, or set \
             HANGAR_CLAUDE_OAUTH_TOKEN to a long-lived `claude setup-token` value \
             for an unattended daemon."
        );
    }
}

/// Extract the claude OAuth access token from a `security -w` value.
///
/// Claude Code stores a JSON blob `{"claudeAiOauth":{"accessToken":"sk-ant-oat…"}}`,
/// so the token is `claudeAiOauth.accessToken`. A value that is not JSON is
/// treated as a raw token string (a defensive path for any install that stores
/// the token bare). A JSON value missing the expected field yields `None` (never
/// a stray sub-string). Pure + testable so the parsing is covered without a real
/// login.
// On a non-macOS build the subprocess reader is compiled out, so this helper is
// reached only by its own unit test; silence the dead-code lint there.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[must_use]
fn extract_access_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        // Not JSON — some installs may store the token bare.
        return Some(trimmed.to_string());
    };
    v.get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(serde_json::Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Extract `claudeAiOauth.expiresAt` (epoch milliseconds) from a `security -w`
/// value, or `None` for a raw / fieldless value.
///
/// Probed against Claude Code 2.1.x: `expiresAt` is an integer epoch-ms. Pure +
/// testable; used only to decide whether to warn about an expired login.
// Reached only by the macOS reader (and its own test); silence dead-code elsewhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[must_use]
fn expires_at_ms(raw: &str) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    v.get("claudeAiOauth")
        .and_then(|o| o.get("expiresAt"))
        .and_then(serde_json::Value::as_i64)
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
    keys_for_backend_with(kind, secrets, daemon_env, read_system_claude_oauth)
}

/// [`keys_for_backend`] with an injectable system-login reader, so unit tests
/// exercise the injection seam without shelling out to `/usr/bin/security`.
#[must_use]
pub(crate) fn keys_for_backend_with<S, F>(
    kind: Backend,
    secrets: &dyn SecretBackend,
    daemon_env: &HashMap<String, String, S>,
    read_system: F,
) -> Vec<(String, String)>
where
    S: std::hash::BuildHasher,
    F: FnOnce() -> Option<SecretBytes>,
{
    if kind != Backend::Claude {
        return Vec::new();
    }
    let Some(token) = resolve_with(secrets, daemon_env, read_system).1 else {
        return Vec::new();
    };
    // A claude OAuth token is ASCII (`sk-ant-oat…`); a non-UTF-8 value means a
    // corrupted store entry. Inject NOTHING rather than a lossily-mangled token —
    // claude then fails to authenticate loudly (a wrong token is a silent 401),
    // which is the correct outcome for corruption. The error is logged without the
    // value.
    match std::str::from_utf8(token.as_bytes()) {
        Ok(s) => vec![(CHILD_ENV_VAR.to_string(), s.to_string())],
        Err(e) => {
            tracing::error!(
                error = %e,
                "stored claude credential is not valid UTF-8 (corrupted?); injecting nothing"
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_secrets::InMemoryBackend;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// A system reader that finds no login — the isolation used by the store /
    /// none fallback tests so they never shell out to `/usr/bin/security`.
    fn no_system() -> Option<SecretBytes> {
        None
    }

    /// A system reader returning a fixed token, standing in for a logged-in
    /// operator without touching the real Keychain.
    fn some_system(tok: &'static [u8]) -> impl FnOnce() -> Option<SecretBytes> {
        move || Some(SecretBytes::from(tok))
    }

    #[test]
    fn env_override_wins_over_system_and_store() {
        let b = InMemoryBackend::new();
        b.put(&Scope::Global, SECRET_KEY, b"stored").unwrap();
        let (src, tok) = resolve_with(&b, &env(&[(ENV_OVERRIDE, "from-env")]), some_system(b"sys"));
        assert_eq!(src, CredSource::Env);
        assert_eq!(tok.unwrap().as_bytes(), b"from-env");
    }

    #[test]
    fn system_login_wins_over_the_store() {
        let b = InMemoryBackend::new();
        b.put(&Scope::Global, SECRET_KEY, b"stored").unwrap();
        let (src, tok) = resolve_with(&b, &env(&[]), some_system(b"sk-ant-oat-system"));
        assert_eq!(src, CredSource::System);
        assert_eq!(tok.unwrap().as_bytes(), b"sk-ant-oat-system");
    }

    #[test]
    fn falls_back_to_the_store_then_to_none() {
        let b = InMemoryBackend::new();
        assert_eq!(resolve_with(&b, &env(&[]), no_system).0, CredSource::None);
        store(&b, &SecretBytes::from(b"stored".as_slice())).unwrap();
        assert_eq!(resolve_with(&b, &env(&[]), no_system).0, CredSource::Store);
        clear(&b).unwrap();
        assert_eq!(resolve_with(&b, &env(&[]), no_system).0, CredSource::None);
    }

    #[test]
    fn blank_env_override_is_not_a_credential() {
        let b = InMemoryBackend::new();
        assert_eq!(
            resolve_with(&b, &env(&[(ENV_OVERRIDE, "   ")]), no_system).0,
            CredSource::None
        );
    }

    #[test]
    fn only_the_claude_backend_receives_the_token() {
        let b = InMemoryBackend::new();
        store(&b, &SecretBytes::from(b"tok".as_slice())).unwrap();
        let e = env(&[]);
        let claude = keys_for_backend_with(Backend::Claude, &b, &e, no_system);
        assert_eq!(claude, vec![(CHILD_ENV_VAR.to_string(), "tok".to_string())]);
        // A claude credential must never reach another provider's subprocess.
        assert!(keys_for_backend_with(Backend::Codex, &b, &e, no_system).is_empty());
        assert!(keys_for_backend_with(Backend::Copilot, &b, &e, no_system).is_empty());
    }

    #[test]
    fn system_login_is_injected_for_claude_only() {
        let b = InMemoryBackend::new();
        let e = env(&[]);
        // No env override, no store — the system login is the resolved source.
        let claude = keys_for_backend_with(Backend::Claude, &b, &e, some_system(b"sk-ant-oat-sys"));
        assert_eq!(
            claude,
            vec![(CHILD_ENV_VAR.to_string(), "sk-ant-oat-sys".to_string())]
        );
        assert!(
            keys_for_backend_with(Backend::Codex, &b, &e, some_system(b"sk-ant-oat-sys"))
                .is_empty()
        );
    }

    #[test]
    fn no_credential_injects_nothing() {
        let b = InMemoryBackend::new();
        assert!(keys_for_backend_with(Backend::Claude, &b, &env(&[]), no_system).is_empty());
    }

    /// A [`SecretBackend`] whose every operation panics — proves a code path
    /// resolved a credential WITHOUT touching the store (the macOS login
    /// Keychain). Paired with a panicking system reader, it proves the env
    /// override reads NEITHER the system item nor the store.
    struct PanicBackend;
    impl SecretBackend for PanicBackend {
        fn get(&self, _s: &Scope, _k: &str) -> ainb_hangar_secrets::Result<Option<SecretBytes>> {
            panic!("the store must NOT be read when a higher-precedence source resolves");
        }
        fn put(&self, _s: &Scope, _k: &str, _v: &[u8]) -> ainb_hangar_secrets::Result<()> {
            panic!("put must not be called");
        }
        fn delete(&self, _s: &Scope, _k: &str) -> ainb_hangar_secrets::Result<()> {
            panic!("delete must not be called");
        }
    }

    #[test]
    fn env_override_reads_neither_system_nor_store() {
        let e = env(&[(ENV_OVERRIDE, "sk-ant-oat-from-env")]);
        // Panicking system reader + panicking backend: the env override must
        // short-circuit before either is touched. This is the no-keychain-prompt
        // guarantee for the unattended-token path.
        let (src, tok) = resolve_with(&PanicBackend, &e, || panic!("system reader must not run"));
        assert_eq!(src, CredSource::Env);
        assert_eq!(tok.unwrap().as_bytes(), b"sk-ant-oat-from-env");
    }

    #[test]
    fn system_login_is_not_overridden_by_the_store_read() {
        // System login present -> the store (PanicBackend) must never be read.
        let (src, _tok) = resolve_with(&PanicBackend, &env(&[]), some_system(b"sk-ant-oat-sys"));
        assert_eq!(src, CredSource::System);
    }

    #[test]
    fn extract_access_token_reads_the_json_blob() {
        // The real `Claude Code-credentials` value shape.
        let blob = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-ABC","refreshToken":"r","expiresAt":123}}"#;
        assert_eq!(
            extract_access_token(blob).as_deref(),
            Some("sk-ant-oat01-ABC")
        );
        // Trailing whitespace (security -w appends a newline) is tolerated.
        assert_eq!(
            extract_access_token("  {\"claudeAiOauth\":{\"accessToken\":\"tok\"}}\n").as_deref(),
            Some("tok")
        );
        // A bare (non-JSON) token is returned verbatim — the defensive path.
        assert_eq!(
            extract_access_token("sk-ant-oat-raw\n").as_deref(),
            Some("sk-ant-oat-raw")
        );
        // JSON without the expected field -> None (never a stray sub-string).
        assert_eq!(extract_access_token(r#"{"other":1}"#), None);
        assert_eq!(
            extract_access_token(r#"{"claudeAiOauth":{"accessToken":""}}"#),
            None
        );
        // Empty output -> None.
        assert_eq!(extract_access_token("   \n"), None);
    }

    #[test]
    fn expires_at_ms_reads_the_epoch_millis_field() {
        // The real value shape: expiresAt is an integer epoch-ms.
        let blob = r#"{"claudeAiOauth":{"accessToken":"t","expiresAt":1784852992972}}"#;
        assert_eq!(expires_at_ms(blob), Some(1_784_852_992_972));
        // Missing field / non-JSON / raw token -> None (no expiry known).
        assert_eq!(
            expires_at_ms(r#"{"claudeAiOauth":{"accessToken":"t"}}"#),
            None
        );
        assert_eq!(expires_at_ms("sk-ant-oat-raw"), None);
        assert_eq!(expires_at_ms(""), None);
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
