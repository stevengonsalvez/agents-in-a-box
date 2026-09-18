// ABOUTME: Secret scrubbing for bridge diagnostics, frames and transcripts.
//
// The scrubber itself lives in `ainb_hangar_core::redact`, below both this
// crate and the transcript classifier in `ainb-hangar-proto`, which must scrub
// before it cuts (#1187). Re-exported here so every caller keeps its path.

pub use ainb_hangar_core::redact::*;
