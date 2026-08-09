// ABOUTME: The bridge's client onto the hangar control plane (spec P8 / D18),
// now living in the `ainb-hangar-client` crate.
//
// It moved out so the fleet copilot's MCP tool server (`ainb-fleet-tools`) can
// dial the daemon with the same auth, framing and subscription code instead of
// growing a second dialect of it: `ainb-core` depends on `ainb-hangar-daemon`,
// so no crate below can depend back on `ainb-core`.
//
// This path stays valid for every existing caller.

pub use ainb_hangar_client::*;
