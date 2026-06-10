//! Integration tests for the `pat` / `daemon_token` repositories (P5.4).
//!
//! Proves the storage discipline against a real `SQLite` database: minting
//! returns a one-time plaintext, only the SHA-256 digest is persisted (the
//! plaintext never appears in any column), verification round-trips by
//! re-hashing, tampering is rejected, `lookup-by-hash` works, and revoke removes
//! the row so verification then returns `None`. Determinism comes from a frozen
//! clock + seeded id generator + seeded RNG, so no field but the
//! invariant-checked random body is left to chance.

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::idgen::FixedIdGen;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::token::{DaemonTokenRepo, PatRepo, mint_daemon_token, mint_pat};
use rand::SeedableRng;
use rand::rngs::StdRng;
use sqlx::Row;

const FIXED_NOW: i64 = 1_700_000_000_000;

/// Seed a `user` row (the `pat.user_id` FK), returning its id.
async fn seed_user(store: &Store) -> String {
    let user = "user-1";
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind(user)
        .bind("a@example.com")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert user");
    user.to_string()
}

/// Seed the workspace -> `agent_runtime` FK chain (the `daemon_token.runtime_id`
/// FK), returning the runtime id.
async fn seed_runtime(store: &Store) -> String {
    let ws = "ws-1";
    let runtime = "rt-1";
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(ws)
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");
    sqlx::query(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(runtime)
    .bind(ws)
    .bind("daemon-1")
    .bind("claude")
    .bind("local")
    .bind("online")
    .execute(store.pool())
    .await
    .expect("insert agent_runtime");
    runtime.to_string()
}

fn seeded_rng() -> StdRng {
    StdRng::seed_from_u64(0xBEEF_0504)
}

#[tokio::test]
async fn mint_pat_stores_only_sha256_never_plaintext() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let user_id = seed_user(&store).await;

    let clock = FixedClock(FIXED_NOW);
    let idgen = FixedIdGen::new(vec!["pat-1".into()]);
    let mut rng = seeded_rng();

    let (record, minted) = mint_pat(
        store.pool(),
        &user_id,
        Some("read"),
        &clock,
        &idgen,
        &mut rng,
    )
    .await
    .expect("mint pat");

    // Plaintext carries the user-facing prefix and is shown once.
    assert!(minted.plaintext.starts_with("ainb_"));
    // The persisted digest equals sha256(plaintext) and is NOT the plaintext.
    assert_eq!(record.sha256_token, minted.sha256_hex);
    assert_ne!(record.sha256_token, minted.plaintext);
    assert_eq!(record.created_at, FIXED_NOW);
    assert_eq!(record.scope.as_deref(), Some("read"));
    assert_eq!(record.last_used, None);

    // Scan every text column of the row: the plaintext must appear nowhere.
    let row = sqlx::query("SELECT id, user_id, sha256_token, scope FROM pat WHERE id = ?")
        .bind("pat-1")
        .fetch_one(store.pool())
        .await
        .expect("fetch pat row");
    for col in ["id", "user_id", "sha256_token", "scope"] {
        let value: Option<String> = row.try_get(col).expect("read column");
        if let Some(v) = value {
            assert!(
                !v.contains(&minted.plaintext),
                "column {col} leaked the plaintext token"
            );
        }
    }
    // And the stored hash is plain hex, not the token body.
    let stored: String = row.try_get("sha256_token").expect("hash col");
    assert_eq!(stored.len(), 64);
    assert!(stored.bytes().all(|b| b.is_ascii_hexdigit()));
}

#[tokio::test]
async fn mint_pat_distinct_each_call() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let user_id = seed_user(&store).await;

    let clock = FixedClock(FIXED_NOW);
    let idgen = FixedIdGen::new(vec!["pat-a".into(), "pat-b".into()]);
    let mut rng = seeded_rng();

    let (a_rec, a) = mint_pat(store.pool(), &user_id, None, &clock, &idgen, &mut rng)
        .await
        .expect("mint a");
    let (b_rec, b) = mint_pat(store.pool(), &user_id, None, &clock, &idgen, &mut rng)
        .await
        .expect("mint b");

    assert_ne!(a.plaintext, b.plaintext, "each mint is a distinct token");
    assert_ne!(a_rec.sha256_token, b_rec.sha256_token);
}

