---
title: Usage tracking
description: How the skill manager records and surfaces per-unit usage.
---

> Per-unit invocation counts + last-used timestamps surfaced in the
> SkillManager Detail pane. Layers on top of v1 (manifest +
> lockfile) — see `ainb-tui/plans/skill-manager/spec.md` for v1
> mechanics, and `.agents/goals/ainb-skill-manager-v1.2-rollup-plan.md`
> §B for the full v1.2 spec this document summarises.

## Why usage tracking exists

Before v1.2 the Detail pane showed only the unit URI and its
deployed paths — useful for verifying *what* is installed but
silent on *whether you actually use it*. v1.2 adds a lockfile-side
counter that the SkillManager can render at a glance, so users can
prune skills they have not invoked in months and recognise the ones
they reach for every day.

## Architecture

```text
ainb skill usage           ─▶  scan tool session logs (read-only)
                                │
                                ▼
                            UsageCache  (LockedUnit.usage)
                                │
                                ▼
                            SkillsScreenData::detail
                                │
                                ▼
                            "12 invocations · last used 3h ago"
```

## Lockfile schema (B.1)

`LockedUnit.usage: UsageRecord` is a new field on schema v2:

```yaml
units:
  - uri: gh:owner/repo@main/skills/commit
    declared_uri: gh:owner/repo/skills/commit
    sha: <fetched-sha>
    deployed:
      claude:
        status: deployed
        path: ~/.claude/skills/commit/SKILL.md
    usage:
      last_used_at: "2026-05-01T00:00:00Z"     # RFC 3339, optional
      invocations: 12                          # u64, default 0
```

Legacy v1 lockfiles without `usage` parse cleanly — the field is
`serde(default, skip_serializing_if = "UsageRecord::is_empty")`, so
roundtrip byte-stability is preserved.

## Per-tool invocation detector (B.2)

`ainb_usage::detect_invocations(tool_home, unit_name)` returns an
`InvocationRecord { invocations: u64, last_used_at: Option<String> }`
from a pure read of the tool's session log directories. No mutation,
no network. Adapters sandbox via `AINB_TOOL_HOME_<TOOL>` so tests
never touch the real `~/.<tool>` tree.

## `ainb skill usage` CLI (B.3)

```bash
ainb skill usage                  # refresh every locked unit
ainb skill usage <unit-name>      # one unit
ainb skill usage --verbose        # print per-unit counts as they land
```

For each targeted unit, the subcommand walks every tool the unit is
deployed to, sums `detect_invocations` results (keeping the most
recent timestamp across tools), and writes the aggregate to the
lockfile. Idempotent — running twice on a quiet system produces the
same lockfile.

## Detail pane render (B.4)

`SkillsScreenData::compute_detail_for_selected` reads `usage` from
the matching `LockedUnit` and populates `UnitDetail::{last_used,
invocations}`. The Detail pane formats the line as:

```text
Usage: 12 invocations · last used 3h ago
```

The "time-ago" string is computed against `chrono::Utc::now()` (no
chrono dep added at the `ainb-skill-core` crate boundary — the
timestamp is stored as a string and parsed at the render layer).
Unparseable timestamps fall back to the raw value.

## Acceptance tripwire

`tripwire_core_skill_manager_usage_renders.rs` seeds a temp `$home`
with a manifest + lockfile carrying `usage{last_used_at,
invocations: 12}`, calls `SkillsScreenData::load_from_disk`, renders
via ratatui's `TestBackend`, and asserts the rendered buffer
contains `"12 invocations"`.
