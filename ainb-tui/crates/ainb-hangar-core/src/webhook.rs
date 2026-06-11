//! Webhook signing primitives for webhook-triggered autopilots (e38.18, IO-free).
//!
//! An autopilot can be fired by an authenticated local HTTP webhook
//! (`POST /hangar/webhook/<autopilot_id>`). The request is authenticated with an
//! HMAC-SHA256 signature over the **exact request body**, the GitHub/Stripe
//! webhook scheme: the caller computes `HMAC-SHA256(secret, body)` and sends the
//! lower-case hex digest in an `X-Hangar-Signature` header (optionally prefixed
//! `sha256=`); the daemon recomputes the same MAC with the autopilot's secret and
//! compares **in constant time**.
//!
//! # Why HMAC-of-body, not a bearer secret
//!
//! A bearer secret presented verbatim in a header would be replayable against
//! any body and would put the raw secret on the wire each request. Signing the
//! body binds the credential to the payload and lets the verifier hold the
//! secret without the caller ever transmitting it — only the per-request MAC
//! travels. This is why the daemon must hold the recoverable secret (in the 0600
//! `webhook_secrets.json` file the store layer manages), while the database
//! stores only the secret's SHA-256 digest.
//!
//! # Determinism / no IO
//!
//! Every function here is a pure transform over byte slices. [`mint_secret`]
//! takes the random source as a [`rand::RngCore`] parameter (production passes
//! [`rand::rngs::OsRng`]; tests inject a seeded RNG), exactly like
//! [`crate::token::mint`].

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Number of random bytes behind a freshly minted webhook secret (256 bits).
///
/// The same entropy budget as [`crate::token::TOKEN_RAND_BYTES`]; the secret is
/// rendered lower-case hex (64 chars) so it copies/pastes cleanly into a webhook
/// configuration field.
pub const SECRET_RAND_BYTES: usize = 32;

/// The HTTP header carrying the request's HMAC signature.
///
/// The value is the lower-case hex HMAC-SHA256 of the body, optionally prefixed
/// `sha256=` (the GitHub convention — both `<hex>` and `sha256=<hex>` are
/// accepted by [`verify_body_signature`]).
pub const SIGNATURE_HEADER: &str = "x-hangar-signature";

/// The HTTP header carrying the event name (an alternative to a body `event`
/// field) used by the optional per-autopilot event filter.
pub const EVENT_HEADER: &str = "x-hangar-event";

type HmacSha256 = Hmac<Sha256>;

/// A freshly minted webhook secret: the plaintext (returned once, written only
/// to the 0600 secret file) plus the SHA-256 hex digest persisted in the
/// `webhook_secret_sha256` column.
///
/// Mirrors [`crate::token::MintedToken`]: the database never holds the
/// plaintext, only the digest.
#[derive(Debug, Clone)]
pub struct MintedSecret {
    /// The full plaintext secret (64 lower-case hex chars). Written to the 0600
    /// `webhook_secrets.json` file and shown to the operator once at mint/rotate
    /// time. **Never** persist this in the database or write it to logs.
    pub plaintext: String,
    /// Lower-case hex SHA-256 digest of [`MintedSecret::plaintext`]. This is what
    /// the `webhook_secret_sha256` column stores (the rotation / "is a secret
    /// set" inspection value).
    pub sha256_hex: String,
}

/// Mint a fresh webhook secret, drawing [`SECRET_RAND_BYTES`] from `rng`.
///
/// The plaintext is the lower-case hex of the random bytes; the stored digest is
/// `sha256(plaintext)`. The random source is a parameter so tests inject a
/// seeded RNG; production passes a CSPRNG ([`rand::rngs::OsRng`]).
pub fn mint_secret<R: RngCore>(rng: &mut R) -> MintedSecret {
    let mut bytes = [0_u8; SECRET_RAND_BYTES];
    rng.fill_bytes(&mut bytes);
    let plaintext = hex::encode(bytes);
    let sha256_hex = crate::token::sha256_hex(&plaintext);
    MintedSecret {
        plaintext,
        sha256_hex,
    }
}

/// Compute `HMAC-SHA256(secret, body)` as lower-case hex.
///
/// The single signing seam: the verifier recomputes the same MAC and compares.
/// `secret` is the plaintext secret bytes; `body` is the exact request body
/// bytes.
#[must_use]
pub fn compute_signature(secret: &[u8], body: &[u8]) -> String {
    // `new_from_slice` accepts any key length (HMAC pads/hashes as needed); it
    // is infallible for HMAC, so the `expect` is unreachable.
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// Strip an optional `sha256=` prefix from a signature header value, returning
/// the bare hex digest. A header with no recognised prefix is returned as-is
/// (trimmed).
fn normalize_signature(header_value: &str) -> &str {
    let trimmed = header_value.trim();
    trimmed.strip_prefix("sha256=").unwrap_or(trimmed)
}

/// Constant-time check that `presented_signature` is a valid HMAC-SHA256 of
/// `body` under `secret`.
///
/// Accepts the bare hex digest or a `sha256=<hex>` value. Recomputes the
/// expected MAC and compares with [`subtle::ConstantTimeEq`] over the raw MAC
/// bytes, so the running time does not depend on how many leading bytes matched.
/// A signature that fails to hex-decode, or has the wrong length, returns
/// `false` (never panics, never short-circuits on length in a secret-dependent
/// way — the decode itself is input-shaped, not secret-shaped).
#[must_use]
pub fn verify_body_signature(secret: &[u8], body: &[u8], presented_signature: &str) -> bool {
    let presented_hex = normalize_signature(presented_signature);
    // Decode the presented hex to raw MAC bytes. A malformed hex string can
    // never match a well-formed MAC, so a decode failure is a rejection.
    let Ok(presented) = hex::decode(presented_hex) else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body);
    let expected = mac.finalize().into_bytes();
    // Length differs only on a malformed (non-32-byte) presented value, which is
    // attacker-controlled input shape, not secret material; ct_eq additionally
    // guards the equal-length path.
    presented.as_slice().ct_eq(expected.as_slice()).into()
}