#[tokio::test]
async fn verify_accepts_minted_and_rejects_tampered() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let user_id = seed_user(&store).await;

    let clock = FixedClock(FIXED_NOW);
    let idgen = FixedIdGen::new(vec!["pat-1".into()]);
    let mut rng = seeded_rng();

    let (record, minted) = mint_pat(store.pool(), &user_id, None, &clock, &idgen, &mut rng)
        .await
        .expect("mint pat");

    // Round-trip: the exact plaintext verifies to the same row.
    let verified = PatRepo::verify(store.pool(), &minted.plaintext)
        .await
        .expect("verify")
        .expect("token present");
    assert_eq!(verified, record);

    // Tamper: flip the last character -> no match.
    let mut tampered = minted.plaintext.clone();
    let last = tampered.pop().expect("non-empty");
    tampered.push(if last == 'A' { 'B' } else { 'A' });
    assert!(
        PatRepo::verify(store.pool(), &tampered).await.expect("verify").is_none(),
        "tampered token must not verify"
    );

    // Unknown token entirely -> no match.
    assert!(
        PatRepo::verify(store.pool(), "ainb_TOTALLYNOTAREALTOKEN")
            .await
            .expect("verify")
            .is_none()
    );
}

#[tokio::test]
async fn get_by_hash_finds_minted_pat() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let user_id = seed_user(&store).await;

    let clock = FixedClock(FIXED_NOW);
    let idgen = FixedIdGen::new(vec!["pat-1".into()]);
    let mut rng = seeded_rng();

    let (record, _minted) = mint_pat(store.pool(), &user_id, None, &clock, &idgen, &mut rng)
        .await
        .expect("mint pat");

    let found = PatRepo::get_by_hash(store.pool(), &record.sha256_token)
        .await
        .expect("lookup")
        .expect("row present");
    assert_eq!(found, record);

    assert!(
        PatRepo::get_by_hash(store.pool(), &"f".repeat(64))
            .await
            .expect("lookup")
            .is_none()
    );
}

#[tokio::test]
async fn revoke_pat_removes_row_and_verify_returns_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let user_id = seed_user(&store).await;

    let clock = FixedClock(FIXED_NOW);
    let idgen = FixedIdGen::new(vec!["pat-1".into()]);
    let mut rng = seeded_rng();

    let (record, minted) = mint_pat(store.pool(), &user_id, None, &clock, &idgen, &mut rng)
        .await
        .expect("mint pat");

    assert!(PatRepo::revoke(store.pool(), &record.id).await.expect("revoke"));
    // Second revoke is a no-op (already gone).
    assert!(!PatRepo::revoke(store.pool(), &record.id).await.expect("revoke"));

    assert!(
        PatRepo::verify(store.pool(), &minted.plaintext)
            .await
            .expect("verify")
            .is_none(),
        "revoked token must not verify"
    );
}

#[tokio::test]
async fn touch_last_used_stamps_timestamp() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let user_id = seed_user(&store).await;

    let clock = FixedClock(FIXED_NOW);
    let idgen = FixedIdGen::new(vec!["pat-1".into()]);
    let mut rng = seeded_rng();

    let (record, _minted) = mint_pat(store.pool(), &user_id, None, &clock, &idgen, &mut rng)
        .await
        .expect("mint pat");
    assert_eq!(record.last_used, None);

    let later = FIXED_NOW + 60_000;
    PatRepo::touch_last_used(store.pool(), &record.id, later).await.expect("touch");

    let refreshed = PatRepo::get_by_hash(store.pool(), &record.sha256_token)
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(refreshed.last_used, Some(later));
}

