# Specification: reflect-kb v4 — Universal Cross-Harness Install

> **Version reslot (2026-05-31):** the plugin's `4.0.0` release was taken by the
> cost rearchitecture (W1–W5; see `../../CHANGELOG.md`). This "universal install"
> effort now targets **5.0.0**. The "v4" here is the original codename, not the
> plugin version it ships under.

**Generated from:** Interview session 2026-04-23 with Stevie
**Scope:** The follow-up work after PR #43 (v3.1.0) ships.
**Version:** 1.0

## Executive Summary

Extract the reflect retrieval system from the ai-coder-rules toolkit and ship it as a standalone, cross-harness installable with two-tier knowledge (personal + team-shared), using Nix as the canonical packager and pipx as a fallback. First-class support for Claude Code, Codex CLI, and GitHub Copilot. PR #43 ships as-is on Claude Code; this spec describes v4, which makes it universal.

## Objectives

### Primary Goals
- Single canonical install path that works across Claude Code, Codex, and Copilot
- Team-shared knowledge base via git, with quality gates on what gets shared
- Personal KB stays local; users choose what to promote to the team
- Eliminate the broken-transitive-deps problem (nano-graphrag) via deterministic packaging

### Success Metrics
- `nix run github:stevengonsalvez/reflect-kb -- --version` works on any macOS/Linux dev machine without prior setup
- `pipx install reflect-kb` as a fallback install, same CLI surface
- New engineer on a team clones the team KB, runs `reflect init`, queries against it within 10 minutes
- Zero silent write-paths: every `/reflect` output is either local or has a traceable PR
- Recall cache hit rate visible in `reflect stats`

## Scope

### In Scope
- Extracting `learnings_cli.py` + `graph_engine.py` + `entity_store.py` + `recall.py` + hooks + skill files from `toolkit/packages/plugins/reflect/` into a new `reflect-kb` repo
- Nix flake that bundles CLI + Python deps (nano-graphrag, qmd, sentence-transformers)
- pipx-installable Python package as fallback
- Two-KB model: `~/.learnings/` (personal) + configurable team KB path
- YAML frontmatter schema + pre-commit validation for team KB
- Confidence-gated write flow (HIGH auto-commit, MED/LOW → PR)
- JSONL metrics + opt-in dashboard endpoint
- Cross-harness invocation stubs: Claude full, Codex/Copilot slash-command only

### Out of Scope (v4)
- Cursor support (no skill concept, would need rules-only mode)
- MCP server wrapping (deferred — reconsider if Claude/Cursor both adopt MCP hooks)
- Windows native support (WSL acceptable)
- Multi-tenant / enterprise SSO for team KB

### Future Considerations
- MCP server exposure once MCP adoption stabilizes
- Dashboard self-hosting guide (Grafana / simple FastAPI)
- Cursor rules-mode port once Cursor's skill story matures

## Technical Requirements

### Architecture

**Two repos:**

```
reflect-kb/                       # THE TOOL (this spec's deliverable)
├── flake.nix                     # Nix package + dev shell
├── pyproject.toml                # pipx fallback
├── cli/                          # learnings_cli.py, recall.py, etc.
├── skills/                       # SKILL.md files (tool-agnostic via {{HOME_TOOL_DIR}})
├── hooks/                        # session_start_recall.py
├── schema/                       # YAML frontmatter JSON Schema
└── harness-adapters/             # Per-tool install scripts
    ├── claude-code/              # plugin.json + hooks wiring
    ├── codex/                    # ~/.codex/skills/ wiring
    └── copilot/                  # ~/.copilot/ skill wiring

team-kb/                          # THE CONTENT (one per team, separate repo)
├── documents/                    # Learnings .md + .entities.yaml sidecars
├── .github/workflows/            # Pre-commit schema validation in CI
├── .pre-commit-config.yaml       # Local schema check
├── CODEOWNERS                    # Review routing by category
└── README.md                     # How to contribute

# Personal KB lives at ~/.learnings/, gitignored by default, not a repo.
```

### Components

| Component | Purpose | Technology |
|-----------|---------|------------|
| reflect CLI | Unified entry: add, search, stats, reindex, share | Python + click |
| recall.py | Hybrid retrieval (GraphRAG + QMD + RRF) | Python + nano-graphrag + qmd |
| session_start_recall.py | Auto-inject prior art on session start (Claude only) | Python subprocess |
| reflect-flake | Nix derivation bundling CLI + all deps | Nix |
| schema validator | Enforces YAML frontmatter on team KB writes | jsonschema + pre-commit |
| metrics writer | Append-only JSONL of queries, hits, latencies | stdlib |

### Install UX

**Primary (Nix):**
```bash
# One-time Nix install (for users who don't have it)
curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install

# Install reflect-kb
nix profile install github:stevengonsalvez/reflect-kb

# Per-harness adapters (run once per harness you use)
reflect adapter install claude-code
reflect adapter install codex
reflect adapter install copilot
```

