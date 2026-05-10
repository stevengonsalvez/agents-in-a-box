//! Inline FNV-1a 64-bit hash.
//!
//! Used as a non-cryptographic dedupe id for [`ProviderCall`]s
//! constructed from `(file_path, byte_offset)` tuples. Inlining the
//! 7-line hash drops blake3 + its SIMD intrinsics from the binary,
//! per `reference_inline_fnv_drop_blake3`.
//!
//! [`ProviderCall`]: ainb_plugin_types_sessions::ProviderCall

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64-bit hash of `bytes`.
#[must_use]
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Stable provider-call id from `path:offset`.
#[must_use]
pub fn provider_call_id(path: &str, offset: u64) -> u64 {
    let mut buf = Vec::with_capacity(path.len() + 24);
    buf.extend_from_slice(path.as_bytes());
    buf.push(b':');
    buf.extend_from_slice(offset.to_string().as_bytes());
    fnv1a_64(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_offset_basis() {
        assert_eq!(fnv1a_64(b""), FNV_OFFSET);
    }

    #[test]
    fn known_values_match_reference() {
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn provider_call_id_is_stable_for_same_input() {
        let a = provider_call_id("/tmp/x.jsonl", 42);
        let b = provider_call_id("/tmp/x.jsonl", 42);
        assert_eq!(a, b);
    }

    #[test]
    fn provider_call_id_differs_on_offset_change() {
        let a = provider_call_id("/tmp/x.jsonl", 42);
        let b = provider_call_id("/tmp/x.jsonl", 43);
        assert_ne!(a, b);
    }

    #[test]
    fn provider_call_id_differs_on_path_change() {
        let a = provider_call_id("/tmp/x.jsonl", 42);
        let b = provider_call_id("/tmp/y.jsonl", 42);
        assert_ne!(a, b);
    }
}
