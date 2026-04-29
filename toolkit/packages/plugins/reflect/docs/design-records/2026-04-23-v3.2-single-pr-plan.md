# Plan: Reflect v3.2 Single-PR Delivery

**Date**: 2026-04-23  
**Repository**: ai-coder-rules  
**Branch**: reflect/retrieval-phase-1-2  
**Status**: Planned

## Goal

Deliver `reflect v3.2` as **one PR** that upgrades the current reflect plugin from a good script collection into a coherent, enterprise-grade subsystem with:

- SQLite as the single operational control plane
- explicit lifecycle/state-machine semantics
- normalized multi-tool provenance
- pluggable indexing backends
- recall feedback logging
- comprehensive end-to-end verification

This is a **single integrated refactor**, not a sequence of separate PRs.

## Non-Negotiable Constraints

- **One PR only**
- Must be **comprehensively testable** before opening the PR
- Must preserve the current good direction already present in:
  - `toolkit/packages/plugins/reflect/reflect.toml`
  - `toolkit/packages/plugins/reflect/scripts/reflect_config.py`
  - `toolkit/packages/plugins/reflect/scripts/reflect_db.py`
  - `toolkit/packages/plugins/reflect/scripts/memory_discovery.py`
  - `toolkit/packages/plugins/reflect/scripts/providers/*`
- Avoid another full rewrite. Refactor toward clearer boundaries.

---

## Current Baseline

The current plugin already has the right foundational pieces:

- **Layered TOML config**
  - `toolkit/packages/plugins/reflect/reflect.toml`
  - `toolkit/packages/plugins/reflect/scripts/reflect_config.py`
- **SQLite state**
  - `toolkit/packages/plugins/reflect/scripts/reflect_db.py`
- **Provider abstraction**
  - `toolkit/packages/plugins/reflect/scripts/memory_discovery.py`
  - `toolkit/packages/plugins/reflect/scripts/providers/claude.py`
  - `toolkit/packages/plugins/reflect/scripts/providers/codex.py`
  - `toolkit/packages/plugins/reflect/scripts/providers/copilot.py`
  - `toolkit/packages/plugins/reflect/scripts/providers/gemini.py`
- **Existing integration test seed**
  - `toolkit/packages/plugins/reflect/tests/test_reflect_system.py`

The v3.2 work should consolidate and harden these, not replace them.

---

## v3.2 Outcomes

By the end of the PR, the reflect system should satisfy all of the following:

1. Lifecycle state is explicit and enforced through services.
2. Learnings have first-class provenance and supersession metadata.
3. Discovery output from Claude, Codex, Copilot, and Gemini normalizes into one shape.
4. Indexing is routed through backend interfaces rather than ad hoc GraphRAG-only calls.
5. Recall logs usage and feedback for future ranking improvements.
6. Tests cover schema, migration, provider normalization, lifecycle, indexing, recall, and end-to-end flows.
7. Skills and docs accurately describe the behavior of the actual system.

---

## Target Architecture

### Keep

- `scripts/reflect_config.py` as the config loader
- `scripts/reflect_db.py` as the database entrypoint
- `scripts/providers/*` as source adapters
- `docs/solutions/*.md` + `.entities.yaml` as human-readable artifacts
- `skills/*` as user-facing entrypoints

### Add

```
toolkit/packages/plugins/reflect/scripts/
├── domain/
│   ├── __init__.py
│   ├── models.py
│   ├── enums.py
│   ├── dedup.py
│   ├── provenance.py
│   └── privacy.py
├── services/
│   ├── __init__.py
│   ├── reflect_service.py
│   ├── consolidate_service.py
│   ├── ingest_service.py
│   ├── recall_service.py
│   └── status_service.py
├── indexers/
│   ├── __init__.py
│   ├── base.py
│   ├── graphrag.py
│   └── qmd.py
├── artifacts/
│   ├── __init__.py
│   ├── knowledge_notes.py
│   └── sidecars.py
└── cli/
    ├── __init__.py
    └── doctor.py
```

### Architectural Rule

Only **service layer methods** may change lifecycle state. Skills, hooks, and CLI wrappers call services. They do not perform direct state transitions themselves.

---

## Data Model Changes

All schema work lands in the same PR and is migrated in-place through `reflect_db.py`.

### Extend `learnings`

Add:

