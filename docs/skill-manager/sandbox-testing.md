# Sandbox Testing

> Reproducible test harness for the SkillManager — drives the TUI +
> CLI against a seeded fake `~/.claude` / `~/.codex` without touching
> your real tool homes. Two surfaces from one design: a Rust fixture
> for in-process integration tests, plus a bash script for manual TUI
> testing. Bead `ai-e7t`.

## Why sandbox testing exists

The SkillManager surface (CLI + TUI) is broad enough that
in-process unit tests can't prove the full path end-to-end. We need
a fixture that:

- seeds skills, agents, marketplace plugins, and a bare-remote git
  repo with one stroke;
- isolates the fixture from production binaries (zero fixture bytes
  in `cargo build --release`);
- powers both Rust integration tests AND manual developer TUI
  testing against a real `ainb` binary;
- never touches your real `~/.claude` or `~/.codex`.

## Two surfaces, one design

```text
┌──────────────────────────────────────────────────────────────┐
│           ainb-skill-core::fixtures (Rust API)               │
│           #[cfg(feature = "test-fixtures")]                  │
│                                                              │
│  pub fn build_skill_manager_sandbox(root, tier)              │
│      -> io::Result<SandboxLayout>                            │
└──────────────────┬───────────────────────────────────────────┘
                   │
        ┌──────────┴─────────────┐
        ▼                        ▼
   integration tests       scripts/skill-manager-sandbox.sh
   (tempfile::tempdir)     (~/.cache/ainb-sandbox)
        │                        │
        │  ── same dir layout ── │
        │  ── same env vars  ── │
        ▼                        ▼
   tempdir wiped on Drop    `down` requires .ainb-sandbox-marker
```

The two paths share design (same seeded skills, same env-var set,
same bare-remote shape) but not code — the Rust API uses
`tempfile::tempdir()` for parallel-safe tests; the bash script
writes to a stable cache path so a developer can leave the sandbox
in place between TUI sessions.

## Tiers

| Tier      | What gets seeded                                                       |
|-----------|------------------------------------------------------------------------|
| `Minimal` | 2 Claude skills (`commit` local, `fireworks-tech-graph` external-clone-shape), 1 agent, 1 Codex skill, 1 marketplace plugin (`sandbox-marketplace/discord`), bare git remote with one seed commit |
| `Full`    | Above + all 9 adapter tool homes (copilot/gemini/cursor/amazonq/claude-desktop/cline/roo each get one skill) + 2 more marketplace plugins (official/community) + manifest pre-seeded with a `shadowed_by` conflict pair + `.skip-banner` marker + real-schema `external-dependencies.yaml` |

`Minimal` is fast (~1s) and sufficient for sync + drift tests.
`Full` (~2s) is for tripwires that need multi-tool coverage,
conflict-flip exercises, or the upcoming provenance matcher.

## Manual workflow (just — recommended)

`brew install just`, then from the worktree root:

```bash
just skill-manager up --tier full       # build the sandbox
just skill-manager tui                  # launch TUI against it
just skill-manager cli skill check      # drive CLI against it
just skill-manager inspect              # see what's seeded
just skill-manager down                 # teardown
just --list skill-manager               # list every recipe
```

All env vars (`HOME`, `AINB_HOME`, `AINB_TOOL_HOME_*`,
`GIT_TERMINAL_PROMPT`, `GIT_ASKPASS`) are set inside the justfile
itself — no `source env.sh` step. Override the sandbox root with
`AINB_SANDBOX_ROOT=/tmp/foo just skill-manager up`.

## Manual workflow (raw bash — no just)

```bash
# 1. Build a sandbox (default ~/.cache/ainb-sandbox, default Minimal)
scripts/skill-manager-sandbox.sh up

# 2. Arm the env
source ~/.cache/ainb-sandbox/env.sh

# 3. Launch ainb — press [m] to enter SkillManager
./target/debug/ainb

# 4. Or drive the CLI
ainb source list
ainb skill check
echo "edit" >> ~/.claude/skills/commit/SKILL.md
ainb skill sync --to-repo --yes
git -C ~/.cache/ainb-sandbox/sandbox-remote.git log --oneline

# 5. Teardown
scripts/skill-manager-sandbox.sh down
```

### Options

| Flag                  | Default                                | Notes                                |
|-----------------------|----------------------------------------|--------------------------------------|
| `--root <dir>`        | `${XDG_CACHE_HOME:-~/.cache}/ainb-sandbox` | Where the sandbox lives             |
| `--tier minimal\|full`| `minimal`                              | How much to seed (`up` only)        |

### Safety guards

