//! `SystemIdGen` mints ULIDs that strictly increase within the process (#1246).
//!
//! Tables order equal-millisecond rows by their ULID id (`dispatch_attempt`'s
//! migration says so: "ULIDs are monotonic within a millisecond"). A plain
//! `Ulid::new()` is random within its millisecond, so two rows written in the
//! same millisecond sorted by chance, and a card's newest attempt could list
//! second.

use std::collections::HashSet;

use ainb_hangar_core::idgen::{IdGen, SystemIdGen};

#[test]
fn back_to_back_ids_strictly_increase() {
    let ids: Vec<String> = (0..10_000).map(|_| SystemIdGen.new_ulid()).collect();
    let out_of_order = ids.windows(2).filter(|pair| pair[1] <= pair[0]).count();
    assert_eq!(
        out_of_order, 0,
        "{out_of_order} of 9999 consecutive ids did not increase"
    );
}

#[test]
fn ids_minted_on_many_threads_stay_unique() {
    let handles: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| (0..2_500).map(|_| SystemIdGen.new_ulid()).collect::<Vec<_>>())
        })
        .collect();
    let mut seen = HashSet::new();
    for handle in handles {
        for id in handle.join().expect("minting thread") {
            assert!(seen.insert(id.clone()), "duplicate id {id}");
        }
    }
    assert_eq!(seen.len(), 10_000);
}
