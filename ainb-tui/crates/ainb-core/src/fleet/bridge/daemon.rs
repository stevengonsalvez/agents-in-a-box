// ABOUTME: The bridge's client onto the hangar control plane (spec P8 / D18),
// now living in the `ainb-hangar-client` crate.
//
// It moved out so the fleet Pal's MCP tool server (`ainb-fleet-tools`) can
// dial the daemon with the same auth, framing and subscription code instead of
// growing a second dialect of it: `ainb-core` depends on `ainb-hangar-daemon`,
// so no crate below can depend back on `ainb-core`.
//
// This path stays valid for every existing caller.

pub use ainb_hangar_client::*;
use ainb_hangar_proto::connections::{SurfaceInfo, SurfaceKind};

/// A daemon client that announces itself as the TUI.
///
/// `DaemonClient::from_env` defaults every client to `SurfaceKind::Cli`, which
/// is right for the `ainb` subcommands that dial once and exit, and wrong for
/// the TUI: it holds the connection for the life of the process, and the
/// connection registry is how every other surface learns a TUI is running at
/// all. Without this a running TUI appears in `hangar connections list` as one
/// more `cli` row, indistinguishable from the short-lived CLI call that is
/// printing the list.
///
/// Every dial from a TUI pane goes through here, so "is a TUI connected" has
/// exactly one answer rather than one per call site.
pub fn tui_client() -> Result<DaemonClient, DaemonError> {
    let mut client = DaemonClient::from_env()?;
    client.set_surface(SurfaceInfo {
        kind: SurfaceKind::Tui,
        pid: std::process::id(),
    });
    Ok(client)
}
