//! Host-side logic for `host/secret_store_get`.
//!
//! The wire contract is `(scope, key)`-addressed: a request names a `scope`
//! (`"workspace"` — requiring `workspace_id` — or `"global"`) and a `key`.
//! The actual platform lookup is delegated to a
//! [`ainb_hangar_secrets::SecretBackend`] injected into the runtime (DI), so
//! production reads the macOS Keychain (or the linux Secret Service stub)
//! while tests use the in-memory double.
//!
//! This module owns the cap-form-independent helpers so they can be unit
//! tested without spinning up a plugin subprocess:
//!
//! - [`key_allowed`] — does the `secrets:read` allow-list name this `key`?
//! - [`parse_scope`] — turn `(scope, workspace_id)` into a [`Scope`].
//! - [`secret_store_get_logic`] — the full cap-gate + backend lookup +
//!   base64 encode, returning the wire result or an [`RpcError`].
//!
//! Capability gating is enforced HERE (grant present + `key` on the
//! allow-list), matching the runtime's other host caps — never by omitting
//! the method from the dispatcher.

use std::sync::Arc;

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_secrets::{Scope, SecretBackend, SecretError};
use ainb_plugin_protocol::errors::RpcError;
use ainb_plugin_protocol::manifest::CapabilityGrant;
use ainb_plugin_protocol::params::{SecretStoreGetParams, SecretStoreGetResult};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::Value;

/// A shared, thread-safe handle to the host's secret backend.
///
/// `host/secret_store_get` is dispatched on the per-plugin task; every task
/// holds a clone of this `Arc` so a single backend (e.g. one Keychain
/// session) is shared across all plugins. Tests swap in an
/// `ainb_hangar_secrets::InMemoryBackend` here.
pub type SharedSecretBackend = Arc<dyn SecretBackend + Send + Sync>;

/// Build the platform-default secret backend for production.
///
/// - macOS: the login-Keychain backend (`MacKeychainBackend`).
/// - linux: the Secret Service stub (`LinuxSecretServiceBackend`), which
///   reports `NotImplemented` until the D-Bus backend lands.
/// - any other target: a `NotImplemented` stub so the runtime still builds.
#[must_use]
pub fn default_backend() -> SharedSecretBackend {
    #[cfg(target_os = "macos")]
    {
        Arc::new(ainb_hangar_secrets::MacKeychainBackend::new())
    }
    #[cfg(target_os = "linux")]
    {
        Arc::new(ainb_hangar_secrets::LinuxSecretServiceBackend::new())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Arc::new(UnsupportedSecretBackend)
    }
}

/// Fallback backend for non-macOS, non-linux targets: every op is
/// `NotImplemented`. Keeps the runtime building on exotic targets without
/// pulling a platform secret store dependency.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
struct UnsupportedSecretBackend;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl SecretBackend for UnsupportedSecretBackend {
    fn get(
        &self,
        _scope: &Scope,
        _key: &str,
    ) -> ainb_hangar_secrets::Result<Option<ainb_hangar_secrets::SecretBytes>> {
        Err(SecretError::NotImplemented)
    }
    fn put(&self, _scope: &Scope, _key: &str, _value: &[u8]) -> ainb_hangar_secrets::Result<()> {
        Err(SecretError::NotImplemented)
    }
    fn delete(&self, _scope: &Scope, _key: &str) -> ainb_hangar_secrets::Result<()> {
        Err(SecretError::NotImplemented)
    }
}

/// Whether `requested` key is permitted by a `secrets:read` allow-list
/// (list form). Comparison is exact — there is no wildcard / prefix
/// semantics for key names.
///
/// The caller handles the bool-true form (unconditional grant) and the
/// `is_granted()` gate separately; this only answers "does the allow-list
/// name this key?".
#[must_use]
pub fn key_allowed(allow_list: &[String], requested: &str) -> bool {
    allow_list.iter().any(|entry| entry == requested)
}

/// Map a `SecretError` from the backend onto the wire [`RpcError`] for the
/// requested `(scope, key)`.
///
/// The variants map onto distinct codes so the plugin can branch its
/// recovery: a locked backend (`-32006`) is retriable after unlock, denied
/// access (`-32007`) needs the user to re-authorize, `NotImplemented`
/// (`-32005`) is a permanent platform gap the plugin degrades around, and a
/// genuine miss (`-32004`) is just "no such secret".
fn secret_error_into_rpc(err: &SecretError, scope_label: &str, key: &str) -> RpcError {
    match err {
        SecretError::NotFound => RpcError::secret_not_found(scope_label, key),
        SecretError::NotImplemented => {
            RpcError::not_implemented("platform secret store backend not implemented")
        }
        SecretError::BackendLocked => RpcError::secret_backend_locked(scope_label, key),
        SecretError::AccessDenied => RpcError::secret_access_denied(scope_label, key),
        SecretError::Backend(msg) => RpcError::new(
            ainb_plugin_protocol::errors::SECRET_NOT_FOUND,
            format!("secret store backend error: {msg}"),
        ),
    }
}

/// Parse a `(scope, workspace_id)` wire pair into a [`Scope`].
///
/// `"workspace"` requires a non-empty `workspace_id`; `"global"` ignores it.
/// Any other scope string, or a workspace scope with a missing/empty id, is
/// an [`RpcError::invalid_params`] (`-32602`).
///
/// # Errors
///
/// Returns [`RpcError`] (`-32602`) for an unknown scope or a workspace scope
/// with a missing/empty `workspace_id`.
pub fn parse_scope(scope: &str, workspace_id: Option<&str>) -> Result<Scope, RpcError> {
    match scope {
        "global" => Ok(Scope::Global),
        "workspace" => {
            let raw = workspace_id.ok_or_else(|| {
                RpcError::invalid_params("scope=\"workspace\" requires workspace_id")
            })?;
            let id = WorkspaceId::from_str(raw)
                .map_err(|e| RpcError::invalid_params(format!("invalid workspace_id: {e}")))?;
            Ok(Scope::Workspace(id))
        }
        other => Err(RpcError::invalid_params(format!(
            "unknown scope: {other:?} (expected \"workspace\" or \"global\")"
        ))),
    }
}

