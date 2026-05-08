# Plugin contract changelog

Tracks how the plugin contract (`docs/plugin-spec/v1.md`) evolves.

## Versioning policy

* **Adding** a manifest key, host-fn, ABI export, or `PluginEvent`
  variant stays inside the current version (`v1` while we're in the
  1.x host series). Plugins that don't use the new addition keep
  passing `run_contract_v1`.
* **Renaming** or **removing** any of those is a breaking change. The
  contract bumps to `v2`; the CTS crate ships a `run_contract_v2` next
  to `run_contract_v1`. The existing `v1` runner stays supported until
  the host drops every plugin still pinned to it.
* The host's `host_version` is the contract version of the host-fn
  table. Adding a host-fn bumps minor; changing a signature bumps
  major and refuses plugins declaring an older `ainb_min_version`
  major.

## v1 — 2026-05-08

Initial release alongside MVP. Surface area:

* Manifest schema (`[plugin]`, `[capabilities]`, `[provides]`).
* Six required exports (`_init`) + five optional
  (`_render`, `_tick`, `_handle_event`, `_shutdown`, `_alloc`).
* Twelve host-fn imports — see §3 of `v1.md` for the table.
* Resource budgets: 50ms per-call wall time, 64 KiB render/event
  payloads, 1 MiB `_alloc` request cap.
* External-tagging only on `PluginEvent` (rmp-serde rejects internally
  tagged primitive newtypes — locked decision).

Twelve CTS axes shipped (axis 1, 6, 7, 8, 11, 12 in Phase 1.5; axes 2,
3, 4, 5, 9, 10 in Phase 5b). Burndown plugin is the in-tree dogfood —
its `tests/conformance.rs` runs the CTS against the live wasm bytes.
