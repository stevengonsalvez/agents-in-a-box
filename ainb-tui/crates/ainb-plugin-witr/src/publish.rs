//! Event-bus publishing for the `witr.snapshot` topic.
//!
//! On every successful scan the plugin publishes the parsed snapshot
//! so other plugins can consume process-causality data without
//! re-exec'ing witr. Per
//! `research/2026-05-19_12-13-09_witr-spec-open-questions.md` (Q4,
//! "Path 2"), the payload is the snapshot serialised to a
//! `serde_json::Value` then msgpack-encoded — no new shared-types
//! crate for v1; consumers decode msgpack → `Value` and read fields
//! by name. When a real consumer appears, lift the shape into a
//! typed `ainb-plugin-types-witr` crate.
//!
//! ## Byte-determinism
//!
//! The acceptance bar requires byte-identical msgpack across runs.
//! Two things guarantee it:
//!
//! 1. The crate does NOT enable serde_json's `preserve_order`
//!    feature, so `Value::Object` is a `BTreeMap` — keys serialise in
//!    sorted order, not insertion/hash order. (If any workspace crate
//!    ever turns `preserve_order` on, feature unification would make
//!    ordering insertion-based — still deterministic from a fixed
//!    struct, but the byte-identity test below is the tripwire that
//!    would catch a regression either way.)
//! 2. `WitrSnapshot`'s only typed map (`Source.details`) is a
//!    `BTreeMap`, and every other field is a scalar/`Vec`/`Option` —
//!    no `HashMap` anywhere on the wire path
//!    ([[reference_msgpack_byte_determinism_vec_over_hashmap]]).

use crate::model::WitrSnapshot;

/// The event-bus topic the plugin publishes fresh scans on. Declared
/// in `manifest.toml` `[provides] snapshots`.
pub const WITR_SNAPSHOT_TOPIC: &str = "witr.snapshot";

/// Encode a snapshot into the `witr.snapshot` wire payload: the
/// snapshot as a `serde_json::Value`, msgpack-serialised. Deterministic
/// across runs for a given snapshot (see module docs).
pub fn encode_snapshot_payload(snap: &WitrSnapshot) -> Result<Vec<u8>, EncodeError> {
    let value = serde_json::to_value(snap).map_err(EncodeError::Json)?;
    // `to_vec` (not `to_vec_named`) is correct here: on a
    // `Value::Object` the Value's own Serialize impl drives a msgpack
    // *map* with string keys regardless — `_named` only matters for
    // typed structs (map vs positional array). The round-trip test
    // below proves the map form decodes back cleanly.
    rmp_serde::to_vec(&value).map_err(EncodeError::MsgPack)
}

/// Failure encoding a snapshot payload.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    /// Snapshot → `serde_json::Value` failed (shouldn't happen for a
    /// well-formed snapshot).
    #[error("snapshot to JSON value failed: {0}")]
    Json(serde_json::Error),
    /// `Value` → msgpack failed.
    #[error("JSON value to msgpack failed: {0}")]
    MsgPack(rmp_serde::encode::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(json: serde_json::Value) -> WitrSnapshot {
        serde_json::from_value(json).expect("fixture parses")
    }

    fn nginx() -> WitrSnapshot {
        snap(serde_json::json!({
            "Target": {"Type": "name", "Value": "nginx"},
            "ResolvedTarget": "nginx",
            "Process": {"PID": 1234, "PPID": 800, "Command": "nginx", "User": "root"},
            "Ancestry": [{"PID": 800, "PPID": 1, "Command": "systemd"}],
            "Source": {
                "Type": "systemd",
                "Name": "nginx.service",
                "Details": {"z-last": "1", "a-first": "2", "m-mid": "3"}
            },
            "Warnings": ["running as root"]
        }))
    }

    #[test]
    fn encodes_non_empty_payload() {
        let bytes = encode_snapshot_payload(&nginx()).expect("encode");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn payload_is_byte_identical_across_runs() {
        // The acceptance bar: same snapshot → identical msgpack bytes
        // every time. Encodes twice (independent serde passes) and
        // asserts byte-equality — catches HashMap/iteration-order
        // non-determinism if it ever creeps in.
        let a = encode_snapshot_payload(&nginx()).expect("encode a");
        let b = encode_snapshot_payload(&nginx()).expect("encode b");
        assert_eq!(a, b, "msgpack payload must be byte-stable across runs");
    }

    #[test]
    fn payload_round_trips_back_to_snapshot() {
        // A consumer decodes msgpack → Value → WitrSnapshot. Prove the
        // round-trip is lossless for the stable surface.
        let original = nginx();
        let bytes = encode_snapshot_payload(&original).expect("encode");
        let value: serde_json::Value = rmp_serde::from_slice(&bytes).expect("msgpack decode");
        let back: WitrSnapshot = serde_json::from_value(value).expect("value to snapshot");
        assert_eq!(original, back);
    }

    #[test]
    fn map_field_order_does_not_affect_bytes() {
        // Two snapshots whose Source.details were *built* in different
        // insertion orders must encode identically (BTreeMap sorts).
        let mut a = nginx();
        let mut b = nginx();
        a.source.details.clear();
        a.source.details.insert("a".into(), "1".into());
        a.source.details.insert("b".into(), "2".into());
        b.source.details.clear();
        b.source.details.insert("b".into(), "2".into());
        b.source.details.insert("a".into(), "1".into());
        assert_eq!(
            encode_snapshot_payload(&a).unwrap(),
            encode_snapshot_payload(&b).unwrap(),
            "map field insertion order must not change the bytes",
        );
    }
}
