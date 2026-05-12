# existing-tests.md — current tripwires to copy patterns from

## tripwire_real_data_in_tui (plugin path)

**File:** `crates/ainb-core/tests/tripwire_real_data_in_tui.rs`

**What it proves:** Burndown analytics screen renders with real session
data after the user presses `i` from HomeScreen. Catches Phase 7 plugin
pipeline regressions (eager-spawn, AMFI re-sign, snapshot publish/subscribe).

**Setup:**
- Isolated HOME via `tempfile::tempdir()`
- Pre-seeded `onboarding.toml` (wizard skip)
- Pre-seeded synthetic claude jsonl (one `assistant` line, 1000+500 tokens,
  `claude-sonnet-4-5`) — produces ~$0.0105 cost so `$0.` matches
- Plugin staging probe: skip if `dist/plugins/{burndown,session-reader}` absent
- Launches with `AINB_PLUGIN_ROOT=<staged>` (NOT `AINB_DISABLE_PLUGINS=1`)

**Keystroke:** `"i"` (single char, no Enter)

**Assertions:**
- Pre-press: contains `"Stats"` AND `"[i]"`, does NOT contain `"Usage Analytics"`
- Post-press (must satisfy ALL):
  - contains `"Usage Analytics"` (chrome present)
  - does NOT contain `"Waiting for session-reader plugin"` (placeholder gone)
  - contains `"Total Calls"` OR `"Total Cost"` OR `"$0."` OR `"$N."` (real data)

**Runtime:** ~6s on M1

**Skip conditions:** tmux not available; plugins not staged

## tripwire_sessions_screen (non-plugin path)

**File:** `crates/ainb-core/tests/tripwire_sessions_screen.rs`

**What it proves:** Session List screen (host-owned, no plugin involved)
renders when the user presses `s`. Sister test to the plugin path —
catches regressions where a plugin refactor breaks the host's own UI code.

**Setup:**
- Isolated HOME via `tempfile::tempdir()`
- Pre-seeded `onboarding.toml`
- NO data seeded (empty-state render is fine — accepts "Select a session
  to view details" OR a session list, since fresh tempdir has no sessions)
- Launches with `AINB_DISABLE_PLUGINS=1` (faster, no plugin staging needed)

**Keystroke:** `"s"`

**Assertions:**
- Pre-press: contains `"Sessions"` AND `"[s]"`, does NOT contain `"Session List"` chrome
- Post-press:
  - contains `"Session Details"` OR `"Select a session to view details"`
    OR (`"attach"` AND `"restart"` AND `"cleanup"`)
  - does NOT contain `"Usage Analytics"` (didn't drift to wrong screen)

**Runtime:** ~6s on M1

**Skip conditions:** tmux not available (no plugin gate — pure host path)

## tripwire_reproduced (non-tmux, supporting tests)

**File:** `crates/ainb-core/tests/tripwire_reproduced.rs`

**What it proves:** Pre-Phase-7 wasmi tripwires reproduced through the
new subprocess runtime (action invoke, render, snapshot publish, CLI
lint). Pure in-process tests — no tmux involved.

**When to mirror:** When you need to assert on plugin runtime behaviour
WITHOUT a UI — these tests run faster and are easier to debug than tmux
tripwires. Use the tmux path only when the user-visible TUI is what
matters.

## Anti-pattern: the original 7f.3 (pre-tightening)

The original assertion was substring-OR on chrome strings:

```rust
// DON'T DO THIS — copied here as a cautionary tale.
assert!(
    capture.contains("ainb")
    || capture.contains("Container")
    || capture.contains("session")
    || capture.contains("Workspace")
);
```

All four substrings appear in the sidebar regardless of whether the
feature under test rendered. Test was green for 4+ commits while
burndown was broken. The fix was the tightened assertion in the current
`tripwire_real_data_in_tui` — pair POSITIVE markers with NEGATIVE
placeholder checks.

## How to add a new tripwire

1. Decide path: plugin (`AINB_PLUGIN_ROOT=...`) or non-plugin (`AINB_DISABLE_PLUGINS=1`)
2. Copy helper functions from `helpers.md` into the new test file
3. Decide keystroke + expected screen markers
4. Pick a UNIQUE marker pair (positive screen marker + negative placeholder/wrong-screen marker)
5. If asserting real-data, seed via `seed-data.md`
6. Run: `just stage-plugins && cargo test -p ainb --test tripwire_<name> -- --nocapture`
7. Iterate on assertion strictness — first run with weak assertions to
   discover what the pane actually contains, then tighten

## Naming convention

`tripwire_<feature>_<verb>.rs` in `crates/ainb-core/tests/`. Test function
inside named `<feature>_renders_after_pressing_<key>` or similar declarative
form. One test per file (cargo runs them in parallel processes — different
tmux sessions don't collide).
