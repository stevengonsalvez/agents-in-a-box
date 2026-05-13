# Fleet Lambda Overlay Migration Plan

## Canonical boundary

### Code repos own
- fleet hook source code
- tests
- schemas/specs
- behavior docs
- install/sync scripts
- backup/restore script source when maintained as code

### Runtime owns
- deployed copies under `~/.hermes/plugins/` and `~/.hermes/bin/`
- ledgers under `~/.clan/learnings/`
- generated sqlite indexes and caches

### Backup repo owns
- manual profile/config snapshots
- DR copies of non-code machine state
- backup indexes/manifests

## Recommended next moves

### Phase 1 — docs + boundary freeze
1. Land ownership matrix and provenance design docs.
2. Freeze claim that backup repo is canonical for validator/spec code.
3. Audit every doc line that implies live validation exists.

### Phase 2 — source extraction
1. Choose canonical code home for fleet overlay source.
2. Copy live-authored hook/plugin code into that repo.
3. Copy or rewrite maintained runtime scripts into same code repo.
4. Add tests and sync/install scripts.

### Phase 3 — validator truth
1. If schema validation matters, restore `spec_validator.py` plus schema tree in canonical code repo.
2. Add tests.
3. Wire it live only after verification.
4. Otherwise delete stale docs/spec claims.

### Phase 4 — provenance implementation
1. Emit `lookup`, `injection`, and `correction_after_injection` events.
2. Add session-level inspection tooling.
3. Add usefulness/noise rollup later if signal quality is good.

## Smallest safe implementation slice

If full migration is too big for one pass:
1. keep runtime code untouched
2. land docs + boundaries first
3. add provenance event sink design next
4. then cut code moves in separate PRs

## Current call

This repo now holds the planning/docs layer for Fleet Lambda overlay cleanup.
Actual runtime source migration still needs explicit canonical code-home decision before broad file moves.