#[tokio::test]
async fn list_by_user_returns_only_owned_pats() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let user_id = seed_user(&store).await;

    let clock = FixedClock(FIXED_NOW);
    let idgen = FixedIdGen::new(vec!["pat-1".into(), "pat-2".into()]);
    let mut rng = seeded_rng();

    mint_pat(store.pool(), &user_id, None, &clock, &idgen, &mut rng)
        .await
        .expect("mint 1");
    mint_pat(store.pool(), &user_id, None, &clock, &idgen, &mut rng)
        .await
        .expect("mint 2");

    let listed = PatRepo::list_by_user(store.pool(), &user_id).await.expect("list");
    assert_eq!(listed.len(), 2);
    // The list must not carry a plaintext (the type has no such field — proven
    // structurally — but assert the digest is hex to confirm it is the hash).
    for rec in &listed {
        assert_eq!(rec.sha256_token.len(), 64);
    }

    assert!(PatRepo::list_by_user(store.pool(), "nobody").await.expect("list").is_empty());
}

#[tokio::test]
async fn mint_daemon_token_format_and_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let runtime_id = seed_runtime(&store).await;

    let clock = FixedClock(FIXED_NOW);
    let idgen = FixedIdGen::new(vec!["dt-1".into()]);
    let mut rng = seeded_rng();

    let (record, minted) = mint_daemon_token(store.pool(), &runtime_id, &clock, &idgen, &mut rng)
        .await
        .expect("mint daemon token");

    assert!(minted.plaintext.starts_with("mdt_"));
    assert_eq!(record.sha256_token, minted.sha256_hex);
    assert_ne!(record.sha256_token, minted.plaintext);
    assert_eq!(record.runtime_id, runtime_id);
    assert_eq!(record.created_at, FIXED_NOW);

    let verified = DaemonTokenRepo::verify(store.pool(), &minted.plaintext)
        .await
        .expect("verify")
        .expect("present");
    assert_eq!(verified, record);
}

#[tokio::test]
async fn revoke_daemon_token_removes_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let runtime_id = seed_runtime(&store).await;

    let clock = FixedClock(FIXED_NOW);
    let idgen = FixedIdGen::new(vec!["dt-1".into()]);
    let mut rng = seeded_rng();

    let (record, minted) = mint_daemon_token(store.pool(), &runtime_id, &clock, &idgen, &mut rng)
        .await
        .expect("mint daemon token");

    assert!(DaemonTokenRepo::revoke(store.pool(), &record.id).await.expect("revoke"));
    assert!(
        DaemonTokenRepo::verify(store.pool(), &minted.plaintext)
            .await
            .expect("verify")
            .is_none()
    );
}

/// `SocketTokenRepo` (migration 0011): a fresh database has no socket token,
/// `set` upserts the single row, `verify` accepts the matching plaintext and
/// rejects a tampered one, and a re-mint replaces the stored digest so the old
/// plaintext stops verifying.
#[tokio::test]
async fn socket_token_set_verify_and_replace() {
    use ainb_hangar_core::token::{TokenKind, mint};
    use ainb_hangar_store::repo::token::SocketTokenRepo;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");

    // Fresh database: nothing stored, nothing verifies.
    assert!(SocketTokenRepo::get(store.pool()).await.expect("get").is_none());
    assert!(
        !SocketTokenRepo::verify(store.pool(), "mdt_ANYTHING").await.expect("verify"),
        "no stored token must verify nothing"
    );

    let mut rng = seeded_rng();
    let first = mint(TokenKind::Daemon, &mut rng);
    SocketTokenRepo::set(store.pool(), &first.sha256_hex, FIXED_NOW)
        .await
        .expect("set");

    assert_eq!(
        SocketTokenRepo::get(store.pool()).await.expect("get").as_deref(),
        Some(first.sha256_hex.as_str()),
        "only the digest is stored"
    );
    assert!(SocketTokenRepo::verify(store.pool(), &first.plaintext).await.expect("verify"));
    assert!(
        !SocketTokenRepo::verify(store.pool(), "mdt_WRONG").await.expect("verify"),
        "a wrong plaintext must not verify"
    );

    // Re-mint: the upsert replaces the single row; the old plaintext dies.
    let second = mint(TokenKind::Daemon, &mut rng);
    SocketTokenRepo::set(store.pool(), &second.sha256_hex, FIXED_NOW + 1)
        .await
        .expect("replace");
    assert!(!SocketTokenRepo::verify(store.pool(), &first.plaintext).await.expect("verify"));
    assert!(SocketTokenRepo::verify(store.pool(), &second.plaintext).await.expect("verify"));
}
