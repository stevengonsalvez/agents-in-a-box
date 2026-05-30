//! Process-local, in-memory [`SecretBackend`] for tests.
//!
//! Gated behind `#[cfg(any(test, feature = "test-backend"))]` so it never ships
//! in a release binary. Storage is a `Mutex<HashMap>` keyed by the same
//! `(service, key)` pair the real backends use, so scope isolation behaves
//! identically to the macOS backend.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::{Result, Scope, SecretBackend, SecretBytes};

/// An in-memory secret store. `Default::default()` yields an empty backend.
#[derive(Default)]
pub struct InMemoryBackend {
    // Keyed by `(service_string, key)` so distinct scopes never collide, exactly
    // as the per-platform service/account split does on macOS.
    entries: Mutex<HashMap<(String, String), Vec<u8>>>,
}

impl InMemoryBackend {
    /// Construct an empty in-memory backend.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretBackend for InMemoryBackend {
    fn get(&self, scope: &Scope, key: &str) -> Result<Option<SecretBytes>> {
        let lookup = (scope.service_string(), key.to_owned());
        let found = self
            .entries
            .lock()
            .expect("secret store mutex poisoned")
            .get(&lookup)
            .map(|bytes| SecretBytes::from(bytes.as_slice()));
        Ok(found)
    }

    fn put(&self, scope: &Scope, key: &str, value: &[u8]) -> Result<()> {
        self.entries
            .lock()
            .expect("secret store mutex poisoned")
            .insert((scope.service_string(), key.to_owned()), value.to_vec());
        Ok(())
    }

    fn delete(&self, scope: &Scope, key: &str) -> Result<()> {
        self.entries
            .lock()
            .expect("secret store mutex poisoned")
            .remove(&(scope.service_string(), key.to_owned()));
        Ok(())
    }
}
