//! Token minting and verification primitives (IO-free).
//!
//! Hangar issues two kinds of bearer credential:
//!
//! - **PAT** (`ainb_…`) — a personal access token a user presents to the host.
//! - **daemon token** (`mdt_…`) — a per-runtime token a daemon presents to
//!   another daemon (the daemon-to-daemon path is future work; the format and
//!   storage discipline are fixed now).
//!
//! # Storage discipline
//!
//! The plaintext token is shown to the user **exactly once**, at mint time, and
//! is **never** persisted. Only its SHA-256 digest (lower-case hex) is stored,
//! in the `sha256_token` column. Authentication re-hashes the presented
//! plaintext and compares digests in **constant time** (via
//! [`subtle::ConstantTimeEq`]) so a timing side-channel cannot leak how many
//! leading bytes of a guess were correct.
//!
//! # Determinism
//!
//! [`mint`] takes the random source as a [`rand::RngCore`] parameter rather than
//! reaching for a thread-local CSPRNG, so tests inject a seeded RNG and assert
//! the prefix + the "stored value is `sha256(plaintext)`" invariant without
//! flakiness. Production callers pass [`rand::rngs::OsRng`] (a CSPRNG).

use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Number of random bytes behind every token (256 bits of entropy).
///
/// 32 bytes base32-encode (RFC 4648, no padding) to exactly 52 characters from
/// the `[A-Z2-7]` alphabet, which the P5.4 format regex pins down.
pub const TOKEN_RAND_BYTES: usize = 32;

/// Base32 body length for [`TOKEN_RAND_BYTES`] bytes (`ceil(32 * 8 / 5) = 52`).
pub const TOKEN_BODY_LEN: usize = 52;

/// RFC 4648 base32 alphabet (upper-case, no padding): `A-Z` then `2-7`.
const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Which kind of credential a token is.
///
/// The discriminant fixes the human-readable prefix and nothing else — both
/// kinds share the same entropy, encoding, and hashing discipline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Personal access token (user -> host). Prefix `ainb_`.
    Pat,
    /// Daemon-to-daemon bearer token (future). Prefix `mdt_`.
    Daemon,
}

impl TokenKind {
    /// The literal prefix (including the trailing underscore) for this kind.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Pat => "ainb_",
            Self::Daemon => "mdt_",
        }
    }
}

/// A freshly minted token: the plaintext (shown once, never stored) plus the
/// SHA-256 hex digest that is the only thing persisted.
///
/// The plaintext lives only as long as this struct; callers print it once and
/// drop it. Persist [`MintedToken::sha256_hex`] into the `sha256_token` column.
#[derive(Debug, Clone)]
pub struct MintedToken {
    /// The full plaintext token (`<prefix><base32 body>`), shown to the user
    /// once at creation. **Never** write this to disk or logs.
    pub plaintext: String,
    /// Lower-case hex SHA-256 digest of [`MintedToken::plaintext`]'s exact
    /// UTF-8 bytes. This is what the `sha256_token` column stores.
    pub sha256_hex: String,
}

/// SHA-256 of `plaintext`'s exact UTF-8 bytes as lower-case hex.
///
/// The single hashing seam: minting and verification both go through here, so
/// the stored digest and the verification digest can never diverge.
#[must_use]
pub fn sha256_hex(plaintext: &str) -> String {
    let digest = Sha256::digest(plaintext.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        // Two lower-case hex nibbles per byte; width is fixed so no separators.
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Encode `bytes` as RFC 4648 base32 (upper-case, no padding).
///
/// Kept private and minimal (no decode path, no padding) because the only
/// caller needs a fixed-width `[A-Z2-7]` body for the token format.
fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = ((buffer >> bits) & 0b1_1111) as usize;
            out.push(BASE32_ALPHABET[index] as char);
        }
    }
    if bits > 0 {
        // Left-align the remaining bits into a final 5-bit group.
        let index = ((buffer << (5 - bits)) & 0b1_1111) as usize;
        out.push(BASE32_ALPHABET[index] as char);
    }
    out
}

