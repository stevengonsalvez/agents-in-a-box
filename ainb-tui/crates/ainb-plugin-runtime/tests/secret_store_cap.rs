//! P5.2 — `host/secret_store_get` cap integration (scope/key contract).
//!
//! These tests exercise the runtime's cap-gate + scope parse + injected
//! `SecretBackend` lookup + base64 encode directly via the testable
//! `secret_store_get_logic` seam, using an in-memory backend (no plugin
//! subprocess, no real keychain). The end-to-end subprocess path (including
//! the macOS Keychain read with an anti-cheat sentinel) is covered by the
//! CTS A18 golden canary.

use ainb_hangar_secrets::{InMemoryBackend, Scope, SecretBackend};
use ainb_plugin_protocol::errors;
use ainb_plugin_protocol::manifest::CapabilityGrant;
use ainb_plugin_protocol::methods::ALL_METHODS;
use ainb_plugin_protocol::params::{SecretStoreGetParams, SecretStoreGetResult};
use ainb_plugin_runtime::secret_store::secret_store_get_logic;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

/// A global-scope read of `key`.
fn global_params(key: &str) -> SecretStoreGetParams {
    SecretStoreGetParams {
        scope: "global".into(),
        workspace_id: None,
        key: key.into(),
    }
}

/// RED 1: a plugin whose manifest omits the `secrets:read` capability gets
/// `-32001` (capability denied) before any backend hit.
#[test]
fn plugin_without_capability_gets_32001() {
    // Default grant = denied (Bool(false)).
    let grant = CapabilityGrant::default();
    // Seed the backend so the ONLY thing that can deny is the cap gate.
    let backend = InMemoryBackend::new();
    backend
        .put(&Scope::Global, "anthropic_api_key", b"seeded")
        .unwrap();

    let err = secret_store_get_logic(&grant, &backend, &global_params("anthropic_api_key"))
        .expect_err("cap-omitted read must be denied");
    assert_eq!(
        err.code,
        errors::CAPABILITY_DENIED,
        "cap-omitted secret read must return -32001"
    );
}

/// RED 2: a plugin granted `secrets:read` reads a pre-seeded secret; the
/// `value` field is base64 and decodes to exactly the seeded bytes.
#[test]
fn plugin_with_capability_reads_existing_secret() {
    let secret = b"super-secret-token";
    let backend = InMemoryBackend::new();
    backend
        .put(&Scope::Global, "anthropic_api_key", secret)
        .unwrap();

    // List-form grant whitelisting exactly this key.
    let grant = CapabilityGrant::List(vec!["anthropic_api_key".into()]);

    let value =
        secret_store_get_logic(&grant, &backend, &global_params("anthropic_api_key")).unwrap();
    let res: SecretStoreGetResult = serde_json::from_value(value).unwrap();

    // The wire field is `value` (base64); decode and compare to the seed.
    let decoded = B64.decode(res.value.as_bytes()).expect("value must be base64");
    assert_eq!(decoded, secret, "decoded secret must match the seeded bytes");
}

/// RED 2b: a workspace-scoped read isolates by workspace — a secret seeded
/// under one workspace is not readable under a different scope, and IS
/// readable under the matching scope.
#[test]
fn plugin_reads_workspace_scoped_secret_with_isolation() {
    let ws_a = Scope::Workspace(
        ainb_hangar_core::ids::WorkspaceId::from_str("ws-aaa").unwrap(),
    );
    let backend = InMemoryBackend::new();
    backend.put(&ws_a, "anthropic_api_key", b"ws-a-secret").unwrap();

    let grant = CapabilityGrant::List(vec!["anthropic_api_key".into()]);

    // Matching workspace scope reads it back.
    let ok = secret_store_get_logic(
        &grant,
        &backend,
        &SecretStoreGetParams {
            scope: "workspace".into(),
            workspace_id: Some("ws-aaa".into()),
            key: "anthropic_api_key".into(),
        },
    )
    .unwrap();
    let res: SecretStoreGetResult = serde_json::from_value(ok).unwrap();
    assert_eq!(B64.decode(res.value).unwrap(), b"ws-a-secret");

    // A different workspace scope must NOT see it (isolation) → -32004.
    let miss = secret_store_get_logic(
        &grant,
        &backend,
        &SecretStoreGetParams {
            scope: "workspace".into(),
            workspace_id: Some("ws-bbb".into()),
            key: "anthropic_api_key".into(),
        },
    )
    .expect_err("cross-workspace read must miss");
    assert_eq!(miss.code, errors::SECRET_NOT_FOUND);
}

/// RED 3: a granted read of a key that has no stored secret returns
/// `-32004 SECRET_NOT_FOUND`.
#[test]
fn plugin_reads_missing_secret_gets_not_found() {
    let backend = InMemoryBackend::new(); // empty
    let grant = CapabilityGrant::List(vec!["openai_api_key".into()]);

    let err = secret_store_get_logic(&grant, &backend, &global_params("openai_api_key"))
        .expect_err("missing secret must error");
    assert_eq!(
        err.code,
        errors::SECRET_NOT_FOUND,
        "absent secret must return -32004"
    );
}

/// RED 4: the cap is READ-only. There is no `host/secret_store_set` method
/// in the protocol's method registry, so a plugin holding `secrets:read`
/// has no write path to abuse.
#[test]
fn plugin_cannot_write_via_get_cap() {
    // The router dispatches only methods in ALL_METHODS; none is a secret
    // store *write*.
    assert!(
        !ALL_METHODS
            .iter()
            .any(|m| m.contains("secret_store_set") || m.contains("secret_store_put")),
        "the secret store cap is read-only: no write method may exist in the registry"
    );
    // And the one secret method that DOES exist is the read.
    assert!(
        ALL_METHODS.contains(&"host/secret_store_get"),
        "the read method must be registered"
    );
    // Granting the cap and issuing the read never mutates the backend: a
    // miss stays a miss after a (read-only) get attempt.
    let backend = InMemoryBackend::new();
    let grant = CapabilityGrant::Bool(true);
    let _ = secret_store_get_logic(&grant, &backend, &global_params("k"));
    assert!(
        backend.get(&Scope::Global, "k").unwrap().is_none(),
        "a get must not create an entry — the cap has no write side effect"
    );
}
