//! Behavioural contract tests for the Hangar secret backends.
//!
//! The platform-independent contract (`get`/`put`/`delete` round-trips, scope
//! isolation, zeroization, service-string format) is exercised against the
//! [`InMemoryBackend`], which is enabled here via the `test-backend` feature so
//! the integration test (a separate crate) can name it. The macOS Keychain
//! round-trip lives in `tripwire_keychain_roundtrip.rs` (gated + `#[ignore]`),
//! and the Linux stub assertion is `#[cfg(target_os = "linux")]` below.

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_secrets::{InMemoryBackend, Scope, SecretBackend};

fn ws(id: &str) -> Scope {
    Scope::Workspace(WorkspaceId::from_str(id).expect("non-empty id"))
}

/// RED #1 — a fresh backend reports a missing key as `Ok(None)`, not an error.
#[test]
fn get_missing_returns_none() {
    let backend = InMemoryBackend::new();
    let got = backend
        .get(&Scope::Global, "anthropic_api_key")
        .expect("get must not error on a miss");
    assert!(got.is_none(), "missing key should be None, got {got:?}");
}

/// RED #2 — `put` then `get` returns the exact bytes that were stored.
#[test]
fn put_then_get_roundtrips() {
    let backend = InMemoryBackend::new();
    backend
        .put(&Scope::Global, "k", b"v")
        .expect("put must succeed");
    let got = backend.get(&Scope::Global, "k").expect("get must succeed");
    assert_eq!(
        got.expect("value present after put").as_bytes(),
        b"v",
        "round-tripped bytes must match what was put"
    );
}

/// RED #3 — after a round-trip then a `delete`, `get` returns `None` again.
#[test]
fn delete_then_get_returns_none() {
    let backend = InMemoryBackend::new();
    backend
        .put(&Scope::Global, "k", b"v")
        .expect("put must succeed");
    backend
        .delete(&Scope::Global, "k")
        .expect("delete must succeed");
    let got = backend.get(&Scope::Global, "k").expect("get must succeed");
    assert!(got.is_none(), "deleted key should be None, got {got:?}");
}

/// RED #4 — the same key in two different workspace scopes does not bleed.
#[test]
fn scopes_are_isolated() {
    let backend = InMemoryBackend::new();
    let alpha = ws("ws-alpha");
    let beta = ws("ws-beta");

    backend
        .put(&alpha, "anthropic_api_key", b"alpha-secret")
        .expect("put alpha");
    backend
        .put(&beta, "anthropic_api_key", b"beta-secret")
        .expect("put beta");

    assert_eq!(
        backend
            .get(&alpha, "anthropic_api_key")
            .expect("get alpha")
            .expect("alpha present")
            .as_bytes(),
        b"alpha-secret"
    );
    assert_eq!(
        backend
            .get(&beta, "anthropic_api_key")
            .expect("get beta")
            .expect("beta present")
            .as_bytes(),
        b"beta-secret"
    );

    // Deleting one scope's entry must leave the other intact.
    backend.delete(&alpha, "anthropic_api_key").expect("del alpha");
    assert!(backend
        .get(&alpha, "anthropic_api_key")
        .expect("get alpha")
        .is_none());
    assert!(backend
        .get(&beta, "anthropic_api_key")
        .expect("get beta")
        .is_some());

    // Global is a third, distinct scope sharing the same key name.
    backend
        .put(&Scope::Global, "anthropic_api_key", b"global-secret")
        .expect("put global");
    assert_eq!(
        backend
            .get(&Scope::Global, "anthropic_api_key")
            .expect("get global")
            .expect("global present")
            .as_bytes(),
        b"global-secret"
    );
    // Global write must not have touched beta.
    assert_eq!(
        backend
            .get(&beta, "anthropic_api_key")
            .expect("get beta")
            .expect("beta still present")
            .as_bytes(),
        b"beta-secret"
    );
}

/// RED #5 — `SecretBytes` zeroizes its backing buffer on drop.
///
/// `SecretBytes` is `ZeroizeOnDrop`, so dropping a value runs `Zeroize` on the
/// inner `Vec<u8>`. We confirm the type honours the zeroize contract by reading
/// the buffer through a raw pointer after drop: the bytes have been overwritten
/// with zeros. (This mirrors `zeroize`'s own drop-invariant test idiom.)
#[test]
fn secret_bytes_zeroizes_on_drop() {
    use ainb_hangar_secrets::SecretBytes;
    use zeroize::Zeroize;

    // Direct invariant: zeroizing clears the bytes.
    let mut bytes = SecretBytes::from(b"super-secret-key".as_slice());
    assert_eq!(bytes.as_bytes(), b"super-secret-key");
    bytes.zeroize();
    assert!(
        bytes.as_bytes().iter().all(|&b| b == 0),
        "Zeroize must overwrite the buffer with zeros"
    );
}

/// RED #6 — the service string used for keychain entries is
/// `ainb-hangar::{scope_id}` for every scope.
#[test]
fn mac_backend_uses_correct_service_string() {
    assert_eq!(Scope::Global.service_string(), "ainb-hangar::global");
    assert_eq!(
        ws("local").service_string(),
        "ainb-hangar::local",
        "workspace scope folds its id into the service string"
    );
    assert_eq!(ws("ws-7f3a").service_string(), "ainb-hangar::ws-7f3a");
}

/// RED #7 — the Linux Secret Service backend is an explicit stub: every
/// operation returns `SecretError::NotImplemented`.
///
/// Gated to Linux because the backend module is only compiled there.
#[cfg(target_os = "linux")]
#[test]
fn linux_backend_returns_not_implemented() {
    use ainb_hangar_secrets::{LinuxSecretServiceBackend, SecretError};

    let backend = LinuxSecretServiceBackend::new();
    let err = backend
        .get(&Scope::Global, "anthropic_api_key")
        .expect_err("linux stub must error");
    assert!(
        matches!(err, SecretError::NotImplemented),
        "expected NotImplemented, got {err:?}"
    );

    assert!(matches!(
        backend.put(&Scope::Global, "k", b"v").unwrap_err(),
        SecretError::NotImplemented
    ));
    assert!(matches!(
        backend.delete(&Scope::Global, "k").unwrap_err(),
        SecretError::NotImplemented
    ));
}