/// Mint a fresh token of `kind`, drawing [`TOKEN_RAND_BYTES`] from `rng`.
///
/// The body is the base32 encoding of the random bytes; the plaintext is
/// `<kind.prefix()><body>`; the stored digest is `sha256(plaintext)`. The
/// random source is a parameter so tests can inject a seeded RNG; production
/// passes a CSPRNG ([`rand::rngs::OsRng`]).
pub fn mint<R: RngCore>(kind: TokenKind, rng: &mut R) -> MintedToken {
    let mut bytes = [0_u8; TOKEN_RAND_BYTES];
    rng.fill_bytes(&mut bytes);
    let body = base32_encode(&bytes);
    let plaintext = format!("{}{body}", kind.prefix());
    let sha256_hex = sha256_hex(&plaintext);
    MintedToken {
        plaintext,
        sha256_hex,
    }
}

/// Constant-time check that `plaintext` hashes to `stored_sha256_hex`.
///
/// Re-hashes `plaintext` and compares the resulting hex digest to the stored
/// one with [`subtle::ConstantTimeEq`], so the comparison's running time does
/// not depend on how many leading characters matched. Returns `true` only on a
/// full match.
#[must_use]
pub fn verify(plaintext: &str, stored_sha256_hex: &str) -> bool {
    let computed = sha256_hex(plaintext);
    // ct_eq over the hex bytes: equal-length, fixed-width digests, so length
    // itself leaks nothing. `into()` collapses the `Choice` to a bool only
    // after the constant-time comparison has run.
    computed.as_bytes().ct_eq(stored_sha256_hex.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn seeded() -> StdRng {
        StdRng::seed_from_u64(0x0A1B_0504)
    }

    #[test]
    fn pat_plaintext_matches_format_regex() {
        let token = mint(TokenKind::Pat, &mut seeded());
        assert!(
            token.plaintext.starts_with("ainb_"),
            "PAT must carry the ainb_ prefix: {}",
            token.plaintext
        );
        let body = &token.plaintext["ainb_".len()..];
        assert_eq!(body.len(), TOKEN_BODY_LEN, "body is 52 base32 chars");
        assert!(
            body.bytes().all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b)),
            "body must be RFC4648 base32 [A-Z2-7]: {body}"
        );
    }

    #[test]
    fn daemon_plaintext_carries_mdt_prefix() {
        let token = mint(TokenKind::Daemon, &mut seeded());
        assert!(token.plaintext.starts_with("mdt_"), "{}", token.plaintext);
        assert_eq!(token.plaintext["mdt_".len()..].len(), TOKEN_BODY_LEN);
    }

    #[test]
    fn stored_digest_is_sha256_of_plaintext_and_excludes_it() {
        let token = mint(TokenKind::Pat, &mut seeded());
        assert_eq!(
            token.sha256_hex,
            sha256_hex(&token.plaintext),
            "stored digest must be sha256 of the exact plaintext"
        );
        assert!(
            !token.sha256_hex.contains(&token.plaintext),
            "stored digest must never contain the plaintext"
        );
        assert_eq!(token.sha256_hex.len(), 64, "sha256 hex is 64 chars");
        assert!(
            token.sha256_hex.bytes().all(|b| b.is_ascii_hexdigit()),
            "digest is lower-case hex"
        );
    }

    #[test]
    fn distinct_random_draws_yield_distinct_plaintext() {
        let mut rng = seeded();
        let a = mint(TokenKind::Pat, &mut rng);
        let b = mint(TokenKind::Pat, &mut rng);
        assert_ne!(a.plaintext, b.plaintext);
        assert_ne!(a.sha256_hex, b.sha256_hex);
    }

    #[test]
    fn verify_accepts_minted_plaintext() {
        let token = mint(TokenKind::Pat, &mut seeded());
        assert!(verify(&token.plaintext, &token.sha256_hex));
    }

    #[test]
    fn verify_rejects_tampered_plaintext() {
        let token = mint(TokenKind::Pat, &mut seeded());
        let mut tampered = token.plaintext.clone();
        // Flip the final character to a different base32 symbol.
        let last = tampered.pop().expect("non-empty");
        tampered.push(if last == 'A' { 'B' } else { 'A' });
        assert!(!verify(&tampered, &token.sha256_hex));
    }

    #[test]
    fn verify_rejects_wrong_length_digest() {
        let token = mint(TokenKind::Pat, &mut seeded());
        // A truncated stored digest must never compare equal.
        assert!(!verify(&token.plaintext, &token.sha256_hex[..32]));
    }

    #[test]
    fn base32_known_vector_rfc4648() {
        // RFC 4648 §10: base32("foobar") == "MZXW6YTBOI" (no padding here).
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
    }
}
