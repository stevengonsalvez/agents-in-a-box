//! Tripwire: the `ainb fleet` token-efficiency surface end-to-end through the
//! real binary — the content-addressed enrich cache round-trips, a miss exits
//! non-zero, and `needs --no-enrich` emits a well-formed 0-token JSON array
//! that flags nothing for enrichment.
//!
//! Deterministic + CI-safe: isolates HOME and the cache path via env, never
//! spawns a real agent or touches a live fleet. The token-efficiency *logic*
//! (blake3 key, LRU eviction, JSONL ERR fallback, classify split) is unit-
//! tested in-process under `src/fleet/`; this tripwire guards the CLI wiring
//! the producer and the HUD actually call.

use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

#[test]
fn enrich_cache_put_get_roundtrips_and_miss_fails() {
    let home = tempfile::tempdir().unwrap();
    let cache = home.path().join("enrich-cache.json");

    // A miss on an empty cache must exit non-zero (so the producer/script can
    // branch on it).
    let miss = Command::new(ainb_bin())
        .env("HOME", home.path())
        .env("AINB_FLEET_ENRICH_CACHE", &cache)
        .args(["fleet", "enrich-cache", "get", "--key", "k-abc"])
        .output()
        .expect("spawn ainb");
    assert!(!miss.status.success(), "get on an empty cache should fail");

    // Put, then the cache file exists at the env-pointed path.
    let put = Command::new(ainb_bin())
        .env("HOME", home.path())
        .env("AINB_FLEET_ENRICH_CACHE", &cache)
        .args([
            "fleet",
            "enrich-cache",
            "put",
            "--key",
            "k-abc",
            "--suggestion",
            "Approve as-is",
        ])
        .output()
        .expect("spawn ainb");
    assert!(
        put.status.success(),
        "put failed: {}",
        String::from_utf8_lossy(&put.stderr)
    );
    assert!(
        cache.exists(),
        "cache file not created at AINB_FLEET_ENRICH_CACHE"
    );

    // Get returns the stored suggestion verbatim.
    let hit = Command::new(ainb_bin())
        .env("HOME", home.path())
        .env("AINB_FLEET_ENRICH_CACHE", &cache)
        .args(["fleet", "enrich-cache", "get", "--key", "k-abc"])
        .output()
        .expect("spawn ainb");
    assert!(hit.status.success(), "get after put should succeed");
    assert_eq!(String::from_utf8_lossy(&hit.stdout).trim(), "Approve as-is");
}

#[test]
fn needs_no_enrich_emits_wellformed_zero_token_json() {
    let home = tempfile::tempdir().unwrap();
    // Fully isolate discovery so the fleet reads as empty regardless of the
    // host: point ainb-shelling, the jobs scan, and the broker DB at the
    // empty tempdir. Discovery failures degrade to an empty fleet by design.
    let out = Command::new(ainb_bin())
        .env("HOME", home.path())
        .env("AINB_BIN", ainb_bin())
        .env("AINB_FLEET_JOBS_DIR", home.path().join("jobs"))
        .env("CLAUDE_PEERS_DB", home.path().join("peers.db"))
        .args(["--format", "json", "fleet", "needs", "--no-enrich"])
        .output()
        .expect("spawn ainb");
    assert!(
        out.status.success(),
        "needs --no-enrich failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let body = stdout.trim();
    assert!(
        body.starts_with('['),
        "needs --format json should emit a JSON array, got:\n{body}"
    );
    // --no-enrich must never flag a card for the producer (0-token contract).
    assert!(
        !body.contains("\"need_enrich\": true"),
        "--no-enrich must not flag any card need_enrich:\n{body}"
    );
}
