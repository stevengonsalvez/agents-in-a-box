//! macOS Keychain [`SecretBackend`].
//!
//! A thin wrapper over `security-framework`'s generic-password API. Each secret
//! is stored as a Keychain *generic password* whose *service* is the scope's
//! [`Scope::service_string`] (`ainb-hangar::{scope_id}`) and whose *account* is
//! the `key`. We deliberately use no access groups: Hangar is single-user on a
//! dev mac, so the default keychain access is exactly the semantics we want.
//!
//! ## Error mapping
//!
//! The `OSStatus` codes the Keychain returns are mapped onto the distinct
//! [`SecretError`] variants the daemon UX branches on:
//!
//! | `OSStatus` | meaning | maps to |
//! |------------|---------|---------|
//! | `errSecItemNotFound` (-25300) | no such entry | `get` → `Ok(None)`, others → [`SecretError::NotFound`] |
//! | `errSecInteractionNotAllowed` (-25308) | keychain locked, no UI allowed | [`SecretError::BackendLocked`] |
//! | `errSecAuthFailed` (-25293) | auth failed | [`SecretError::AccessDenied`] |
//! | `errSecUserCanceled` (-128) | user dismissed the prompt | [`SecretError::AccessDenied`] |
//! | anything else | — | [`SecretError::Backend`] with the OS message |

use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

use crate::{Result, Scope, SecretBackend, SecretBytes, SecretError};

// `security-framework-sys` re-exports only a subset of the `errSec*` codes, so
// the two we branch on but that are not re-exported are pinned here by value
// against Apple's stable `SecBase.h` constants.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
const ERR_SEC_AUTH_FAILED: i32 = -25293;
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
const ERR_SEC_USER_CANCELED: i32 = -128;

/// Map a `security-framework` error onto our domain error, preserving the
/// distinctions the daemon recovers from.
fn map_err(err: security_framework::base::Error) -> SecretError {
    match err.code() {
        ERR_SEC_ITEM_NOT_FOUND => SecretError::NotFound,
        ERR_SEC_INTERACTION_NOT_ALLOWED => SecretError::BackendLocked,
        ERR_SEC_AUTH_FAILED | ERR_SEC_USER_CANCELED => SecretError::AccessDenied,
        _ => SecretError::Backend(err.to_string()),
    }
}

/// A [`SecretBackend`] backed by Keychain Services generic passwords.
#[derive(Default)]
pub struct MacKeychainBackend;

impl MacKeychainBackend {
    /// Construct the macOS Keychain backend.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl SecretBackend for MacKeychainBackend {
    fn get(&self, scope: &Scope, key: &str) -> Result<Option<SecretBytes>> {
        let service = scope.service_string();
        match get_generic_password(&service, key) {
            Ok(bytes) => Ok(Some(SecretBytes::from(bytes))),
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(err) => Err(map_err(err)),
        }
    }

    fn put(&self, scope: &Scope, key: &str, value: &[u8]) -> Result<()> {
        let service = scope.service_string();
        // `set_generic_password` upserts: it overwrites an existing entry, so no
        // separate delete-then-add is needed.
        set_generic_password(&service, key, value).map_err(map_err)
    }

    fn delete(&self, scope: &Scope, key: &str) -> Result<()> {
        let service = scope.service_string();
        match delete_generic_password(&service, key) {
            Ok(()) => Ok(()),
            // Idempotent delete: a missing entry is success, not an error.
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(err) => Err(map_err(err)),
        }
    }
}
