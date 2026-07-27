# Fleet parity catalogue

`manifest.json` is versioned protocol inventory for the Hangar daemon, Fleet
TUI, and native macOS app. It is not a second source of Fleet state.

`fixtures/v1` contains deterministic, pretty JSON generated only by:

```bash
cd ainb-tui
cargo run -p ainb-hangar-proto --example export_fleet_fixtures
cargo run -p ainb-hangar-proto --example export_fleet_fixtures -- --check
```

Swift decodes these exact fixtures into named `Codable` types. It does not
derive Fleet semantics from arbitrary JSON keys.

The manifest validator enforces these rules:

- Capability IDs are sorted and exactly match `FLEET_PROTOCOL_CAPABILITY_IDS`.
- `daemon_request_method` belongs to `methods::ALL_METHODS`.
- `daemon_notification_method`, when present, belongs only to
  `methods::FLEET_PROTOCOL_NOTIFICATION_METHODS`.
- Every `pass` status names existing evidence paths. `evidence_root` defaults
  to `ainb_tui`; use `workspace` for evidence under `apps/`.
- Every `known_gap` names its separate-session bead.
- TUI-only deferral requires one `tui_deferral` bead and non-empty individual
  `gaps`. All TUI surfaces must use `deferred`; daemon and Swift cannot.
- `phase: e01_exit` rejects Foundation daemon or Swift `blocked_proof`, a TUI
  `blocked_proof`, and a `blocked_proof` release classification. A deferred
  TUI surface is valid only with its one bead and gap list.

`e01_in_progress` permits `blocked_proof` while later E01 tasks add real
evidence. TUI parity may be explicitly deferred under one separate-session
bead without blocking Swift app delivery. Set `phase` to `e01_exit` only when
every Foundation daemon and Swift proof is resolved. Do not change a status to
`pass` from source inspection alone.

`fleet.action.execute` and `fleet.broadcast.execute` remain in the negotiated
catalogue but are V1 capabilities. AFM-E01 exposes no product write method, so
its Swift surface is `not_applicable`; AFM-E03 owns their Swift action, receipt,
and broadcast proof.
