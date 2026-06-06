---
name: tmux-ui-tripwire
description: Write or debug tmux-driven end-to-end TUI tests ("tripwires") for the ainb terminal app. Use when the user asks to "write a tmux test", "add a tripwire", "verify the TUI in tmux", "test feature X by pressing key Y", "validate plugin Z renders", or when editing any file under `crates/ainb-core/tests/tripwire_*.rs`. Codifies the silent traps we hit during Phase 7 plugin testing — macOS AMFI SIGKILL of staged binaries (exit 137, no stderr), first-run wizard intercepting keystrokes, EnvFilter crate-name drift hiding logs, and substring-OR assertions that pass while the feature is broken. Use proactively before writing any new `tripwire_*.rs` test, and reference whenever an existing tripwire fails for non-obvious reasons.
---

# tmux-ui-tripwire

## Overview

Tripwire tests drive the real `ainb` TUI binary in a detached `tmux` session, send keystrokes, capture the pane, and assert on rendered output. They are the ONLY tests that catch user-visible regressions the unit tests miss (e.g. plugin pipeline broken but `cargo test` green).

## When to use this skill

- Writing a new `tripwire_*.rs` test
- An existing tripwire fails and the cause isn't obvious from `cargo test` output
- Tightening a tripwire whose assertions look weak (substring-OR on chrome strings)
- Diagnosing why a plugin "spawns but doesn't render"

## Quick start — minimal viable tripwire

```rust
// crates/ainb-core/tests/tripwire_<feature>.rs
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

// 1. Skip gracefully if env can't support the test.
fn tmux_available() -> bool { /* see references/helpers.md */ }
fn ainb_bin() -> PathBuf { PathBuf::from(env!("CARGO_BIN_EXE_ainb")) }

#[test]
fn feature_renders_after_pressing_x() {
    if !tmux_available() { eprintln!("SKIP: tmux"); return; }
    // 2. Isolated HOME — seed onboarding.toml to skip wizard.
    let home = tempfile::tempdir().unwrap();
    seed_isolated_home(home.path());

    // 3. Launch ainb tui in detached tmux session.
    let session = format!("tripwire-{}", std::process::id());
    Command::new("tmux").args(["new-session","-d","-s",&session,"-x","180","-y","50"]).status().unwrap();
    let cmd = format!("HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home.path().display(), ainb_bin().display());
    Command::new("tmux").args(["send-keys","-t",&session,&cmd,"Enter"]).status().unwrap();

    // 4. Wait for HomeScreen — poll, don't bare-sleep.
    let pre = poll_capture(&session, Instant::now() + Duration::from_secs(45),
        |c| c.contains("Stats") && c.contains("[i]")).expect("HomeScreen never rendered");

    // 5. Pre-press negative assertion — confirm we're not already on target screen.
    assert!(!pre.contains("Usage Analytics"));

    // 6. Send key WITHOUT Enter (single-char nav).
    Command::new("tmux").args(["send-keys","-t",&session,"x"]).status().unwrap();

    // 7. Post-press: positive marker AND negative placeholder.
    let post = poll_capture(&session, Instant::now() + Duration::from_secs(30),
        |c| c.contains("Expected Chrome") && !c.contains("Loading placeholder"))
        .unwrap_or_else(|| capture_pane(&session));
    Command::new("tmux").args(["kill-session","-t",&session]).status().ok();

    assert!(post.contains("Expected Chrome"), "feature chrome never rendered:\n{post}");
    assert!(!post.contains("Loading placeholder"), "stuck on placeholder:\n{post}");
}
```

Full copy-paste helper functions in `references/helpers.md`.

## Required pre-work

| Step | Command | Why |
|---|---|---|
| Stage plugins (if test exercises plugin path) | `just stage-plugins` | Plugins live at `dist/plugins/<id>/<id>` and are re-signed for macOS AMFI |
| Build ainb | `cargo build -p ainb` | Test resolves binary via `env!("CARGO_BIN_EXE_ainb")` |

## Hard rules (violating these costs hours)

