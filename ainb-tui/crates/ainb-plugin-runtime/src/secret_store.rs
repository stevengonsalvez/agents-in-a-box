//! Host-side backend for `host/secret_store_get`.
//!
//! Reads a secret keyed by `(service, account)` from the platform secret
//! store. The capability gate (grant present, `service` on the allow-list)
//! is enforced by the caller in `plugin_task.rs`; this module only owns the
//! platform lookup and the cap-form-independent `service_allowed` helper.
//!
//! Platform split (per P3.5 + `build-plan.md` Open Risks row 4):
//!
//! - **macOS**: reads the generic password from the login Keychain via
//!   `security-framework`. A missing item maps to
//!   [`SecretStoreError::NotFound`].
//! - **linux / other**: the Secret Service backend is deferred, so every
//!   lookup returns [`SecretStoreError::NotImplemented`]. The capability,
//!   the wire contract, and the cap gate all still exist — the plugin gets a
//!   clean `-32005` and can fall back to dotenv.

use ainb_plugin_protocol::errors::RpcError;

/// Failure modes of a platform secret lookup.
///
/// These map 1:1 onto the JSON-RPC error codes the handler returns:
/// [`Self::NotFound`] → `-32004`, [`Self::NotImplemented`] → `-32005`.
#[derive(Debug)]
pub enum SecretStoreError {
    /// No secret exists for the requested `(service, account)` pair.
    NotFound,
    /// This platform has no secret-store backend wired up yet.
    NotImplemented,
    /// The backend returned an unexpected error (I/O, locked keychain, ...).
    Backend(String),
}

impl SecretStoreError {
    /// Convert into the wire [`RpcError`] for `(service, account)`.
    #[must_use]
    pub fn into_rpc(self, service: &str, account: &str) -> RpcError {
        match self {
            Self::NotFound => RpcError::secret_not_found(service, account),
            Self::NotImplemented => {
                RpcError::not_implemented("linux Secret Service deferred to P5")
            }
            Self::Backend(msg) => RpcError::new(
                ainb_plugin_protocol::errors::SECRET_NOT_FOUND,
                format!("secret store backend error: {msg}"),
            ),
        }
    }
}

/// Whether `requested` service is permitted by a `secret_store_get`
/// allow-list (list form). Comparison is exact — there is no wildcard /
/// prefix semantics for service names.
///
/// The caller handles the bool-true form (unconditional grant) and the
/// `is_granted()` gate separately; this only answers "does the allow-list
/// name this service?".
#[must_use]
pub fn service_allowed(allow_list: &[String], requested: &str) -> bool {
    allow_list.iter().any(|entry| entry == requested)
}

/// Read the secret bytes for `(service, account)` from the platform store.
///
/// macOS implementation: reads the generic password from the login
/// Keychain. Returns [`SecretStoreError::NotFound`] when the item is
/// absent.
#[cfg(target_os = "macos")]
pub fn get_secret(service: &str, account: &str) -> Result<Vec<u8>, SecretStoreError> {
    use security_framework::passwords::get_generic_password;
    match get_generic_password(service, account) {
        Ok(bytes) => Ok(bytes),
        Err(e) => {
            // errSecItemNotFound (-25300) is the "no such item" sentinel.
            if e.code() == -25300 {
                Err(SecretStoreError::NotFound)
            } else {
                Err(SecretStoreError::Backend(e.to_string()))
            }
        }
    }
}

/// Read the secret bytes for `(service, account)` from the platform store.
///
/// Non-macOS stub: the Secret Service backend is deferred, so this always
/// reports [`SecretStoreError::NotImplemented`] (`-32005` on the wire).
#[cfg(not(target_os = "macos"))]
pub fn get_secret(_service: &str, _account: &str) -> Result<Vec<u8>, SecretStoreError> {
    Err(SecretStoreError::NotImplemented)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_allowed_exact_match_only() {
        let allow = vec!["ainb-hangar".to_string(), "anthropic-api-key".to_string()];
        assert!(service_allowed(&allow, "ainb-hangar"));
        assert!(service_allowed(&allow, "anthropic-api-key"));
        assert!(!service_allowed(&allow, "openai-api-key"));
        // No prefix semantics.
        assert!(!service_allowed(&allow, "ainb-hangar-extra"));
        assert!(!service_allowed(&allow, "ainb"));
    }

    #[test]
    fn service_allowed_empty_list_denies() {
        assert!(!service_allowed(&[], "anything"));
    }

    #[test]
    fn not_found_maps_to_secret_not_found_code() {
        let e = SecretStoreError::NotFound.into_rpc("svc", "acct");
        assert_eq!(e.code, ainb_plugin_protocol::errors::SECRET_NOT_FOUND);
        assert!(e.message.contains("svc"));
        assert!(e.message.contains("acct"));
    }

    #[test]
    fn not_implemented_maps_to_not_implemented_code() {
        let e = SecretStoreError::NotImplemented.into_rpc("svc", "acct");
        assert_eq!(e.code, ainb_plugin_protocol::errors::NOT_IMPLEMENTED);
    }

    #[test]
    fn backend_error_maps_to_secret_not_found_code_with_message() {
        let e = SecretStoreError::Backend("locked".into()).into_rpc("svc", "acct");
        assert_eq!(e.code, ainb_plugin_protocol::errors::SECRET_NOT_FOUND);
        assert!(e.message.contains("locked"));
    }

    /// On non-macOS the stub always reports NotImplemented.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn get_secret_stub_is_not_implemented() {
        assert!(matches!(
            get_secret("svc", "acct"),
            Err(SecretStoreError::NotImplemented)
        ));
    }
}