/// Apply an optional event filter: `true` when the request should fire the
/// autopilot, `false` when it is accepted-but-filtered.
///
/// - `filter == None`  → fire on every signed request (no filter configured).
/// - `filter == Some`  → fire only when `event == Some(filter)` (exact match).
///
/// The event-name match is exact (the v1 collapse of Multica's richer filter
/// expression). A configured filter with no event on the request never fires.
#[must_use]
pub fn event_passes_filter(filter: Option<&str>, event: Option<&str>) -> bool {
    filter.is_none_or(|want| event == Some(want))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn seeded() -> StdRng {
        StdRng::seed_from_u64(0x00E3_8018)
    }

    #[test]
    fn mint_secret_is_64_char_hex_with_matching_digest() {
        let s = mint_secret(&mut seeded());
        assert_eq!(s.plaintext.len(), 64, "32 bytes hex-encode to 64 chars");
        assert!(
            s.plaintext.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "plaintext is lower-case hex: {}",
            s.plaintext
        );
        assert_eq!(
            s.sha256_hex,
            crate::token::sha256_hex(&s.plaintext),
            "stored digest is sha256 of the exact plaintext"
        );
        assert!(
            !s.sha256_hex.contains(&s.plaintext),
            "digest must never contain the plaintext"
        );
    }

    #[test]
    fn distinct_draws_yield_distinct_secrets() {
        let mut rng = seeded();
        let a = mint_secret(&mut rng);
        let b = mint_secret(&mut rng);
        assert_ne!(a.plaintext, b.plaintext);
        assert_ne!(a.sha256_hex, b.sha256_hex);
    }

    #[test]
    fn compute_signature_matches_known_rfc4231_style_vector() {
        // Stable round-trip: the same (secret, body) always yields the same MAC,
        // and it is 64 hex chars (32-byte SHA-256 MAC).
        let sig = compute_signature(b"secret", b"hello world");
        assert_eq!(sig.len(), 64);
        assert_eq!(sig, compute_signature(b"secret", b"hello world"));
        assert_ne!(sig, compute_signature(b"secret", b"hello WORLD"));
        assert_ne!(sig, compute_signature(b"other", b"hello world"));
    }

    #[test]
    fn verify_accepts_a_correctly_signed_body() {
        let secret = b"top-secret-key";
        let body = br#"{"event":"push"}"#;
        let sig = compute_signature(secret, body);
        assert!(verify_body_signature(secret, body, &sig));
        // The `sha256=` GitHub-style prefix is accepted too.
        assert!(verify_body_signature(
            secret,
            body,
            &format!("sha256={sig}")
        ));
        // Surrounding whitespace is tolerated.
        assert!(verify_body_signature(secret, body, &format!("  {sig}  ")));
    }

    #[test]
    fn verify_rejects_wrong_signature_secret_and_body() {
        let secret = b"top-secret-key";
        let body = br#"{"event":"push"}"#;
        let sig = compute_signature(secret, body);

        // Wrong secret.
        assert!(!verify_body_signature(b"WRONG", body, &sig));
        // Tampered body (replay against a different payload).
        assert!(!verify_body_signature(
            secret,
            br#"{"event":"deploy"}"#,
            &sig
        ));
        // Flipped last hex nibble.
        let mut tampered = sig.clone();
        let last = tampered.pop().unwrap();
        tampered.push(if last == 'a' { 'b' } else { 'a' });
        assert!(!verify_body_signature(secret, body, &tampered));
    }

    #[test]
    fn verify_rejects_malformed_signature_without_panicking() {
        let secret = b"k";
        let body = b"b";
        assert!(
            !verify_body_signature(secret, body, ""),
            "empty is rejected"
        );
        assert!(
            !verify_body_signature(secret, body, "not-hex-zzz"),
            "non-hex is rejected"
        );
        assert!(
            !verify_body_signature(secret, body, "deadbeef"),
            "short hex is rejected"
        );
    }

    #[test]
    fn event_filter_none_fires_on_everything() {
        assert!(event_passes_filter(None, None));
        assert!(event_passes_filter(None, Some("push")));
    }

    #[test]
    fn event_filter_some_requires_exact_match() {
        assert!(event_passes_filter(Some("push"), Some("push")));
        assert!(!event_passes_filter(Some("push"), Some("deploy")));
        assert!(
            !event_passes_filter(Some("push"), None),
            "a configured filter with no event never fires"
        );
    }
}
