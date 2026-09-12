//! The 128 bits behind a D18 op id.
//!
//! Split from the op id TYPE, which lives in `ainb-hangar-proto`, because the
//! two are different concerns and only one of them needs a dependency. Proto is
//! pure data (serde and nothing else, by its own discipline), so it cannot own
//! a CSPRNG; this crate already has one for token minting.
//!
//! So: this decides where the randomness comes from, and
//! `ainb_hangar_proto::mutation::OpId::from_bytes` decides what an op id looks
//! like on the wire. Every client mints through both, in one line, instead of
//! keeping its own copy of the same four.

/// Mint 128 bits from the OS CSPRNG.
///
/// 128 bits because two devices minting concurrently for the rest of the decade
/// will not collide, and because the ledger's foreign-principal rule covers the
/// case that actually happens, an id REUSED rather than guessed.
#[must_use]
pub fn mint_bytes() -> [u8; 16] {
    use rand::RngCore as _;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two mints differ. A fixed or counter-based source would make every
    /// client's first mutation collide with every other client's.
    #[test]
    fn successive_mints_differ() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            assert!(
                seen.insert(mint_bytes()),
                "the op id source repeated itself"
            );
        }
    }
}