**Fallback (pipx):**
```bash
pipx install reflect-kb
# deps with broken chains get bundled wheels or --no-deps workarounds
reflect adapter install claude-code
```

**Per-harness adapters** copy skill files + wire hooks where applicable:
- Claude: drops plugin.json + hook entry into `~/.claude/settings.json`
- Codex: copies skills to `~/.codex/skills/`
- Copilot: copies skills to `~/.copilot/skills/`

### Team KB Write Flow (Confidence-Gated Hybrid)

```
  /reflect in Claude
        │
   ┌────▼────────────────────────┐
   │ Extract + classify learning │
   └────┬─────────────┬──────────┘
        │             │
    HIGH conf      MED/LOW conf
        │             │
        ▼             ▼
   ┌──────────┐  ┌──────────────┐
   │ Direct   │  │ Write to     │
   │ commit + │  │ worktree,    │
   │ push to  │  │ open PR with │
   │ team-kb  │  │ team-kb      │
   │ main     │  │ reviewers    │
   └──────────┘  └──────────────┘

ALWAYS writes local copy to ~/.learnings/ first (no data loss if push fails).
```

**Quality gate (pre-commit in team-kb repo):**
- Frontmatter schema validation (title, category, confidence, tags, entities)
- Required `entities.yaml` sidecar for every `.md`
- No binary indexes in commits (`nano_graphrag_cache/`, `.venv/` in `.gitignore`)
- CI rebuilds indexes on merge → pushes to GitHub release or S3 artifact

### Invocation Per Harness

| Harness | Auto-recall on session start | Slash commands | Notes |
|---------|------------------------------|----------------|-------|
| Claude Code | ✅ SessionStart hook | ✅ `/reflect`, `/recall`, `/reflect:ingest` | Flagship — feature-complete |
| Codex CLI | ❌ No hook parity | ✅ skill invocation | User runs `/recall` manually |
| Copilot | ❌ No hook system | ✅ skill invocation | Same as Codex |

Rationale: Building hook equivalents in Codex/Copilot is fragile (shell wrapper acrobatics). Better to be honest: Claude is flagship, others are first-class for invocation but not for magic.

### Metrics

```jsonl
# ~/.learnings/metrics.jsonl (always on)
{"ts":"2026-04-23T14:30:00Z","op":"recall","query":"...","hits":3,"cached":true,"latency_ms":12,"harness":"claude"}
{"ts":"2026-04-23T14:35:00Z","op":"add","category":"architecture","confidence":"HIGH","shared":true}
```

**Opt-in dashboard:**
- `reflect config set dashboard.url https://team-dash.example.com/ingest`
- Metrics batched and POSTed every N events or on reflect command exit
- Aggregated view: team-wide recall hit rate, most-queried categories, orphan learnings (never retrieved)

## User Experience

### User Flows

**Flow 1: Fresh install (new engineer joining a team)**
1. Install Nix (if needed): `curl … install.determinate.systems/nix | sh`
2. `nix profile install github:stevengonsalvez/reflect-kb`
3. `reflect adapter install claude-code`
4. `reflect team clone git@github.com:orgname/team-kb.git`  ← clones + rebuilds indexes (~5-10 min)
5. Open any project in Claude Code → SessionStart auto-injects team learnings

**Flow 2: Capturing a learning**
1. User says "we got burned when mocked tests passed but prod migration failed"
2. Claude runs `/reflect` → extracts learning, assigns confidence
3. HIGH confidence → written to `~/.learnings/` AND pushed to team-kb main
4. MED/LOW → written to `~/.learnings/` AND PR opened against team-kb

**Flow 3: Recall (auto via SessionStart)**
1. User opens Claude Code in a project
2. Hook fires, builds query from project context
3. Parallel query: personal KB + team KB, RRF-fused, reranked
4. Top 3 learnings injected as `additionalContext`

### Edge Cases

| Scenario | Expected Behavior |
|----------|-------------------|
| Nix install fails on corporate laptop | Fall back to `pipx install` doc path |
| Team KB push fails (network / auth) | Local write succeeds; warn + queue for later retry |
| Pre-commit schema rejects a PR | Clear error message with schema path; no corruption |
| Team KB repo renamed | `reflect team reconfigure` updates the remote |
| Personal KB + team KB have conflicting learning IDs | Namespace: personal `lrn-local-*`, team `lrn-team-*` |
| Offline: neither KB reachable | Recall returns empty silently; session boots normally |
| Two engineers PR same learning | Schema includes `content_hash`; pre-commit dedups |

## Constraints & Dependencies

### Technical Constraints
- Python ≥3.11 (PEP 723 inline deps used everywhere)
- Nix ≥2.18 (flakes enabled)
- Git (obviously)
- Platforms: macOS + Linux first-class; Windows via WSL only