- `scope TEXT NOT NULL DEFAULT 'project'`
- `source_provider TEXT NOT NULL DEFAULT ''`
- `source_kind TEXT NOT NULL DEFAULT ''`
- `source_quote TEXT NOT NULL DEFAULT ''`
- `source_quote_hash TEXT NOT NULL DEFAULT ''`
- `session_id TEXT NOT NULL DEFAULT ''`
- `thread_id TEXT NOT NULL DEFAULT ''`
- `privacy_level TEXT NOT NULL DEFAULT 'internal'`
- `artifact_path TEXT NOT NULL DEFAULT ''`
- `sidecar_path TEXT NOT NULL DEFAULT ''`
- `supersedes_learning_id TEXT`
- `superseded_by_learning_id TEXT`
- `last_recalled_at TEXT`
- `recall_count INTEGER NOT NULL DEFAULT 0`
- `helpful_count INTEGER NOT NULL DEFAULT 0`
- `ignored_count INTEGER NOT NULL DEFAULT 0`
- `stale_count INTEGER NOT NULL DEFAULT 0`

### Extend `proposals`

Add:

- `proposal_type TEXT NOT NULL DEFAULT 'learning'`
- `target_kind TEXT NOT NULL DEFAULT ''`
- `target_path TEXT NOT NULL DEFAULT ''`
- `decision_actor TEXT NOT NULL DEFAULT ''`
- `decided_at TEXT`
- `materialized_at TEXT`
- `materialization_error TEXT`
- `rationale_json TEXT NOT NULL DEFAULT '{}'`

### Extend `events`

Add:

- `actor TEXT NOT NULL DEFAULT ''`
- `parent_event_id TEXT`
- `idempotency_key TEXT NOT NULL DEFAULT ''`

Add indexes:

- `(type, learning_id)`
- unique `idempotency_key` when non-empty

### Extend `sources`

Add:

- `source_kind TEXT NOT NULL DEFAULT ''`
- `provider_id TEXT NOT NULL DEFAULT ''`
- `canonical_project_id TEXT NOT NULL DEFAULT ''`
- `first_seen TEXT NOT NULL DEFAULT ''`
- `archived_at TEXT`
- `ingest_state TEXT NOT NULL DEFAULT 'discovered'`

### New table: `index_jobs`

Columns:

- `id`
- `learning_id`
- `backend`
- `status`
- `idempotency_key`
- `attempt_count`
- `last_error`
- `created_at`
- `started_at`
- `finished_at`

### New table: `recall_events`

Columns:

- `id`
- `learning_id`
- `query`
- `query_hash`
- `source_context`
- `rank`
- `feedback`
- `created_at`

### New table: `artifacts`

Columns:

- `id`
- `learning_id`
- `artifact_type`
- `path`
- `content_hash`
- `status`
- `created_at`

---

## Lifecycle State Machine

These states must be explicit in code and documented in tests:

- `detected`
- `proposed`
- `approved`
- `materialized`
- `indexed`
- `recalled`
- `superseded`
- `reverted`
- `rejected`

### Transition Rules

- Only services may transition states.
- Every transition must emit an event row.
- Indexing must be idempotent.
- Revert must not silently delete provenance.
- Supersession must link old and new learning rows.

---

## Config Changes

Extend `toolkit/packages/plugins/reflect/reflect.toml` and `reflect_config.py` to support:

```toml
[schema]
version = 3

[privacy]
redact_secrets = true
redact_paths = true
allowed_roots = ["docs/solutions", ".agents"]
blocked_globs = ["**/.env*", "**/secrets/**"]

[dedup]
mode = "hash_then_semantic"
supersession_threshold = 0.90
duplicate_threshold = 0.97

[recall]
log_feedback = true
cache_ttl_seconds = 3600
default_limit = 10

[indexers.qmd]
enabled = true

[indexers.graphrag]
enabled = true

[policies]
require_approval_for_behavioral = true
require_approval_for_global_index = true
auto_reject_stale_pending_days = 14
```

### Config Requirements

- preserve current layered config behavior
- add schema versioning
- add env overrides for new keys where appropriate
- avoid introducing provider-specific policy leakage into shared logic

---

## Implementation Slices Inside the Single PR

These are **internal branch slices**, not separate PRs.

### Slice 1: Domain and enums

Create:

- `scripts/domain/models.py`
- `scripts/domain/enums.py`

Define:

- typed records for `Learning`, `Proposal`, `SourceRecord`, `IndexJob`, `RecallEvent`
- lifecycle/status enums
- helper constructors for normalized provider outputs

