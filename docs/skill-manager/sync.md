# Skill Sync (v1.2)

> Bidirectional reconciliation between a unit's deployed copies on
> tool homes and the source-of-truth in its declared repo. Layers
> on top of v1 (install / update) and the v1.1 promote flow — see
> `docs/skill-manager/promote.md` for the one-way promote path and
> `.agents/goals/ainb-skill-manager-v1.2-rollup-plan.md` §D for the
> full v1.2 spec this document summarises.

## Why sync exists

`ainb skill install` and `ainb skill update` are one-way fetches:
the repo wins. But users routinely tweak a deployed skill in their
tool home (a typo, a clearer prompt) and want that improvement to
land back in the source repo — and they want the reverse, too:
pulling teammate edits down into every tool home in one shot. Sync
is the one command that handles both directions; the planner
decides which.

## Architecture

```text
SyncPlanner       ─▶  inspect home vs repo per unit (pure)
                       │
                       ▼
                   SyncPlan = Vec<SyncAction>
                       │
        ┌──────────────┼──────────────┐
        ▼              ▼              ▼
   ToHome         ToRepo          NoOp
   (apply_to_home) (apply_to_repo) (skip)
```

## SyncPlanner (D.1)

`ainb_skill_core::sync::plan_sync(source, mappings, units) -> Vec<SyncAction>`
where each `SyncAction { unit_name, direction: ToRepo | ToHome |
NoOp, reason }`. Pure — uses `LockedUnit.sha` (deployed git ref) +
mtime fallback when refs are absent. The `reason` field carries a
human-readable explanation that the CLI surfaces in `--dry-run`.

## TO_HOME executor (D.2)

`apply_to_home(action, tool_home, source, unit_path, fetcher)`:
fetches the file at the source ref via a minimal `ContentFetcher`
trait (kept inside `ainb-skill-core` to avoid a dep cycle with
`ainb-fetch`) and writes it to the mapped home path. Honours
`AINB_TOOL_HOME_<TOOL>` for sandbox isolation; idempotent.

## TO_REPO executor (D.3)

`apply_to_repo(action, opts, fetcher)`:

1. Copies the home file to the cache repo under
   `~/.agents-in-a-box/promote-cache/<user>/<repo>/`.
2. `git add --` the changed file.
3. `git commit -m "sync: <unit-name>"` with a placeholder identity
   when global git config has none (matches the promote flow).
4. `git push -- origin <ref>` — gated by
   `AINB_SYNC_SKIP_PUSH=1` so tripwires exercise the local-commit
   path without configuring a real remote.

`git push` rejects refs starting with `-` to close argv-smuggling
vectors like `--receive-pack=cmd`, matching the hardening in the
drift detector and promote-cache cloner.

## `ainb skill sync` CLI (D.4)

```bash
ainb skill sync                   # bidirectional, every eligible unit
ainb skill sync <source-or-unit>  # scope to a source or single unit
ainb skill sync --dry-run         # print the plan, don't apply
ainb skill sync --to-home         # only ToHome actions
ainb skill sync --to-repo         # only ToRepo actions
```

`SyncArgs` derives `Default` so callers can override one field
without expanding every one. Existing five test callsites use
`..Default::default()` to stay byte-identical.

## `[s]` keybind on Units panel (D.5)

`[s]` is overloaded: when the selected unit has a conflict peer
(`shadowed_by` set), the keypress fires `ConflictFlip` as before;
otherwise it fires `AppEvent::SkillManagerSync` and runs the same
flow as the CLI for the focused unit. The help-bar `[s]` legend
reads `sync`. The conflict-peer check reads the manifest from
`$AINB_HOME/manifest.yaml` on the hot keystroke path — sub-millisecond
on typical manifests; a `selected_unit_has_conflict_peer` cache is
the documented follow-up if profiling ever flags it.

## Auto-infer target_layout (D.6)

Confirmed in `auto_infer_gh_layout_tests`: when a `gh:owner/repo`
source declares no explicit `target_layout`, `MappingEngine::resolve_pair`
falls through to `BOOTSTRAP_DEFAULT_MAPPINGS` (shipped in C.3),
covering the canonical `agents/`, `skills/`, and `commands/` layouts.
Six confirmation tests pin this contract so a future narrowing trips
a named tripwire.

## Acceptance tripwires

- `sync_to_home_tests`, `sync_to_repo_tests` — executor smoke tests
  with mocked fetchers (4 + 4).
- `skill_sync_roundtrip_tests` — full CLI roundtrip: edit home →
  `ainb skill sync` → assert cache repo has new commit; reverse
  edit → sync → assert home file matches (2).
- `skill_manager_sync_keybind_tests` — `[s]` routing (3).
- `auto_infer_gh_layout_tests` — bootstrap-fallback confirmation (6).
