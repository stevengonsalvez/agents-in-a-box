---
name: tmux-verify
description: The outer "is this TUI feature ACTUALLY done?" proof loop for the ainb terminal app. Use when the user asks to "verify the TUI is actually done", "prove this TUI feature works", "validate per tmux-verify", "vhs verify each journey", "frame-truth check the UI", "is the diff actually shown correctly", or to define the acceptance contract for a TUI goal. Orchestrates 6 gates — (1) comprehensive tmux-ui-tripwire passes, (2) a vhs recording per user journey whose frames are READ with /media-processing to assert the EXACT user-visible outcome (not "screen shows something"), (3) one recording per journey, (4) fix-loop until acceptance + tripwire green, (5) a continuous /explain-to-me explainer (tests + commits + GIFs) hosted on /here-now, (6) final HTML-solid validation with the human as the last gate. References (does not reimplement) tmux-ui-tripwire, /media-processing, /explain-to-me, /here-now.
---

# tmux-verify

## Overview

`tmux-verify` is the OUTER acceptance harness: it proves a TUI feature is *actually* done as a user would experience it. It does NOT write the automated test (that is [tmux-ui-tripwire](../tmux-ui-tripwire/SKILL.md)) and does NOT reimplement media tooling (that is `/media-processing`). It orchestrates both plus a live explainer into one repeatable contract.

Use it two ways:
- **As a contract** — a goal/plan says "validate per `/tmux-verify`" and inherits all 6 gates.
- **As a runner** — execute the gates for a feature end-to-end until green.

The non-negotiable principle: **read the frame, don't blank-check.** A passing screen is one whose pixels/text say the *correct* thing for that journey — not merely that something rendered.

## The 6 gates

```
                         ┌──────────────────────────────────────────────┐
 G1 automated  ───────▶  │ tmux-ui-tripwire: comprehensive, VT100-true,  │
 (ref skill)             │ passes in the real TUI, non-flaky ×3          │
                         └──────────────────────────────────────────────┘
 G2+G3 visual            ┌──────────────────────────────────────────────┐
 (THIS SKILL OWNS) ───▶  │ per journey:  .tape ─▶ vhs ─▶ gif+mp4         │
                         │              ─▶ ffmpeg frames ─▶ READ + assert│
                         │ one recording per journey                     │
                         └──────────────────────────────────────────────┘
 G4 fix-loop   ───────▶  acceptance(cargo test) + tripwire green; unbounded
                         until green or a genuine hard blocker (then log + go on)
 G5 explainer  ───────▶  /explain-to-me ─▶ /here-now  (stable slug, republish
                         each phase: progress · test/clippy/tripwire output ·
                         commit log · embedded per-journey GIFs)
 G6 final      ───────▶  HTML solid (fetch URL: links resolve, GIFs load,
                         no empty sections) ─▶ HUMAN validates → done
```

## Gate-by-gate playbook

### G1 — Automated gate (defer to tmux-ui-tripwire)
Write/repair the feature's `crates/ainb-core/tests/tripwire_*.rs` per [tmux-ui-tripwire](../tmux-ui-tripwire/SKILL.md). Bar to clear:
- **Comprehensive**: asserts VT100 truth (rendered cells, colors, bg attrs, exact counter text) — NOT substring-OR on chrome strings that pass while the feature is broken.
- **Green + non-flaky**: `cargo test -p ainb-core --test tripwire_<feature>` passes 3 runs in a row.
- Respect the documented traps: AMFI SIGKILL (exit 137), first-run-wizard key-swallow, EnvFilter crate-name drift, weak assertions. See that skill's `references/`.
- **tmux safety**: only ever `tmux kill-session -t <exact-name>`. Never `kill-server`, `pkill`, `killall`, or any wildcard.

### G2 + G3 — Visual gate, one recording per journey (this skill owns it)
For EACH journey (define them first — see `references/journeys.md`):
1. **Author the tape** from `scripts/journey.tape.tmpl` — isolated `HOME` (skips the wizard), window size, the exact key sequence, `Screenshot` at each settle point, `Output <journey>.gif` + `.mp4`.
2. **Record + extract + assert** with `scripts/verify-journey.sh` — runs `vhs`, pulls frames via ffmpeg, OCRs them (tesseract, best-effort), and emits the frame paths + a checklist of expected content.
3. **READ the frames** (open the PNGs) and assert the EXACT outcome for that journey — the right diff content, the right file list, word-emphasis on the changed token, the active tab actually changed, Esc landed on the main screen. Recipe + anti-patterns: `references/frame-truth.md`.
4. Keep the `<journey>.gif` (for the explainer) and `<journey>.mp4`.

A journey is unverified until a human-or-agent has *read its frames* and confirmed the content. "Non-blank == pass" is a failure of this gate.

### G4 — Fix loop
If any G1 assertion, any G2 frame-check, or any G5/G6 asset is wrong: diagnose → fix → re-run. **Unbounded** until acceptance tests + tripwire are green and every journey frame-checks clean, or a genuine hard blocker (then log the wall and continue everything parallelizable).

### G5 — Continuous explainer (defer publishing to /explain-to-me + /here-now)
Maintain ONE explainer at a **stable slug**, re-published after every phase/commit. Each republish must contain: current phase + progress, the latest `cargo test` / `clippy` / tripwire output, the commit log so far, and the embedded per-journey GIFs. Build it with `/explain-to-me` (status-report / PR-review template) and publish with `/here-now`; `scripts/publish-explainer.sh` checks the dir then prints the exact `/here-now` invocation. Cadence + required sections: `references/explainer.md`.

### G6 — Final HTML-solid validation
Before declaring done, FETCH the live URL and confirm: every link resolves, every GIF loads, no section is empty/placeholder, the test output shown is the latest green run. Then hand the human the live URL — **the human is the final acceptance gate.** Do not merge on their behalf unless the goal says so.

## Quick start

```bash
SK=ainb-tui/.claude/skills/tmux-verify
AINB=$(find ainb-tui/target -name ainb -type f -perm +111 | head -1)   # or: cargo build -p ainb-core

# 1. one journey: render tape, record, extract+OCR frames, print checklist
bash $SK/scripts/verify-journey.sh \
  --name open-g --bin "$AINB" \
  --keys 'Type "G"  Sleep 2s  Screenshot "{OUT}/open-g.png"' \
  --out /tmp/ainb-journeys --expect "Code Review" --expect "skill.rs"
# → then READ /tmp/ainb-journeys/frames/open-g*.png and assert content

# 2. publish/refresh the explainer (after building it with /explain-to-me)
bash $SK/scripts/publish-explainer.sh --dir /tmp/ainb-explainer --slug ainb-warp-diff
```

## Done checklist (all true)
- [ ] G1 tripwire comprehensive + green + non-flaky ×3
- [ ] G2/G3 every journey has its own gif+mp4 AND its frames were read + asserted (content correct)
- [ ] G4 `cargo test -p ainb-core` + `clippy -D warnings` green
- [ ] G5 explainer live on /here-now at the stable slug, current
- [ ] G6 HTML fetched + solid; live URL handed to the human

## References
- `references/journeys.md` — define a journey; the worked ainb diff-review journey set
- `references/frame-truth.md` — vhs → frames → READ + assert; anti-patterns
- `references/explainer.md` — the continuous /explain-to-me → /here-now cadence
- Sibling: [tmux-ui-tripwire](../tmux-ui-tripwire/SKILL.md) (G1). Tooling: `/media-processing` (vhs/ffmpeg/imagemagick), `/explain-to-me`, `/here-now`.