### Slice 2: Schema and migrations

Update:

- `scripts/reflect_db.py`

Add:

- additive migrations for all new columns/tables
- idempotency indexes
- schema-version helper

Keep:

- compatibility with current DBs
- compatibility with `migrate_v2.py`

### Slice 3: Service layer

Create:

- `scripts/services/reflect_service.py`
- `scripts/services/consolidate_service.py`
- `scripts/services/ingest_service.py`
- `scripts/services/recall_service.py`
- `scripts/services/status_service.py`

Move behavior out of:

- `state_manager.py`
- `metrics_updater.py`
- scattered skill instructions

Result:

- state transitions happen through services
- DB mutation semantics are centralized

### Slice 4: Indexer abstraction

Create:

- `scripts/indexers/base.py`
- `scripts/indexers/graphrag.py`
- `scripts/indexers/qmd.py`

Rules:

- no skill or service calls GraphRAG CLI directly
- indexing jobs are tracked in `index_jobs`
- failures are recorded, not swallowed
- re-runs are idempotent

### Slice 5: Provenance, privacy, dedup

Create:

- `scripts/domain/provenance.py`
- `scripts/domain/privacy.py`
- `scripts/domain/dedup.py`

Implement:

- normalized provenance contract across providers
- secret/path redaction before persistence
- duplicate vs supersession distinction
- deterministic quote hashing and content hashing

### Slice 6: Artifact generation

Create:

- `scripts/artifacts/knowledge_notes.py`
- `scripts/artifacts/sidecars.py`

Move or wrap logic from:

- `output_generator.py`
- `validate_sidecar.py`

Goals:

- artifact creation is deterministic
- sidecar generation is validated
- artifacts are tracked in `artifacts` table

### Slice 7: Skill and hook wiring

Update:

- `skills/reflect/SKILL.md`
- `skills/consolidate/SKILL.md`
- `skills/ingest/SKILL.md`
- `skills/recall/SKILL.md`
- `skills/reflect-status/SKILL.md`
- related hooks if needed

Make docs match:

- actual lifecycle
- SQLite-only operational model
- backend abstraction
- recall feedback behavior

### Slice 8: Cleanup and compatibility

Decide which of these become wrappers vs retire:

- `state_manager.py`
- `metrics_updater.py`
- parts of `output_generator.py`

Preferred approach:

- keep thin compatibility wrappers if existing skills/scripts still call them
- route real logic to services

---

## Comprehensive Test Strategy

This PR is only done when the reflect system is testable end-to-end.

### Test Layers

#### 1. Schema and migration tests

Cover:

- fresh DB bootstrap
- current DB -> v3.2 migration
- v2 YAML -> SQLite migration still works
- invalid lifecycle values rejected
- idempotency constraints enforced

Suggested files:

- `toolkit/packages/plugins/reflect/tests/test_reflect_migrations.py`

#### 2. Service lifecycle tests

Cover:

- detect -> propose -> approve -> materialize -> index
- reject flow
- revert flow
- supersede flow
- stale pending flow
- metrics/event side effects

Suggested files:

- `toolkit/packages/plugins/reflect/tests/test_reflect_services.py`

#### 3. Provider normalization tests

Cover:

- Claude fixture normalization
- Codex fixture normalization
- Copilot fixture normalization
- Gemini fixture normalization
- soft-failure behavior for missing providers
- provider cleanup boundary safety

Suggested files:

- `toolkit/packages/plugins/reflect/tests/test_reflect_providers.py`

#### 4. Indexer and artifact tests

Cover:

- knowledge note generation
- sidecar generation
- sidecar validation
- GraphRAG adapter invocation
- QMD adapter invocation
- index failure recording and retry behavior

Suggested files:

- `toolkit/packages/plugins/reflect/tests/test_reflect_indexers.py`

#### 5. End-to-end simulation

Extend:

- `toolkit/packages/plugins/reflect/tests/test_reflect_system.py`

Cover:

- synthetic home dir + project roots
- fake multi-provider memories
- consolidate flow
- ingest flow
- reflect flow
- recall flow
- archived originals
- generated notes
- generated sidecars
- DB rows
- events
- metrics
- recall logs
- dedup/supersession behavior

#### 6. Skill integration tests

Keep and extend:

- `toolkit/packages/plugins/reflect/tests/test_skill_recall_integration.py`

