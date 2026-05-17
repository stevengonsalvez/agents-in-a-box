# Pre-flight report — reflect-kb monorepo consolidation
Generated: 2026-05-17T19:57:08Z

## Subtree feasibility
- reflect-kb commits: 19
- reflect-kb working tree: clean
- agents-in-a-box commits: 1152
- agents-in-a-box working tree: clean (only known untracked dirs: `.herenow/`, `ainb-tui/plans/session-status-filter.md`, `ainb-tui/research/`, `skills/`)
- reflect-kb pushed to origin: yes (0 ahead of `origin/main`)
- agents-in-a-box pushed to origin: yes (0 ahead of `origin/main`)

## Cross-repo deps
- Python import refs (`from reflect_kb|import reflect_kb` in ai-coder-rules): 0
- Install URL refs (need Phase 5 update): 15 hits across 9 files
  - `toolkit/packages/plugins/reflect/skills/ingest/SKILL.md:79`
  - `toolkit/packages/plugins/reflect/skills/reflect-status/SKILL.md:257`
  - `toolkit/packages/plugins/reflect/hooks/reflect-drain-bg.sh:393`
  - `toolkit/packages/plugins/reflect/hooks/reflect-drain-bg.sh:423`
  - `toolkit/external-dependencies.yaml:35`
  - `toolkit/external-dependencies.yaml:51`
  - `toolkit/external-dependencies.yaml:391`
  - `toolkit/README.md:66`
  - `toolkit/bootstrap.js:796`
  - `toolkit/bootstrap.js:817`
  - `toolkit/packages/plugins/reflect/README.md:259`
  - `toolkit/packages/plugins/reflect/README.md:277`
  - `toolkit/packages/plugins/reflect/README.md:323`
  - `toolkit/scripts/update-externals.sh:131`
  - `README.md:375`

## Marketplace.json
- Root (`.claude-plugin/marketplace.json`): valid
- toolkit secondary (`toolkit/.claude-plugin/marketplace.json`): valid
- Current reflect source paths:
  - Root: `./toolkit/packages/plugins/reflect`
  - toolkit: `./packages/plugins/reflect`

## Path collisions
- `reflect-kb/` at root exists: no (clean for Phase 3a)
- `plugins/` at root exists: no (clean for Phase 3b)
- `toolkit/packages/plugins/` contents: `reflect` (only entry — no sibling plugins to consider)

## Plugin self-references (Phase 5 prep)
Files in `toolkit/packages/plugins/reflect/` mentioning their own old path (need updating after the `git mv`):

- `toolkit/packages/plugins/reflect/README.md` (lines 273, 274 — adapter invocation paths)
- `toolkit/packages/plugins/reflect/hooks/README.md:33`
- `toolkit/packages/plugins/reflect/adapters/base.py:42`
- `toolkit/packages/plugins/reflect/adapters/base.py:85`
- `toolkit/packages/plugins/reflect/docs/architecture.md:601`
- `toolkit/packages/plugins/reflect/docs/architecture.md:679`
- `toolkit/packages/plugins/reflect/tests/test_skill_recall_integration.py:24`
- `toolkit/packages/plugins/reflect/adapters/claude/README.md:23`
- `toolkit/packages/plugins/reflect/adapters/claude/README.md:27`
- `toolkit/packages/plugins/reflect/adapters/tests/test_copilot_adapter.py:1`
- `toolkit/packages/plugins/reflect/adapters/tests/test_claude_adapter.py:1`
- `toolkit/packages/plugins/reflect/adapters/tests/test_codex_adapter.py:1`
- `toolkit/packages/plugins/reflect/docs/reflect-architecture.html` (lines 31, 185, 388)
- `toolkit/packages/plugins/reflect/docs/design-records/2026-04-23-v4-universal-install-spec.md:29` (historical context — leave)
- `toolkit/packages/plugins/reflect/docs/design-records/2026-04-23-v3.2-single-pr-plan.md` (many lines — historical design-record, leave)

Note: design-records are historical artifacts; leaving them as-is preserves accurate record of where files lived at the time. Phase 5 should update live code/docs but skip the dated `docs/design-records/` entries.

## Existing pyproject at root
- `pyproject.toml` at `/Users/stevengonsalvez/d/git/ai-coder-rules/`: no

## Verdict
- Ready for Phase 3a (subtree import)? **YES** — reflect-kb working tree clean, both repos pushed, no `reflect-kb/` collision at root, no Python import surprises.
- Ready for Phase 3b (plugin git mv)? **YES** — no `plugins/` collision at root, only one plugin lives under `toolkit/packages/plugins/`, both marketplace.json files valid and patchable.
- Any blockers / pre-cleanup needed? **No blockers.** Untracked dirs in agents-in-a-box (`.herenow/`, `ainb-tui/plans/`, `ainb-tui/research/`, `skills/`) are unrelated and pre-existing; they won't collide with the subtree import or the `git mv`. Phase 5 has a known scope of 15 install-URL refs + ~12 plugin self-path refs (excluding design-records) to update.
