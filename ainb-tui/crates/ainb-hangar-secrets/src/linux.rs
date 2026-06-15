//! Linux Secret Service backend — **stub**.
//!
//! A real implementation would talk to the freedesktop Secret Service over
//! D-Bus, but that is out of scope for P5.1. Until it lands, every operation
//! returns [`SecretError::NotImplemented`] so the daemon can surface a clear
//! "no secret store on this platform" message and fall back to its documented
//! dotenv path rather than silently losing secrets.

use crate::{Result, Scope, SecretBackend, SecretBytes, SecretError};

/// Placeholder Linux backend. Every method returns
/// [`SecretError::NotImplemented`].
#[derive(Default)]
pub struct LinuxSecretServiceBackend;

impl LinuxSecretServiceBackend {
    /// Construct the stub backend.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl SecretBackend for LinuxSecretServiceBackend {
    fn get(&self, _scope: &Scope, _key: &str) -> Result<Option<SecretBytes>> {
        Err(SecretError::NotImplemented)
    }

    fn put(&self, _scope: &Scope, _key: &str, _value: &[u8]) -> Result<()> {
        Err(SecretError::NotImplemented)
    }

    fn delete(&self, _scope: &Scope, _key: &str) -> Result<()> {
        Err(SecretError::NotImplemented)
    }
}
