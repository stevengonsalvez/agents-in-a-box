// ABOUTME: Unit tests for the usage_cache module — fingerprinting, schema
// migration, and basic store get_or_parse semantics. End-to-end parser
// integration lives in tests/usage_cache_integration.rs.

#![cfg(test)]

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

use rusqlite::Connection;

use crate::models::usage::ProviderCall;
use crate::usage_cache::db::SCHEMA_VERSION;
use crate::usage_cache::fingerprint::{
    classify, verify_append_safe, FileFingerprint, FingerprintAction, SUFFIX_HASH_BYTES,
};
use crate::usage_cache::store::{Cache, ParseHint, ParseResult};

fn write_file(dir: &TempDir, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(contents).unwrap();
    path
}

fn synth_call(seed: &str) -> ProviderCall {
    crate::test_support::ProviderCallBuilder::new()
        .with_model("test-model")
        .with_session(format!("session-{seed}"))
        .with_project("p")
        .with_project_path("/tmp/p")
        .with_input_tokens(1)
        .with_output_tokens(1)
        .build()
}

#[test]
fn schema_open_creates_files_table_and_records_version() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("usage.db");
    let cache = Cache::open(db.clone()).unwrap();
    drop(cache);

    let conn = Connection::open(&db).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
    let v: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
}

#[test]
fn schema_bump_drops_and_rebuilds() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("usage.db");

    // Hand-craft an "old" DB at version 0 with an unrelated table to prove
    // the rebuild wipes things.
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version VALUES (0);
             CREATE TABLE files (path TEXT PRIMARY KEY, junk TEXT);
             INSERT INTO files VALUES ('legacy', 'should be wiped');",
        )
        .unwrap();
    }

    let _cache = Cache::open(db.clone()).unwrap();
    let conn = Connection::open(&db).unwrap();
    let v: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0, "legacy row must not survive a schema bump");
}

#[test]
fn fingerprint_unchanged_when_size_and_suffix_match() {
    let prior = FileFingerprint {
        size_bytes: 100,
        mtime_nanos: 1_000,
        suffix_blake3: [0xAB; 32],
    };
    let current = FileFingerprint {
        size_bytes: 100,
        mtime_nanos: 2_000, // mtime changed but size + suffix identical
        suffix_blake3: [0xAB; 32],
    };
    assert_eq!(classify(&prior, &current, 100), FingerprintAction::Unchanged);
}

#[test]
fn fingerprint_truncate_forces_full_reparse() {
    let prior = FileFingerprint {
        size_bytes: 100,
        mtime_nanos: 1_000,
        suffix_blake3: [0xAB; 32],
    };
    let current = FileFingerprint {
        size_bytes: 50,
        mtime_nanos: 2_000,
        suffix_blake3: [0xCD; 32],
    };
    assert_eq!(classify(&prior, &current, 100), FingerprintAction::FullReparse);
}

#[test]
fn fingerprint_growth_signals_append() {
    let prior = FileFingerprint {
        size_bytes: 100,
        mtime_nanos: 1_000,
        suffix_blake3: [0xAB; 32],
    };
    let current = FileFingerprint {
        size_bytes: 200,
        mtime_nanos: 2_000,
        suffix_blake3: [0xEF; 32],
    };
    assert_eq!(
        classify(&prior, &current, 100),
        FingerprintAction::Append { from_offset: 100 }
    );
}

#[test]
fn verify_append_safe_detects_in_place_rewrite() {
    let dir = TempDir::new().unwrap();
    // Original tail bytes:
    let original = b"hello-world".repeat(10); // ~110 bytes
    let path = write_file(&dir, "f.jsonl", &original);
    let prior_size = original.len() as u64;
    let prior_window = SUFFIX_HASH_BYTES.min(prior_size) as usize;
    let prior_suffix = blake3::hash(&original[(original.len() - prior_window)..]);

    // Rewrite the file in-place with different content same length, then
    // append more bytes.
    let mut rewritten = b"GOODBYE-WORLD".repeat(10);
    rewritten.truncate(original.len());
    rewritten.extend_from_slice(b"new-tail-bytes");
    fs::write(&path, &rewritten).unwrap();

    let mut handle = fs::File::open(&path).unwrap();
    let safe = verify_append_safe(&mut handle, prior_size, prior_suffix.as_bytes()).unwrap();
    assert!(!safe, "in-place rewrite must be detected");
}

#[test]
fn cache_disabled_skips_all_writes() {
    let dir = TempDir::new().unwrap();
    let path = write_file(&dir, "f.jsonl", b"line\n");
    let cache = Cache::disabled();

    let calls_emitted = Arc::new(AtomicUsize::new(0));
    let counter = calls_emitted.clone();
    let result = cache.get_or_parse(&path, |_p, hint| {
        counter.fetch_add(1, Ordering::SeqCst);
        assert!(matches!(hint, ParseHint::Full));
        ParseResult {
            calls: vec![synth_call("a")],
            end_offset: 5,
        }
    });
    assert_eq!(result.len(), 1);
    assert_eq!(calls_emitted.load(Ordering::SeqCst), 1);

    // Run again — disabled cache means we still fall through to parse.
    let _ = cache.get_or_parse(&path, |_p, _hint| ParseResult {
        calls: vec![synth_call("b")],
        end_offset: 5,
    });
    assert!(!cache.is_enabled());
}

