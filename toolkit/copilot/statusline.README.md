# Copilot CLI rich statusline

A copilot-native port of the Claude Code rich statusline
(`toolkit/claude-code-4.5/statusline.sh`). GitHub Copilot CLI grew a
Claude-shaped command-driven status line (experimental, ~May 2026, copilot
`v1.0.62`+) using the **same `statusLine.command` shape as Claude** — it spawns
the command after each response and pipes a session JSON object on stdin, then
renders the first line of stdout.

This port keeps the visual structure and quality of the Claude statusline but
reads Copilot's stdin field paths and gracefully omits the signals Copilot does
not provide.

```
┌──────┐   ┌─────────────────┐   ┌──────────────────┐   ┌────────┐   ┌────────┐
│ cwd  │──▶│ git branch ↑↓ ± │──▶│ model · ctx bar  │──▶│ duration│──▶│ remote │
└──────┘   └─────────────────┘   └──────────────────┘   └────────┘   └────────┘
   blue          cyan/orange            purple              green        pink
```

## Files

| File | Purpose |
|---|---|
| `statusline.sh` | The copilot-native statusline (single powerline line). |
| `test_statusline.sh` | Feeds synthetic copilot JSON, asserts the rendered line. |

## Wiring

Copilot reads `~/.copilot/settings.json`. The statusline is **experimental** and
gated behind a feature flag — without `feature_flags.enabled: ["STATUS_LINE"]`
(or running `/experimental` in a session), the `statusLine` block is ignored.

1. Copy the script to your Copilot config dir:

   ```sh
   cp toolkit/copilot/statusline.sh ~/.copilot/statusline.sh
   chmod +x ~/.copilot/statusline.sh
   ```

2. Add this to `~/.copilot/settings.json` (merge into your existing file — do not
   blow away other keys):

   ```json
   {
     "feature_flags": {
       "enabled": ["STATUS_LINE"]
     },
     "statusLine": {
       "type": "command",
       "command": "~/.copilot/statusline.sh",
       "padding": 0
     }
   }
   ```

   Identical `statusLine` shape to Claude's `settings.json` — only the
   `feature_flags` gate is Copilot-specific.

3. Restart Copilot (or toggle with `/experimental` in-session).

A Nerd Font is recommended for the powerline triangle separators (iTerm2:
Profile → Text → "Use built-in Powerline glyphs", or install MesloLGS NF /
JetBrainsMono NF / Hack NF). Without one, the `` / `` glyphs show as
tofu boxes but the line still renders.

## What it renders (and what it drops)

### Rendered (remapped to Copilot stdin paths)

| Segment | Copilot field(s) | Notes |
|---|---|---|
| model | `.model.display_name` → `.model.id` | Claude tier/version shortener still applies to Claude ids; other vendors (GPT-4o, Gemini) fall through to the display name. |
| cwd | `.cwd` | `~`-collapsed, adaptive-shortened by terminal width. |
| git | (none — keyed off `$CWD`) | branch · ahead/behind · staged+unstaged (`±N`) · untracked (`?N`). |
| context bar | `.context_window.current_context_used_percentage`, fallback ratio of `.context_window.current_context_tokens` / `.context_window.context_window_size` | green <50, amber 50–80, red ≥80. |
| duration | `.cost.total_duration_ms` | rendered `Xh Ym` / `Xm Ys` / `Ys` — **Copilot's stand-in for Claude's USD cost**. |
| remote | `.remote.indicator`, fallback `.remote.connected` | omitted when not connected / absent. |

### Dropped Claude signals (no Copilot equivalent)

| Claude signal | Claude field | Why dropped |
|---|---|---|
| USD cost | `.cost.total_cost_usd` | Copilot exposes `total_duration_ms`, not a dollar cost → rendered as a **duration** segment instead. The test asserts the line contains no `$`. |
| 5h / weekly rate-limit bars | `.rate_limits.five_hour.*`, `.rate_limits.seven_day.*` | Copilot has no OAuth-grade budget-window block — nothing to source. |
| session-health `N/50` badge | `.transcript_path` + user-turn count | Copilot provides no transcript path nor message count. |
| reasoning effort / fast-mode | `.effort.level`, `.fast_mode` | Not in Copilot's confirmed field set. Read **defensively** — renders if the field ever appears, otherwise silently omitted (see "Needs live confirmation"). |
| beads / reflect-error / ainb side-channel / reflect-timeline dashboard | — | Claude-ecosystem extras; out of port scope and/or incompatible with single-line output. |

Omission rule: a segment with no data is never printed (no empty/zero segments).

## Defensive design (never blocks Copilot's render loop)

Mirrors the Claude script:

- `jq` reads tolerate missing fields (`// empty`); malformed/empty stdin still
  emits one safe line (cwd→`pwd`, model→`unknown`, ctx→0%).
- Git calls are `timeout 0.5`-guarded (`timeout`→`gtimeout`→unguarded fallback,
  so a stock mac without coreutils on PATH still works).
- No 10s `/tmp` cache (unlike the Claude original): every signal Claude cached
  (`bd` / reflect errors / `ccusage`) was dropped in this port, so the sole
  remaining slow signal is git — which is timeout-guarded, not cached.
- Output is exactly one newline-terminated line.

## Compatibility

- bash 3.2 (macOS system bash) and bash 5.x (linux / homebrew) — verified by the
  test under both. No `declare -A` or other 3.2-breaking bashisms.

## Test

```sh
bash toolkit/copilot/test_statusline.sh        # any bash
/bin/bash toolkit/copilot/test_statusline.sh   # force macOS bash 3.2
```

The live `copilot` binary is **not** invoked (org-policy-blocked); the test
feeds a synthetic copilot-shaped JSON and asserts: exactly one line · contains
the model name · contains a context-% indicator · contains the git branch ·
contains **no** `$` USD cost segment.

## Needs live confirmation (once the `copilot` binary is unblocked)

These come from docs/research, not the blocked binary — verify against the
installed CLI:

- Exact field path `context_window.current_context_used_percentage` (and whether
  it is a 0–100 number or 0–1 fraction; the script treats it as 0–100).
- Whether Copilot exposes any reasoning-effort field (the script reads
  `.effort.level` / `.fast_mode` defensively; both are unconfirmed for Copilot).
- `remote.indicator` value shape (string assumed) and `remote.connected` bool.
- The feature-flag name is exactly `STATUS_LINE` at your installed Copilot
  version.
- That `total_duration_ms` is whole milliseconds (the script integer-divides by
  1000).
