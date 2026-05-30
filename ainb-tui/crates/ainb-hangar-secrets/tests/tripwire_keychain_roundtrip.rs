//! Tripwire: real macOS Keychain round-trip (P5.1).
//!
//! This drives the actual `MacKeychainBackend` against the user's login
//! keychain, so it is **`#[ignore]`** by default — running it can pop a
//! Keychain authorization prompt and mutates the real keychain. It is the
//! manual-acceptance proof referenced by the phase plan; CI and the default
//! `cargo test` run never touch the real keychain (the `backend.rs` contract
//! tests against `InMemoryBackend` are the automated authority).
//!
//! Run manually on a dev mac with:
//!
//! ```text
//! cargo test -p ainb-hangar-secrets --test tripwire_keychain_roundtrip -- --ignored
//! ```
//!
//! After a `put`, the entry is visible via:
//!
//! ```text
//! security find-generic-password -s 'ainb-hangar::local' -a 'anthropic_api_key'
//! ```

#![cfg(target_os = "macos")]

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_secrets::{MacKeychainBackend, Scope, SecretBackend};

#[test]
#[ignore = "touches the real macOS Keychain; run manually with --ignored"]
fn keychain_roundtrip() {
    let backend = MacKeychainBackend::new();
    let scope = Scope::Workspace(WorkspaceId::from_str("local").expect("non-empty id"));
    let key = "anthropic_api_key";
    let secret = b"sk-ant-tripwire-roundtrip";

    // Clean any stale entry from a previous aborted run (idempotent delete).
    backend.delete(&scope, key).expect("pre-clean delete");

    // Miss before write.
    assert!(
        backend.get(&scope, key).expect("get pre-write").is_none(),
        "key must be absent before the round-trip"
    );

    // Write, then read back the exact bytes. The entry is now visible under
    // `security find-generic-password -s 'ainb-hangar::local' -a 'anthropic_api_key'`.
    backend.put(&scope, key, secret).expect("put");
    let got = backend.get(&scope, key).expect("get post-write");
    assert_eq!(
        got.expect("value present after put").as_bytes(),
        secret,
        "keychain round-trip must preserve the secret bytes"
    );

    // Delete, then confirm the miss.
    backend.delete(&scope, key).expect("delete");
    assert!(
        backend.get(&scope, key).expect("get post-delete").is_none(),
        "key must be absent after delete"
    );
}