Add smoke coverage for:

- `reflect`
- `reflect:consolidate`
- `reflect:ingest`
- `reflect:recall`
- `reflect-status`

---

## Test Fixtures

Add:

```
toolkit/packages/plugins/reflect/tests/fixtures/
├── claude/
├── codex/
├── copilot/
├── gemini/
├── project_a/
└── project_b/
```

Fixture requirements:

- fake home roots for each provider
- sample memory files with overlapping content
- sample knowledge notes with and without sidecars
- intentionally stale and duplicate content
- at least one superseding learning case
- at least one redaction-sensitive input

---

## Validation Commands

The PR is not ready until all of these pass:

```bash
pytest toolkit/packages/plugins/reflect/tests -q
python3 toolkit/packages/plugins/reflect/tests/test_reflect_system.py --verbose
python3 toolkit/packages/plugins/reflect/scripts/migrate_v2.py discover
python3 toolkit/packages/plugins/reflect/scripts/validate_sidecar.py --help
python3 toolkit/packages/plugins/reflect/scripts/memory_discovery.py stats
```

### Manual smoke checks

Run at least one smoke path for each skill:

```bash
/reflect
/reflect:consolidate
/reflect:ingest
/reflect:recall "sample query"
/reflect-status
```

Manual verification checklist:

- notes created correctly
- sidecars created correctly
- DB state updated correctly
- duplicate ingest does not duplicate rows
- recall logs feedback events

---

## Definition of Done

The PR is done only when all of the following are true:

- schema changes and migrations land cleanly
- service layer owns state transitions
- provider normalization is consistent across all supported tools
- indexing is abstracted behind backend interfaces
- privacy and dedup logic are implemented
- recall feedback is persisted
- test suite covers end-to-end flows comprehensively
- skill docs reflect actual system behavior
- legacy wrapper scripts either route to services or are explicitly retired

---

## Execution Checklist

### A. Foundations

- [ ] Create `scripts/domain/` package
- [ ] Create `scripts/services/` package
- [ ] Create `scripts/indexers/` package
- [ ] Create `scripts/artifacts/` package
- [ ] Add enums and typed models

### B. Database

- [ ] Extend `learnings`
- [ ] Extend `proposals`
- [ ] Extend `events`
- [ ] Extend `sources`
- [ ] Add `index_jobs`
- [ ] Add `recall_events`
- [ ] Add `artifacts`
- [ ] Add migration logic and indexes

### C. Services

- [ ] Implement reflect lifecycle service
- [ ] Implement ingest service
- [ ] Implement consolidate service
- [ ] Implement recall service
- [ ] Implement status service

### D. Indexing and artifacts

- [ ] Add base indexer interface
- [ ] Add GraphRAG adapter
- [ ] Add QMD adapter
- [ ] Move note generation into artifacts module
- [ ] Move sidecar generation into artifacts module

### E. Governance logic

- [ ] Add provenance normalization
- [ ] Add privacy redaction
- [ ] Add duplicate detection
- [ ] Add supersession logic
- [ ] Add recall feedback persistence

### F. Skills and wrappers

- [ ] Update all reflect skill docs
- [ ] Update hooks if needed
- [ ] Convert legacy scripts to wrappers or retire them

### G. Tests

- [ ] Add migration tests
- [ ] Add service lifecycle tests
- [ ] Add provider normalization tests
- [ ] Add indexer/artifact tests
- [ ] Expand end-to-end simulation
- [ ] Add/extend skill integration tests

### H. Final validation

- [ ] Run full reflect test suite
- [ ] Run end-to-end simulation in verbose mode
- [ ] Run manual smoke tests for all reflect skills
- [ ] Review docs for behavior drift

---

## Risks to Watch

- introducing a second control plane by accident
- partial migration logic that updates DB but not docs/skills
- GraphRAG and QMD adapters diverging in behavior
- recall logging becoming write-noisy without value
- supersession logic being too eager and hiding valid distinct learnings
- legacy scripts silently bypassing the new service layer

---

## Recommended Implementation Order

Use this order inside the single PR:

1. domain models and enums
2. schema migrations
3. service layer
4. indexer abstraction
5. provenance/privacy/dedup
6. artifact generation refactor
7. skill/wrapper updates
8. comprehensive tests
9. docs cleanup and final validation

This gives the branch a stable spine early and keeps test work grounded in the final architecture instead of testing transitional code.
