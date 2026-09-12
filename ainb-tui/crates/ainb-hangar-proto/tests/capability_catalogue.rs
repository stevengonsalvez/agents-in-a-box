//! The capability catalogue is a COMMITTED file, and a string may never leave it.
//!
//! D17's bump rule only works if removing a capability is loud: new methods and
//! new event kinds are capability strings precisely so the protocol integer can
//! stay still, which means a vanished string is the one change that silently
//! breaks a client. A const array alone cannot enforce that, deleting a line
//! from it compiles. A committed file that the array is diffed against can, and
//! this is that diff.
//!
//! Adding a capability: append the const to
//! `ainb_hangar_proto::protocol::CAPABILITY_CATALOGUE`, then append the same
//! string to `capabilities.catalogue`. Removing one is a `PROTOCOL_VERSION`
//! bump, not an edit to this file.

use ainb_hangar_proto::protocol::CAPABILITY_CATALOGUE;

/// The committed record of every capability string this wire has ever served.
const COMMITTED: &str = include_str!("../capabilities.catalogue");

fn committed_ids() -> Vec<&'static str> {
    COMMITTED
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// The gate: a string in the committed file that the build no longer advertises
/// is a removed capability, and removing one is a protocol bump.
#[test]
fn no_committed_capability_may_be_removed() {
    let missing: Vec<&str> = committed_ids()
        .into_iter()
        .filter(|id| !CAPABILITY_CATALOGUE.contains(id))
        .collect();
    assert!(
        missing.is_empty(),
        "these capabilities were removed from CAPABILITY_CATALOGUE: {missing:?}. \
         Removing a capability is a PROTOCOL_VERSION bump (D17 bump rule), not an edit."
    );
}

/// The other direction: a capability the build advertises but never committed
/// is a string no reviewer ever saw, and the file stops being the record.
#[test]
fn every_advertised_capability_is_committed() {
    let committed = committed_ids();
    let uncommitted: Vec<&&str> =
        CAPABILITY_CATALOGUE.iter().filter(|id| !committed.contains(*id)).collect();
    assert!(
        uncommitted.is_empty(),
        "these capabilities are advertised but not in capabilities.catalogue: {uncommitted:?}. \
         Append them to the file in the same change that adds them."
    );
}

/// The catalogue is APPEND-ONLY: the committed file is a prefix of the live
/// array, in order. Reordering would let a removal hide as a move.
#[test]
fn the_catalogue_is_append_only() {
    let committed = committed_ids();
    let live: Vec<&str> = CAPABILITY_CATALOGUE.to_vec();
    assert!(
        live.len() >= committed.len(),
        "the live catalogue is shorter than the committed record"
    );
    assert_eq!(
        &live[..committed.len()],
        committed.as_slice(),
        "the catalogue must be append-only: existing entries may not be reordered"
    );
}

/// The file is a plain newline-delimited list, so a reviewer reads the diff as
/// "one line added" and nothing else.
#[test]
fn the_committed_file_has_no_duplicate_lines() {
    let mut seen = std::collections::HashSet::new();
    for id in committed_ids() {
        assert!(
            seen.insert(id),
            "duplicate line {id:?} in capabilities.catalogue"
        );
    }
}