### External Dependencies (runtime)
- nano-graphrag (has broken transitive chain — Nix override handles it)
- qmd (for BM25 side of hybrid retrieval)
- sentence-transformers (for local embeddings, no API key)
- PyYAML, click, rich (stdlib-adjacent)

### Migration from v3 (PR #43)
- Existing `~/.learnings/` keeps working — same CLI surface
- `~/.claude/global-learnings/` deprecated, delete-safe
- Skill files move from ai-coder-rules toolkit → reflect-kb/skills
- ai-coder-rules' bootstrap.js gains an optional `install reflect-kb` adapter step

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Nix install friction blocks adoption | High | Med | pipx fallback + clear docs; Determinate installer is smooth |
| nano-graphrag breaks again on upstream update | Med | High | Pin versions in Nix/pyproject; test matrix in CI |
| Team KB becomes noisy/low-signal | High | Med | Schema + pre-commit + CODEOWNERS review for MED/LOW |
| HIGH-confidence auto-commit lands a bad learning | Med | Low | `reflect revert <id>` + weekly team-kb review job |
| Dashboard endpoint leaks sensitive queries | High | Low | Opt-in only; docs call out that queries may be proprietary |
| Codex/Copilot invocation UX degrades | Low | Med | Manual-only is still useful; auto-recall is a nice-to-have not a must |

## Decisions Made

### Key Trade-offs

- **Decision:** Nix primary + pipx fallback (not mono-Nix, not mono-pip)
  **Alternatives:** Per-harness plugins, Docker sidecar, Nix-only
  **Rationale:** Nix solves the Python dep chain cleanly; pipx broadens reach for users who can't or won't install Nix. Doubles test surface but worth it.

- **Decision:** 2 repos (tool + team KB), personal KB stays local
  **Alternatives:** Mono-repo, 3 repos, BYO KB
  **Rationale:** Tool versioning and content versioning have different cadences. Personal learnings shouldn't live in a team repo. Two is the minimum viable separation.

- **Decision:** Hybrid write flow (HIGH auto, MED/LOW PR)
  **Alternatives:** All PR-gated, all direct, opt-in share
  **Rationale:** Matches the existing confidence field in the schema. Keeps signal high (review for uncertain) without adding friction for obvious wins.

- **Decision:** Rebuild indexes locally on clone
  **Alternatives:** Commit indexes, git-lfs, shared registry
  **Rationale:** Index files are churn-heavy and don't merge. ~5-10 min one-time cost is acceptable. Keeps team-kb lean.

- **Decision:** Claude gets hooks, others get manual
  **Alternatives:** Full parity, slash-only everywhere, MCP server
  **Rationale:** Hook parity is a rabbit hole (Copilot especially). Explicit invocation is still great UX. MCP can come later.

### Deferred Decisions
- MCP server wrapper: wait for broader MCP adoption across harnesses
- Dashboard backend: spec only the client; let teams self-host
- Cursor support: out of scope for v4

## Implementation Notes

### Priority Order (v4 phases)

1. **Phase 1 — Extract + package**: Move CLI/skills out of ai-coder-rules toolkit into standalone repo. Ship Nix flake + pipx package. Single-harness (Claude).
2. **Phase 2 — Adapters**: `reflect adapter install <harness>` for codex + copilot.
3. **Phase 3 — Team KB**: `reflect team clone/init/sync`, schema validator, CI workflow template.
4. **Phase 4 — Confidence-gated writes**: Hybrid direct/PR flow in `/reflect`.
5. **Phase 5 — Metrics + dashboard**: JSONL writer, optional POST endpoint.

### Technical Debt Accepted
- pipx path has weaker dep guarantees than Nix — document as "best-effort"
- `qmd` is a separate install via `uv tool install` inside pipx path; Nix bundles it cleanly
- Pre-commit validation runs locally and in CI — duplicate work but catches errors earlier

## Open Questions

- [ ] Auth model for private team-kb repos — PAT? SSH? Both?
- [ ] Naming: `reflect-kb` vs `agent-kb` vs `learnings` — brand check needed
- [ ] Does team-kb CI need to regenerate the GraphRAG cache on merge and publish as release artifact, or is local rebuild always acceptable?
- [ ] Embedding model bump strategy — `all-mpnet-base-v2` embeddings don't cross-version; need migration plan when upgrading

## What This Spec Does NOT Cover

- The actual PR #43 (that's shipping as-is, Claude-only)
- Dashboard backend implementation (client spec only)
- Cursor or other rules-based harnesses
- Enterprise features (SSO, audit log, RBAC on team KB)
- Windows native support

---

*This specification was generated through systematic interview and should be treated as the starting design for a follow-up implementation plan. Next step: create a `/plan` from this spec to break into concrete GitHub issues.*
