# Defining journeys

A **journey** = one user action sequence + the EXACT user-visible outcome it must produce. It is the unit the visual gate (G2/G3) proves. Vague outcomes ("the diff renders") are unverifiable; pin the concrete content.

## Anatomy

| Field | Meaning | Example |
|-------|---------|---------|
| `name` | kebab id, becomes the tape/gif/mp4 basename | `open-g` |
| `keys` | vhs key sequence (Type/Enter/Sleep/Screenshot) | `Type "G"  Sleep 2s  Screenshot {OUT}/open-g.png` |
| `expect` | concrete content that MUST appear in the settled frame (1+) | `Code Review`, `+57`, `skill.rs` |
| `assert` | the human/agent judgement to make from the frame | "sidebar lists changed files with correct +N/-M; first file's diff matches the seeded edit" |

`expect` patterns are for the OCR pre-filter (tesseract). `assert` is the real bar — read the frame and judge it. A journey passes only when the `assert` is true, not merely when `expect` strings are found.

## Settle, don't snap
Always `Sleep` after a keypress BEFORE `Screenshot` so the frame captures the *settled* UI, not a mid-paint state. ainb cold paint can take a couple seconds; the burndown/heavy panels longer. Under-sleeping silently captures a loading screen — the classic false pass. If unsure, sleep longer and capture the last frame.

## Worked set — Warp-style diff review (the feature under build)

Launch context: isolated `HOME` seeded to skip the first-run wizard; a temp git repo with a known modified file (with an intra-line word change), an untracked file, and a deleted file; a session selected so `G` opens its review.

| name | keys (after launch) | expect | assert (read the frame) |
|------|---------------------|--------|--------------------------|
| `open-g` | `Type "G"  Sleep 2s` | `Code Review`, `skill.rs` | Review surface up: left sidebar lists the changed files with correct `+N`(green)/`-M`(red); first file's diff body shows the seeded change correctly. |
| `syntax-wordemph` | `Type "G"  Sleep 2s  Down*… ` to a modified line | (token text) | Dracula token colors present AND the changed substring is visibly BRIGHTER than the rest of the tinted row (word-emphasis), not a flat tint. |
| `collapse` | `Type "G"  Sleep 1s  Space  Sleep 1s` | (file path, `›`) | The focused file's chevron flips to `›` and its code rows disappear; the header stays. Toggling back restores rows. |
| `expand-context` | `Type "G"  Sleep 1s  Type "z"  Sleep 1s` | `expand` | A `↕ expand N lines` row reveals previously hidden context lines; the hidden count drops. |
| `hunk-jump` | `Type "G"  Sleep 1s  Type "n"  Sleep 1s` | `Hunk 2/` | Selection moves to the next hunk and the `Hunk x/y` counter advances to the exact expected value. |
| `file-nav` | `Type "G"  Sleep 1s  Type "]"  Sleep 1s` | (2nd file path) | Body scrolls to the next file's header and the sidebar selection follows. |
| `tab-traverse` | `Type "G"  Sleep 1s  Tab  Sleep 1s` | `Commits` (or Markdown) | The active tab actually changes Review → Commits → Markdown; the surface content swaps accordingly. |
| `esc-exit` | `Type "G"  Sleep 1s  Escape  Sleep 1s` | (main/session screen marker) | Esc returns to the previous/main session screen — the Review surface is gone and the session list is shown. |

Each row = one tape, one `vhs` run, one gif + one mp4, one frame-read. Eight journeys here → eight recordings.

## Deriving journeys for any feature
1. List every key the feature binds and every state it can show.
2. For each, write the action and the single most specific observable change.
3. Add the universal pair every TUI surface needs: **traverse** (Tab/arrows move where they should) and **exit** (Esc lands on the right screen).