#[test]
fn cache_first_scan_then_unchanged_serves_from_cache() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("usage.db");
    let path = write_file(&dir, "f.jsonl", b"line1\nline2\n");
    let cache = Cache::open(db).unwrap();

    let parses = Arc::new(AtomicUsize::new(0));

    // First scan → parse called.
    let counter = parses.clone();
    let r1 = cache.get_or_parse(&path, |_p, hint| {
        assert!(matches!(hint, ParseHint::Full));
        counter.fetch_add(1, Ordering::SeqCst);
        ParseResult {
            calls: vec![synth_call("first")],
            end_offset: 12,
        }
    });
    assert_eq!(r1.len(), 1);
    assert_eq!(parses.load(Ordering::SeqCst), 1);

    // Second scan → cache hit; closure must not run.
    let counter = parses.clone();
    let r2 = cache.get_or_parse(&path, |_p, _hint| {
        counter.fetch_add(1, Ordering::SeqCst);
        ParseResult {
            calls: Vec::new(),
            end_offset: 0,
        }
    });
    assert_eq!(r2.len(), 1);
    assert_eq!(
        parses.load(Ordering::SeqCst),
        1,
        "unchanged file must not re-invoke parser"
    );
}

#[test]
fn concurrent_get_or_parse_does_not_deadlock() {
    use std::thread;
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("usage.db");
    let path = write_file(&dir, "f.jsonl", b"line1\nline2\n");
    let cache = Arc::new(Cache::open(db).unwrap());

    let handles: Vec<_> = (0..4)
        .map(|i| {
            let cache = cache.clone();
            let path = path.clone();
            thread::spawn(move || {
                let r = cache.get_or_parse(&path, |_p, _h| ParseResult {
                    calls: vec![synth_call(&format!("t{i}"))],
                    end_offset: 12,
                });
                assert_eq!(r.len(), 1);
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let info = cache.info().unwrap();
    assert_eq!(info.file_count, 1, "single PK row regardless of contention");
}

#[test]
fn cache_clear_drops_rows_but_preserves_schema() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("usage.db");
    let path = write_file(&dir, "f.jsonl", b"line\n");
    let cache = Cache::open(db.clone()).unwrap();

    cache.get_or_parse(&path, |_p, _h| ParseResult {
        calls: vec![synth_call("x")],
        end_offset: 5,
    });

    let before = cache.info().unwrap();
    assert_eq!(before.file_count, 1);

    cache.clear().unwrap();
    let after = cache.info().unwrap();
    assert_eq!(after.file_count, 0);

    // Schema row still present.
    let conn = Connection::open(&db).unwrap();
    let v: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
}

#[test]
fn fingerprint_full_reparse_when_size_equal_but_suffix_differs() {
    // Same size + different suffix means the trailing bytes were rewritten
    // in place; classify must NOT return Append (and rely on
    // verify_append_safe to clean up). It must return FullReparse directly.
    let prior = FileFingerprint {
        size_bytes: 100,
        mtime_nanos: 1_000,
        suffix_blake3: [0xAA; 32],
    };
    let current = FileFingerprint {
        size_bytes: 100,
        mtime_nanos: 2_000,
        suffix_blake3: [0xBB; 32],
    };
    assert_eq!(classify(&prior, &current, 100), FingerprintAction::FullReparse);
}

/// Layout-stability tripwire for the bincode-encoded `Vec<ProviderCall>`
/// blob. If any field is added, removed, or reordered in `ProviderCall`
/// (or any nested type), the encoded length will change and this test will
/// fail. When it does, you MUST bump
/// `usage_cache::db::BLOB_FORMAT_BINCODE_CURRENT` AND update the expected length
/// here — that's the protocol that prevents silent cache corruption from
/// mis-decoding old blobs into a new struct shape.
#[test]
fn provider_call_bincode_layout_is_stable() {
    let fixture = synth_call("layout-tripwire");
    let encoded = bincode::serialize(&fixture).unwrap();

    // Round-trip first as the meaningful behavioural check.
    let decoded: ProviderCall = bincode::deserialize(&encoded).unwrap();
    assert_eq!(decoded.session_id, fixture.session_id);
    assert_eq!(decoded.input_tokens, fixture.input_tokens);
    assert_eq!(decoded.output_tokens, fixture.output_tokens);

    // Tripwire: if the byte length drifts, the struct layout changed.
    // See the doc comment on `ProviderCall` in `models/usage.rs`.
    const EXPECTED_LEN: usize = expected_layout_len();
    assert_eq!(
        encoded.len(),
        EXPECTED_LEN,
        "ProviderCall bincode layout changed — bump BLOB_FORMAT_BINCODE_CURRENT \
         and update EXPECTED_LEN. See ProviderCall doc comment."
    );
}

/// Compute the expected serialized length once. Bincode uses a varint-free
/// fixed encoding by default, so for a fixture with deterministic fields
/// the size is deterministic. Computed at compile time conceptually, but
/// inlined here so a layout drift surfaces as an `assert_eq!` mismatch.
const fn expected_layout_len() -> usize {
    // String layout (bincode default options): u64 len + bytes.
    // Vec<T>:                                  u64 len + items.
    // Option<T> (None):                        1 byte tag.
    // Option<T> (Some):                        1 byte tag + payload.
    // chrono::DateTime<Utc> serializes as i64 secs + u32 nanos pair
    // (via serde impl in chrono); seed value covers it. Local previously
    // serialized as a tagged enum (+5 bytes for the offset variant tag
    // and payload) — the V3 migration to Utc shaved those bytes off.
    //
    // Rather than re-deriving the formula on every change, we hard-code
    // the value observed for `synth_call("layout-tripwire")` (verified
    // by running with the old constant once and reading the actual size
    // from the failure message — see feedback_dont_guess_test_constants).
    // If chrono or bincode bump their encoding, update this constant
    // intentionally.
    //
    // History:
    //   184 (V1) — original 14-field layout with Local timestamp.
    //   185 (V2) — +Option<String> branch field (None tag).
    //   180 (V3) — timestamp Utc instead of Local (-5 bytes).
    180
}