/// The full `host/secret_store_get` handler logic, decoupled from the
/// plugin subprocess so it can be unit-tested with an in-memory backend.
///
/// Stages (every gate returns BEFORE any backend hit):
/// 1. The `secrets:read` grant must be present (`is_granted`); otherwise
///    `-32001`.
/// 2. For a list-form grant, the requested `key` must be on the allow-list;
///    otherwise `-32001`. A bool-true grant reads any key (the danger
///    surface) and skips the allow-list check.
/// 3. Parse `(scope, workspace_id)` into a [`Scope`] (`-32602` on bad input).
/// 4. Look the secret up via the injected [`SecretBackend`]. A miss
///    (`Ok(None)`) maps to `-32004`; a backend error maps per
///    [`secret_error_into_rpc`].
///
/// On success the secret bytes are base64-encoded into `value`.
///
/// # Errors
///
/// Returns [`RpcError`] for a denied capability (`-32001`), bad params
/// (`-32602`), a miss (`-32004`), or any backend failure (`-32005`/`-32006`/
/// `-32007`/`-32004`).
pub fn secret_store_get_logic(
    grant: &CapabilityGrant,
    backend: &dyn SecretBackend,
    params: &SecretStoreGetParams,
) -> Result<Value, RpcError> {
    // Stage 1: cap present at all.
    if !grant.is_granted() {
        return Err(RpcError::capability_denied("secrets:read"));
    }
    // Stage 2: list form = key allow-list; bool-true = any key.
    if let Some(allow) = grant.allow_list() {
        if !key_allowed(allow, &params.key) {
            return Err(RpcError::capability_denied("secrets:read")
                .with_data(serde_json::json!({ "key": params.key })));
        }
    }
    // Stage 3: resolve the scope.
    let scope = parse_scope(&params.scope, params.workspace_id.as_deref())?;
    let scope_label = scope.service_string();
    // Stage 4: platform lookup via the injected backend.
    let bytes = backend
        .get(&scope, &params.key)
        .map_err(|e| secret_error_into_rpc(&e, &scope_label, &params.key))?
        .ok_or_else(|| RpcError::secret_not_found(&scope_label, &params.key))?;
    let res = SecretStoreGetResult {
        value: B64.encode(bytes.as_bytes()),
    };
    Ok(serde_json::to_value(res).expect("SecretStoreGetResult serializable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_allowed_exact_match_only() {
        let allow = vec!["anthropic_api_key".to_string(), "openai_api_key".to_string()];
        assert!(key_allowed(&allow, "anthropic_api_key"));
        assert!(key_allowed(&allow, "openai_api_key"));
        assert!(!key_allowed(&allow, "github_token"));
        // No prefix semantics.
        assert!(!key_allowed(&allow, "anthropic_api_key_extra"));
        assert!(!key_allowed(&allow, "anthropic"));
    }

    #[test]
    fn key_allowed_empty_list_denies() {
        assert!(!key_allowed(&[], "anything"));
    }

    #[test]
    fn parse_scope_global_ignores_workspace_id() {
        assert_eq!(parse_scope("global", None).unwrap(), Scope::Global);
        // Even if a workspace_id is supplied, global ignores it.
        assert_eq!(parse_scope("global", Some("ws-1")).unwrap(), Scope::Global);
    }

    #[test]
    fn parse_scope_workspace_requires_id() {
        let err = parse_scope("workspace", None).unwrap_err();
        assert_eq!(err.code, ainb_plugin_protocol::errors::INVALID_PARAMS);
    }

    #[test]
    fn parse_scope_workspace_rejects_empty_id() {
        let err = parse_scope("workspace", Some("")).unwrap_err();
        assert_eq!(err.code, ainb_plugin_protocol::errors::INVALID_PARAMS);
    }

    #[test]
    fn parse_scope_workspace_builds_scope() {
        let s = parse_scope("workspace", Some("ws-123")).unwrap();
        match s {
            Scope::Workspace(id) => assert_eq!(id.as_str(), "ws-123"),
            Scope::Global => panic!("expected workspace scope"),
        }
    }

    #[test]
    fn parse_scope_unknown_is_invalid_params() {
        let err = parse_scope("nonsense", None).unwrap_err();
        assert_eq!(err.code, ainb_plugin_protocol::errors::INVALID_PARAMS);
    }

    #[test]
    fn secret_error_mapping_codes() {
        use ainb_plugin_protocol::errors;
        assert_eq!(
            secret_error_into_rpc(&SecretError::NotFound, "global", "k").code,
            errors::SECRET_NOT_FOUND
        );
        assert_eq!(
            secret_error_into_rpc(&SecretError::NotImplemented, "global", "k").code,
            errors::NOT_IMPLEMENTED
        );
        assert_eq!(
            secret_error_into_rpc(&SecretError::BackendLocked, "global", "k").code,
            errors::SECRET_BACKEND_LOCKED
        );
        assert_eq!(
            secret_error_into_rpc(&SecretError::AccessDenied, "global", "k").code,
            errors::SECRET_ACCESS_DENIED
        );
        assert_eq!(
            secret_error_into_rpc(&SecretError::Backend("io".into()), "global", "k").code,
            errors::SECRET_NOT_FOUND
        );
    }
}