- `--root /` refused (would clobber the filesystem).
- `--root $HOME` refused (would clobber the user's real home).
- `down` refuses any directory that lacks the `.ainb-sandbox-marker`
  sentinel file — protection against accidentally wiping a wrong
  path.
- `down` on a missing root is a no-op (exit 0).

These four guards are the riskiest surface in the toolchain (the
script is a pure-bash `rm -rf` machine), so they have an automated
regression test that shells out to the real script:

```bash
cd ainb-tui
cargo test -p ainb --test sandbox_script_safety_guards
```

The test (`crates/ainb-core/tests/sandbox_script_safety_guards.rs`)
pins all four behaviours: `down --root /` refused, `up --root $HOME`
refused (with the child's `HOME` pointed at a throwaway `/tmp` path so
even a broken guard can't touch a real home), `down` without the
sentinel refused (with a decoy file proving user data survives), and a
clean `up`/`down` round-trip at a `/tmp` root that is idempotent on a
second `down`. It needs only `bash` + `git` on PATH (no tmux) and runs
in CI under the `ainb-hooks` job on both Linux and macOS
(`.github/workflows/ci.yml`).

## Rust API (integration tests)

```rust
use ainb_skill_core::{build_skill_manager_sandbox, SandboxTier};

#[test]
fn my_skill_manager_test() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = build_skill_manager_sandbox(tmp.path(), SandboxTier::Minimal)
        .expect("sandbox");

    let claude_home = &layout.claude_home;  // install_root_for("claude")
    let bare = &layout.bare_remote;          // bare git URI: layout.bare_remote_uri()
    let ainb_home = &layout.ainb_home;       // manifest.yaml + lock.yaml live here

    // ... drive the code under test against the seeded paths ...
}
```

`SandboxLayout::env_vars()` returns the env-var pairs the bash
launcher writes to `env.sh` — identical contract.

## Test coverage matrix

| Journey                                    | Covered by                                                                                                |
|--------------------------------------------|-----------------------------------------------------------------------------------------------------------|
| Fixture itself builds correctly            | `crates/ainb-skill-core/tests/sandbox_fixture_smoke.rs` — 5 tests across both tiers                       |
| Refuse to seed at `$HOME`                  | `sandbox_fixture_smoke::refuses_to_seed_into_real_home`                                                   |
| Idempotent rebuild                         | `sandbox_fixture_smoke::rebuild_into_existing_root_is_idempotent`                                         |
| env_vars contract                          | `sandbox_fixture_smoke::env_vars_round_trip_paths`                                                        |
| Sync push round-trip vs bare               | `crates/ainb-skill-core/tests/sync_to_repo_tests.rs::apply_to_repo_pushes_to_real_local_bare_remote`      |
| Drift InSync/Outdated round-trip vs bare   | `crates/ainb-skill-core/tests/drift_tests_integration.rs`                                                 |
| TestBackend render of SkillsScreenData     | `crates/ainb-core/tests/tripwire_core_skill_manager_sandbox_loads.rs`                                     |
| Live tmux: press `m`, see SkillManager     | `crates/ainb-core/tests/tripwire_core_skill_manager_sandbox_e2e.rs`                                       |
| Bash `up`/`down` safety guards (rm -rf belts) | `crates/ainb-core/tests/sandbox_script_safety_guards.rs` — 4 tests against the real script               |

## Prod-binary isolation

The fixture lives behind `feature = "test-fixtures"` in
`ainb-skill-core/Cargo.toml`. Default `cargo build` skips the
module entirely; the release binary contains zero fixture seeding
code and zero seeded SKILL.md content bytes. Verify manually:

```bash
# Compile in default-feature mode (production)
cargo build --release -p ainb-skill-core

# Symbol must NOT appear in the rlib
nm target/release/libainb_skill_core.rlib 2>/dev/null \
  | grep -c build_skill_manager_sandbox
# expect 0
```

The wiring that lets `cargo test` see the fixture without an
explicit `--features` flag is a self-dev-dep in
`crates/ainb-skill-core/Cargo.toml`:

```toml
[dev-dependencies]
ainb-skill-core = { path = ".", features = ["test-fixtures"] }
```

Cargo treats this as a separate dependency edge — tests see the
fixture, production builds don't. `ainb-core` adopts the same
pattern in its own dev-deps so the SkillManager tripwires can
consume the fixture.

## Provenance coverage (today vs deferred)

The Full tier seeds three concrete provenance categories on disk:

| Category                  | Seeded path                                                                  | Tests today                                                       |
|---------------------------|------------------------------------------------------------------------------|-------------------------------------------------------------------|
| Local hand-authored skill | `.claude/skills/commit/SKILL.md`                                             | Discovered by Class-C walker; rendered in Sources/Units panels    |
| External-clone-shape skill | `.claude/skills/fireworks-tech-graph/SKILL.md`                              | Same; today resolves to `local:` (provenance matcher pending)     |
| Marketplace bundled skill | `.claude/plugins/cache/sandbox-marketplace/discord/0.1.0/skills/access/SKILL.md` | Discovered by Class-A walker; URI: `marketplace:discord@…`        |
| `external-dependencies.yaml` | `<root>/external-dependencies.yaml` (Full tier only)                       | YAML parses via serde_yaml_ng; ready for the provenance matcher  |

The provenance matcher (separate effort) will resolve the
external-clone-shape skill's URI to its `gh:` upstream by joining
the on-disk skill against the seeded `external-dependencies.yaml`.
The fixture is ready for that work today.