1. **NEVER `tmux kill-server`, `pkill tmux`, `killall tmux`, or wildcard kill.** Only `tmux kill-session -t <exact-name>`. Other agents/dev sessions live in tmux.
2. **NEVER assert with substring-OR on chrome strings** (`"ainb"`, `"session"`, `"Container"`). They appear in sidebar regardless of whether the feature under test rendered. The original 7f.3 passed for 4+ commits while burndown was broken. Always pair a POSITIVE marker with a NEGATIVE placeholder check.
3. **NEVER append `Enter` to a single-character keystroke.** `tmux send-keys -t S "i"` for nav keys. `Enter` as a SEPARATE arg is only for committing a shell command line.
4. **NEVER use bare `sleep` before capture.** TUI render rate is 4–30 Hz. Use `poll_capture(deadline, predicate)` with 500 ms internal sleep.
5. **NEVER grep `Cargo.toml` for the version in a shell wrapper.** Workspace crates have `version.workspace = true` literally — grepping returns that string, breaks the wizard-skip seed. Use `env!("CARGO_PKG_VERSION")` in Rust or read `[workspace.package].version` from the root `Cargo.toml`.
6. **ALWAYS pair forward + return navigation.** When a tripwire asserts "press X → screen Y rendered", add a sibling test (or trailing assertion) that "press Esc → previous screen returned". Forward-only tests pass while return paths silently break — exactly how the Esc-from-burndown swallow shipped past 4 existing tripwires in PR #128. The `plugin/handle_key` wire is a one-way notification: it cannot signal "ignored, fall through", so plugins can swallow keys with no visible failure unless you test the round-trip end-to-end. See `tripwire_burndown_esc_returns_home.rs` for the canonical return-path shape.

## Two-tmux pattern — KEEP BOTH PATHS

Keep BOTH a plugin-path test AND a non-plugin-path test:

| Test | Key | Env | Purpose |
|---|---|---|---|
| `tripwire_real_data_in_tui` | `i` | normal (plugins active) | Verifies plugin pipeline end-to-end |
| `tripwire_sessions_screen` | `s` | `AINB_DISABLE_PLUGINS=1` | Verifies host's own code path isn't broken by plugin refactors |

A refactor that breaks one shouldn't silently pass the other. If you add a new screen test, also add a regression test for the opposite path.

## Workflow when a tripwire fails

1. **Run with `--nocapture`** — `cargo test -p ainb --test tripwire_X -- --nocapture` to see SKIP messages and panic output.
2. **Read the captured pane in the panic message** — it shows what the test actually saw. Often the cause is staring at you (wizard, loading state, wrong screen).
3. **Check the JSONL log** — `~/.agents-in-a-box/logs/agents-in-a-box-*.jsonl`. Key markers:
   - `"registered plugin"` × N — discovery worked
   - `"eager spawn failed: Broken pipe"` + `"plugin exited / pipe closed"` — likely macOS AMFI silent kill (see `references/amfi.md`)
   - `"inbound request"` / `"host->plugin response"` — wire dialog active
   - `plugin = "<name>"` entries — `host.log()` from plugins
4. **Standalone-probe the plugin** if Bug-2-like symptoms:
   ```
   ./dist/plugins/<id>/<id> </dev/null; echo $?
   ```
   Exit 137 in <1ms = AMFI kill. Fix: `just stage-plugins` (re-signs).

## When to consult the references

| Question | File |
|---|---|
| What's the exact code for `poll_capture`, `seed_isolated_home`, etc.? | `references/helpers.md` |
| Why does my plugin exit 137 with no stderr? | `references/amfi.md` |
| How do I seed synthetic Claude data for `$N.NN` assertions? | `references/seed-data.md` |
| Full gotcha checklist (10 silent traps) | `references/gotchas.md` |
| Existing tripwires to copy patterns from | `ls crates/ainb-core/tests/tripwire_*.rs` — each test is self-documenting via its filename + comments |

## Source files

- `crates/ainb-core/tests/tripwire_*.rs` — every tripwire on disk; read them directly, no manifest to keep in sync
- `scripts/build-plugins.sh` — `just stage-plugins` implementation (codesign re-sign on macOS)
- `crates/ainb-core/src/main.rs` `setup_logging()` — default `EnvFilter` widened to include plugin crates
