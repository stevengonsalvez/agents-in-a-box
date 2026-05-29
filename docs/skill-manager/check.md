# Skill Drift Check (v1.2)

> Per-unit drift detection between each locked unit's pinned SHA
> and its source's current upstream tip. See
> `.agents/goals/ainb-skill-manager-v1.2-rollup-plan.md` §E for the
> full v1.2 spec this document summarises.

## Why drift detection exists

Locked units pin a source SHA, but the upstream repo keeps moving.
Before v1.2 there was no surface that said *"the version you
installed is 4 commits behind"* — users had to run `ainb skill
update --check` per-source. Drift detection replaces that with a
single CLI + an always-on column in the SkillManager TUI.

## Architecture

```text
DriftBackend (trait)
└── GitLsRemoteBackend (production: `git ls-remote --`)
└── MockBackend         (tests: synthesise any DriftStatus variant)
        │
        ▼
DriftStatus { InSync | Outdated{behind} | Ahead{ahead} | Diverged{ahead,behind} }
        │
   ┌────┴────┐
   ▼         ▼
ainb skill check    SkillsScreenData::drift_cache
                                  │
                                  ▼
                            Units panel "status" column
                            (✓/⚠/▲/⟷ glyphs)
```

## DriftDetector (E.1)

`ainb_skill_core::drift::detect_drift(unit, source) -> Result<DriftStatus>`
delegates to a `DriftBackend` trait so production uses
`git ls-remote --` and tests inject a `MockBackend` that
synthesises any of the four enum variants without network. The
trait is `Send + Sync` so an `Arc<dyn DriftBackend>` can cross
thread boundaries for the async background poll.

The production backend rejects repo URLs and refs that start with
`-` and inserts an explicit `--` argv terminator before positional
values, closing the `--upload-pack=<cmd>` flag-smuggling vector.

## `ainb skill check` CLI (E.2)

```bash
ainb skill check                  # report every locked unit
ainb skill check <source>         # scope to one source
ainb skill check --json           # machine-readable [{unit, status, ahead?, behind?}, …]
```

Default output is a fixed-width tabular report (`unit`, `status`),
no external table dep. `--json` emits `serde_json` for scripts.
Empty lockfile → `# no units in lockfile`. Out-of-scope source →
`# no units match the requested source`.

## Units panel status column (E.3)

`SkillsScreenData::drift_cache: BTreeMap<String, DriftStatus>` is
keyed by `UnitRow.declared_uri` (new field in v1.2). The Units
table renders the status as a glyph:

| variant | glyph | colour |
|---|---|---|
| `InSync` | `✓` | green |
| `Outdated` | `⚠` | amber |
| `Ahead` | `▲` | cyan |
| `Diverged` | `⟷` | red |
| (missing from cache) | `…` | muted |

The Detail pane shows the commit count under the active unit (for
`Outdated { behind: 4 }` it reads `4 commits behind upstream`).

## Background drift poll (E.4)

`GoToSkillManager` triggers an async `DriftBackend::detect_all`
(tokio `spawn_blocking` over the `Arc<dyn DriftBackend>` + an
`mpsc::unbounded_channel`). Results land in `drift_cache` on the
next `AppState::tick`. The table renders `…` placeholders until
results arrive, so the screen stays interactive. A second
`GoToSkillManager` while a poll is in flight coalesces — the second
request is a no-op until the first lands.

## Acceptance tripwires

- `drift::` unit tests (14) — every variant reachable through the
  Mock backend; backend errors propagate.
- `skill_check_tests` (4) — tabular + JSON + `--source` scoping +
  empty-lockfile path.
- `tripwire_core_skill_manager_drift_column` (5) — every glyph +
  colour combination rendered.
- `tripwire_core_skill_manager_drift_background_poll` (2) — async
  fetch lands in `drift_cache`; second start coalesces.
