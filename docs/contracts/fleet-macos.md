---
title: "Fleet macOS daemon contract"
---

# Fleet macOS daemon contract

Fleet macOS is a native JSON-RPC client of the Hangar daemon. It does not
depend on the TUI, CLI output, daemon database, or Rust internals.

## Contract authority

| Concern | Authority | Fleet macOS boundary |
| --- | --- | --- |
| Protocol version, capabilities, session wire types | `ainb-tui/crates/ainb-hangar-proto/src/fleet.rs` | `apps/ainb-fleet-macos/Sources/FleetRPC/FleetWire.swift` |
| RPC method names and reset signal | `ainb-tui/crates/ainb-hangar-proto/src/methods.rs` | `apps/ainb-fleet-macos/Sources/FleetRPC/FleetConnection.swift` |
| Authentication and socket handlers | `ainb-tui/crates/ainb-hangar-daemon/src/rpc/` | `apps/ainb-fleet-macos/Sources/FleetRPC/HangarLocation.swift` and `FleetConnection.swift` |
| Executable daemon fixture | `ainb-tui/crates/ainb-hangar-daemon/examples/fleet_fixture_daemon.rs` | `apps/ainb-fleet-macos/Tests/FleetRPCTests/FleetDaemonContractTests.swift` |

## Standalone runtime

Fleet setup is an explicit CLI provisioning step:

```sh
ainb fleet runtime install
```

It idempotently starts or repairs the Hangar daemon and notifyd, then installs
supported provider hooks. The app never invokes this command or interprets its
output. After setup it discovers the daemon socket and token, negotiates the
public protocol, then reports hook health through `fleet/runtime_status`.

## Fleet-only read RPCs

| Method | Capability | Purpose | Safety rule |
| --- | --- | --- | --- |
| `fleet/runtime_status` | `fleet.runtime.read` | Provider-hook installation and delivery health | No paths, logs, credentials, or hook files cross the boundary. |
| `fleet/usage_summary` | `fleet.usage.read` | Bounded canonical provider Usage for today, 7 days, or 30 days | No transcripts or raw provider histories cross the boundary. Cost is absent when daemon pricing is unknown. |

Fleet calls either method only after a compatible `fleet/negotiate` result
advertises its capability. Missing capabilities remain unavailable UI states,
never client-side fallback reads.

## Change rules

- Adding optional response fields is compatible until Fleet needs to display them.
- Adding a capability or RPC method needs Fleet work only when Fleet uses it.
- Adding a provider, lifecycle, replay-state, or other closed enum value needs
  Fleet wire-model review before merge. Swift decoding rejects unknown values.
- Renaming or removing required fields, changing action or revision semantics,
  or changing daemon discovery/authentication needs Fleet implementation and
  fixture-test updates.
- TUI layout, keybindings, CLI formatting, and internal database migrations do
  not require Fleet changes while the RPC contract remains identical.

## Required validation

Changes to a contract-authority path or `apps/ainb-fleet-macos/**` run the
`Fleet macOS contract` GitHub Actions workflow. It:

1. Builds `fleet_fixture_daemon` from current Rust sources.
2. Runs the Fleet XCTest suite against that daemon fixture.
3. Builds the Release app and verifies diagnostics without code signing.

Protocol breaking changes must bump negotiated protocol compatibility and keep
the Swift client read and write compatible before Fleet UI behavior changes.
