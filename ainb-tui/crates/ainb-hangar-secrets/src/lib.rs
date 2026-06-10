//! Hangar OS keychain bridge.
//!
//! This crate provides a single, IO-narrow abstraction — [`SecretBackend`] —
//! over whatever per-platform credential store is available, so the rest of the
//! Hangar control plane can read and write API keys without knowing whether
//! they live in the macOS Keychain, a (future) Linux Secret Service, or an
//! in-process test double.
//!
//! ## Backends
//!
//! | Platform | Backend | Storage |
//! |----------|---------|---------|
//! | macOS    | [`MacKeychainBackend`] | Keychain Services generic passwords |
//! | Linux    | [`LinuxSecretServiceBackend`] | not yet implemented — every call returns [`SecretError::NotImplemented`] |
//! | tests    | [`InMemoryBackend`] | a process-local `HashMap` |
//!
//! ## Scoping
//!
//! Every secret is addressed by a ([`Scope`], `key`) pair. The scope maps to a
//! Keychain *service* string of the form `ainb-hangar::{scope_id}` (see
//! [`Scope::service_string`]); the `key` maps to the Keychain *account*. This
//! keeps per-workspace secrets isolated from each other and from
//! [`Scope::Global`] secrets even when they share a `key` name.
//!
//! ## Zeroization
//!
//! Secret material is carried in [`SecretBytes`], which zeroes its backing
//! buffer on drop so plaintext keys do not linger in freed heap memory.

/// The errors a [`SecretBackend`] can return.
///
/// The variants are intentionally distinct so the daemon UX can recover
/// differently per failure mode: a locked keychain prompts the user to unlock,
/// a denied prompt surfaces a "permission needed" banner, a missing entry is a
/// normal `None`-shaped miss for the caller, and `NotImplemented` is a
/// permanent platform gap rather than a transient error.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The backend exists for this platform but has not been implemented yet
    /// (currently only the Linux Secret Service backend).
    #[error("secret backend not implemented on this platform")]
    NotImplemented,

    /// The underlying credential store is locked (e.g. a macOS Keychain that
    /// the user has not unlocked). Retrying after the user unlocks may succeed.
    #[error("secret backend is locked")]
    BackendLocked,

    /// The user (or the OS) denied access to the credential store, e.g. by
    /// dismissing the Keychain authorization prompt.
    #[error("access to the secret backend was denied")]
    AccessDenied,

    /// The requested entry does not exist. Distinct from `Ok(None)` only where
    /// a backend op (e.g. `delete`) treats a miss as an error; `get` maps a
    /// miss to `Ok(None)`.
    #[error("secret not found")]
    NotFound,

    /// Any other backend failure, carrying the platform-specific message.
    #[error("secret backend error: {0}")]
    Backend(String),
}

/// The result type returned by every [`SecretBackend`] operation.
pub type Result<T> = std::result::Result<T, SecretError>;

/// The addressing scope for a secret.
///
/// A scope plus a `key` uniquely identifies a stored secret. Distinct scopes
/// never collide even when they share a `key`, because the scope is folded into
/// the backend's service string (see [`Scope::service_string`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// A secret owned by a single workspace.
    Workspace(ainb_hangar_core::ids::WorkspaceId),
    /// A secret shared across all workspaces on this host.
    Global,
}

impl Scope {
    /// The stable id fragment for this scope, used to build the service string.
    ///
    /// [`Scope::Global`] renders as the literal `global`; a workspace scope
    /// renders as its (non-empty, by construction) id string.
    #[must_use]
    pub fn scope_id(&self) -> &str {
        match self {
            Self::Workspace(id) => id.as_str(),
            Self::Global => "global",
        }
    }

    /// The Keychain *service* string for this scope: `ainb-hangar::{scope_id}`.
    ///
    /// This is the user-visible service under which entries appear in
    /// `security find-generic-password -s '<service>'`, so it is part of the
    /// crate's external contract and is asserted by tests.
    #[must_use]
    pub fn service_string(&self) -> String {
        format!("ainb-hangar::{}", self.scope_id())
    }
}

/// Owned secret bytes that are zeroed when dropped.
///
/// Use this for any plaintext key material so the backing buffer does not
/// linger in freed heap memory after the value goes out of scope. Equality and
/// `Debug` are implemented to support tests; `Debug` deliberately redacts the
/// contents so secrets never leak into logs or panic messages.
///
/// Note: the derived [`PartialEq`] is **not** constant-time and must not be
/// used to verify a caller-supplied secret against a stored one — that is a
/// token-verification concern handled separately with
/// [`subtle::ConstantTimeEq`](https://docs.rs/subtle). `SecretBytes` models key
/// material at rest (fetched and injected), never a comparison target.
#[derive(Clone, PartialEq, Eq, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// Borrow the raw secret bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for SecretBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl From<&[u8]> for SecretBytes {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

impl std::fmt::Debug for SecretBytes {
    /// Redact the contents so secret material never reaches a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SecretBytes").field(&"<redacted>").finish()
    }
}

/// A per-platform credential store.
///
/// Implementors map a ([`Scope`], `key`) pair onto whatever the platform store
/// offers. `get` returns `Ok(None)` for a missing entry (not an error); `put`
/// upserts; `delete` removes an entry (and treats a missing entry as success so
/// the operation is idempotent).
pub trait SecretBackend {
    /// Fetch a secret. Returns `Ok(None)` when no entry exists for
    /// `(scope, key)`.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError`] when the backend is locked, access is denied, or
    /// the platform backend is not implemented.
    fn get(&self, scope: &Scope, key: &str) -> Result<Option<SecretBytes>>;

    /// Store (upsert) a secret for `(scope, key)`.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError`] when the backend is locked, access is denied, or
    /// the platform backend is not implemented.
    fn put(&self, scope: &Scope, key: &str, value: &[u8]) -> Result<()>;

    /// Delete the secret for `(scope, key)`. Idempotent: deleting a
    /// non-existent entry succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError`] when the backend is locked, access is denied, or
    /// the platform backend is not implemented.
    fn delete(&self, scope: &Scope, key: &str) -> Result<()>;
}

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub use mac::MacKeychainBackend;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxSecretServiceBackend;

#[cfg(any(test, feature = "test-backend"))]
mod memory;
#[cfg(any(test, feature = "test-backend"))]
pub use memory::InMemoryBackend;
