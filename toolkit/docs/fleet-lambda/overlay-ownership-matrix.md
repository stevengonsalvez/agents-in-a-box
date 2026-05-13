# Fleet Lambda Overlay Ownership Matrix

## Decision summary

- **Canonical code lives in code repos.**
- **Backup repos hold non-code/manual/runtime-authored state only.**
- **Live runtime paths are deployment targets, not authoring surfaces.**
- **Unwired or backup-only code must either be restored properly or removed from claims.**

## Classification buckets

- **CODE-SOURCE** — authored source code, tests, schemas, docs for behavior
- **RUNTIME-STATE** — generated or live machine state
- **BACKUP-ONLY** — disaster-recovery copy of non-code/manual state
- **DEAD-OR-DRIFT** — stale, misleading, unwired, or duplicated artifacts

## Current artifact map

| Path | Role today | Correct class | Canonical home | Action |
|---|---|---:|---|---|
| `~/.hermes/plugins/fleet-hooks/*.py` | live fleet hook code | CODE-SOURCE + runtime deploy target | code repo, then synced into runtime | move source ownership out of runtime; keep runtime copy generated/synced |
| `~/.hermes/plugins/fleet-hooks/plugin.yaml` | live plugin manifest | CODE-SOURCE + runtime deploy target | code repo | same as above |
| `~/.hermes/plugins/fleet-hooks/injection_history.py` | helper state module used by live hooks | CODE-SOURCE + runtime deploy target | code repo | keep; add observable outputs around it |
| `~/.hermes/bin/*.py` | live ops/runtime scripts | mixed | code repo for maintained scripts; runtime path as install target | split authored scripts from generated wrappers |
| `~/.clan/learnings/*.jsonl` | shared ledgers | RUNTIME-STATE | runtime/shared state | keep out of code repo except schema/docs/examples |
| `~/.clan/learnings/bank.db` | retrieval index | RUNTIME-STATE | runtime/shared state | do not version as canonical source |
| `~/.clan/bin/*` | utility scripts | CODE-SOURCE if maintained, else local ops | code repo or bootstrap bundle source | audit one-by-one |
| `~/d/git/hermes-fleet-backup/plugins/fleet-hooks/spec_validator.py` | backup-only validator | DEAD-OR-DRIFT until restored | code repo if kept | restore properly or delete claim |
| `~/d/git/hermes-fleet-backup/plugins/fleet-hooks-spec/` | backup-only schemas/spec | DEAD-OR-DRIFT until restored | code repo if kept | restore properly or delete claim |
| `~/d/git/hermes-fleet-backup/docs/*` | mixed architecture docs | CODE-SOURCE docs, but misplaced | code repo | move docs to code repo |
| `~/d/git/hermes-fleet-backup/profiles/*` | backup of manual profile/config state | BACKUP-ONLY | backup repo | keep |
| `~/d/git/hermes-fleet-backup/backup*.json` | backup manifests/indexes | BACKUP-ONLY | backup repo | keep |
| `~/d/git/hermes-fleet-backup/backup.sh` / `restore.sh` | DR orchestration scripts | CODE-SOURCE if maintained | code repo, then copied if needed | move source ownership out of backup repo |

## Live wired modules confirmed

Live plugin registry currently wires:
- `acp_metrics`
- `bank_lookup`
- `circuit_breaker`
- `correction_detector`
- `discovery_context`
- `inbox_enforcer`
- `learning_sync`
- `learning_verifier`
- `manifest_context`
- `session_rules`

`injection_history.py` is **not** a hook module, but **is** live internal plumbing used by `bank_lookup.py` and `correction_detector.py`.

## Keep / simplify / kill

### Keep
- `manifest_context`
- `session_rules`
- `inbox_enforcer`
- `acp_metrics`
- `circuit_breaker`
- `bank_lookup`
- `discovery_context`
- `injection_history`

### Simplify
- `correction_detector`
- `learning_sync`
- `learning_verifier`

### Restore-or-kill
- `spec_validator.py`
- `fleet-hooks-spec/`
- any doc claims that validation is live when runtime does not load it

## Boundary rule

**Runtime is where code runs. Repo is where code lives. Backup is where machine/manual state survives.**
