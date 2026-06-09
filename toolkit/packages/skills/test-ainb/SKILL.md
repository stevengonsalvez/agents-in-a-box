---
name: test-ainb
description: Run tests for the ainb (agents-in-a-box) Rust workspace via a 5-layer strategy — unit, insta snapshot, mock-plugin compositing, real-plugin spawn, vhs recording. Wraps cargo + insta + vhs into one CLI. Use when Stevie says "/test-ainb", "test ainb", "run ainb tests", "snapshot <component>", "regenerate vhs tapes", or any phrasing about validating ainb test layers. The skill autodetects which ainb-tui worktree the cwd sits in and dispatches to scripts/run.sh.
user-invocable: true
---

# /test-ainb — ainb 5-layer test runner

When invoked, parse the user's arguments and dispatch to `scripts/run.sh`. The script does all the work — argv parsing, workspace autodetect, layer dispatch, formatted output. Do not narrate; just run it and surface output.

## Invocation

```bash
{{HOME_TOOL_DIR}}/skills/test-ainb/scripts/run.sh [FLAGS]
```

Pass the user's arguments straight through. If user typed `/test-ainb` with no args, run with no args (defaults to `--layer quick` = L1 + L2).

## Argument forwarding

| User typed | Forward as |
|------------|-----------|
| `/test-ainb` | `scripts/run.sh` |
| `/test-ainb --layer L1` | `scripts/run.sh --layer L1` |
| "run ainb tests" | `scripts/run.sh` |
| "snapshot the session_list component" | `scripts/run.sh --component session_list` |
| "test ainb at layer all" | `scripts/run.sh --layer all` |
| "regenerate vhs tapes" | `scripts/run.sh --vhs all --update` |
| "run ci tests" | `scripts/run.sh --ci` |
| "spawn burndown plugin test" | `scripts/run.sh --plugin burndown` |

## Layers (quick reference)

| Layer | Scope | When |
|-------|-------|------|
| L1 | Unit (cargo test --lib --bins --tests) | Always — fastest, broadest. `--tests` is required: without it, integration tests under `crates/*/tests/` are skipped. |
| L2 | Insta snapshot (TestBackend) | Component renders — width-aware regressions |
| L3 | Mock-plugin compositing | BSP tile composition without subprocess spawn |
| L4 | Real-plugin spawn (`--ignored`) | Slow: actually spawns subprocess plugin |
| L5 | VHS recording | Manual visual regression — produces .gif artefacts |

Full layer detail: see `references/layers.md`.

## Defaults

- No args → `--layer quick` = L1 + L2.
- `--layer all` → L1 + L2 + L3 (L4 + L5 require explicit opt-in via `--plugin` / `--vhs`).
- `--ci` → CI subset (L1 + L2, single-threaded).

## What NOT to do

- Don't pre-narrate ("I'll run the tests now") — just invoke the script.
- Don't summarise the script output unless user explicitly asks; the script's own summary is the answer.
- Don't try to interpret test failures inside this skill — surface them and let Stevie decide.
- Don't auto-run L4 or L5 — they're explicit opt-in. `--layer all` deliberately excludes them.
- Don't hardcode any worktree path; the script autodetects.

## Tooling prerequisites

- `cargo` (Rust toolchain) — required for L1–L4.
- `cargo-insta` — required for L2 with `--update`. Install hint: `cargo install cargo-insta`.
- `vhs` — required for L5. Install hint: `brew install vhs`.
- Script prints install hints automatically when tooling is missing.

## Memory notes baked into the script

- **Volatile worktrees** (`user_volatile_worktrees`): workspace autodetect walks up from cwd; never hardcodes a path.
- **Verify worker test-green claims** (`feedback_verify_worker_test_green_claim`): L1 runs the full workspace incl. crates' integration `tests/` dirs.
- **insta trailing-newline trap** (`reference_insta_trailing_newline_trap`): tests must `trim_end_matches('\n')`; skill does NOT auto-accept bad snapshots — `--update` is explicit.
- **VHS sleep budget** (`reference_vhs_sleep_budget`): tapes themselves bake in correct sleeps (ainb home cold ~30s, post-`i` 40-60s); the skill just records.
