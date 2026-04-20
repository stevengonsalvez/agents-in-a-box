# Archive

Retired code kept for historical reference and rollback. Not installed by
`bootstrap.js` or any tool's skills loader — lives outside `packages/` on
purpose.

## Contents

### `reflect-v1/`

The pre-v3.0.0 monolithic `/reflect` skill. Superseded by the plugin at
`toolkit/packages/plugins/reflect/` (v3.0.0), which splits it into
colon-namespaced sub-skills (`/reflect`, `/reflect:consolidate`,
`/reflect:ingest`, `/reflect:status`) and adds:

- Multi-tool memory discovery (Claude / Codex / Copilot / Gemini)
- SQLite state with audit trail (was: YAML files)
- Layered TOML config
- Provenance tracking on every learning
- Entity sidecar validator
- End-to-end simulation test suite (13 tests)

Kept here so any learnings/patterns that lived only in the v1 code paths
remain recoverable via `git log toolkit/archive/reflect-v1/`. Safe to
delete once v3 has been running in production for a while and any
bespoke logic has been audited.

**Do not deploy from here.** If you want the old behaviour back:
`git checkout <pre-v3-commit> -- toolkit/packages/skills/reflect`.
