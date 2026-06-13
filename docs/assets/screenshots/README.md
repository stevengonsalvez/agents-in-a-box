---
title: "Screenshots"
---

# Screenshots

Reproducible TUI screenshots for the project docs. Generated headlessly by [vhs](https://github.com/charmbracelet/vhs) — no real terminal needed, no on-screen pop-ups, output is byte-deterministic per binary build + theme + `$HOME` state.

## Files

| File | Tape | What it shows |
|---|---|---|
| `home.png` | `home.tape` | ainb home screen with sidebar + welcome panel. Plugins disabled (`AINB_DISABLE_PLUGINS=1`). |
| `burndown.png` | `burndown.tape` | Full burndown dashboard post-Tab (Daily Activity, By Project, By Model, Top Sessions, Budget, Optimization). Plugins enabled. |

Both tapes run against the **contributor's real `$HOME`** so the screenshots reflect actual workspace state and Claude usage. The committed PNGs are point-in-time snapshots of whoever last regenerated them — they're not byte-identical across machines.

### Code Review (animated)

| File | What it shows |
|---|---|
| `code-review-diff.gif` | The Code Review surface — file sidebar, per-file diff blocks, syntax colours, row tints, gutter. |
| `code-review-expand.gif` | Word-level emphasis (only changed tokens are brighter) + `z` revealing hidden context. |
| `code-review-collapse.gif` | `Space` collapsing a file's diff block to its header and back. |
| `code-review-hunk.gif` | `n` / `N` jumping hunks with the `Hunk x/y` counter. |

These are recorded with vhs driving `ainb diff-review <repo>` on a throwaway demo repo (a few modified files with intra-line word changes, an untracked file, a deleted file), then optimised with `gifsicle -O3 --lossy=60 --colors 200 --resize-width 1100`. They don't need a seeded `$HOME` — `diff-review` skips onboarding and reads the target repo directly.

### Shared MCP pool (animated)

| File | Tape | What it shows |
|---|---|---|
| `mcp-pool.gif` | `mcp-pool.tape` | Two sessions attach to ONE real context7 (npx) server through the pool: `ainb mcp import`, daemon start, both sessions get context7 tools, `ainb mcp status` shows `clients: 2` against one `child_pid`, and the process-group proof confirms a single shared server. |

`mcp-pool.tape` drives [`scripts/mcp-pool-demo.sh`](../../../scripts/mcp-pool-demo.sh), which is fully self-contained: it uses its own isolated `$HOME` (so it never touches your real pool sockets), spins up the daemon, attaches two `ainb mcp proxy` shims to a real context7 server, and prints the proof. No Claude auth or TUI involved — deterministic and re-runnable.

## Regenerating

From `ainb-tui/`:

```bash
just stage-plugins                                  # rebuild + re-sign plugin binaries (needed for burndown)
vhs ../docs/assets/screenshots/home.tape            # → home.png (~80s wall-clock)
vhs ../docs/assets/screenshots/burndown.tape        # → burndown.png (~140s wall-clock)
cargo build --release                               # mcp-pool.tape drives the release binary
vhs ../docs/assets/screenshots/mcp-pool.tape        # → mcp-pool.gif (~55s wall-clock, real context7 npx)
```

Each tape drives the real `target/debug/ainb` binary inside vhs's virtual pty, sends scripted keystrokes, snaps a PNG, and exits cleanly.

## Timing budgets

vhs `Sleep` directives are wall-clock — under-sleeping captures a loading state silently.

| Phase | Why slow on real $HOME | Sleep budget |
|---|---|---|
| ainb startup → home render | workspace scan over `~/.agents-in-a-box/{repos,sessions.json}` + auth probe | 75s |
| burndown ingest (after `i`) | session-reader plugin walks all of `~/.claude/projects` (2.2 GB / 179 dirs in one author's case) | 60s |

If your `~/.claude/projects` is much larger, bump `Sleep` accordingly and re-record.

## How it stays portable

- **`Catppuccin Mocha` theme** — pinned in the tape; output is independent of your terminal style.
- **Fixed width × height** — pinned at 1600×900 px so layout is identical across machines.
- **Real `$HOME`** — no seeding, no symlinks. The trade-off: PNGs reflect *your* usage, not a synthetic fixture. Approved for public docs as of this branch.

## Adding a new screenshot

1. Copy `burndown.tape` (or `home.tape`) as a starting template.
2. Adjust the keystroke sequence (`Type`, `Tab`, `Sleep`, etc. — see [vhs docs](https://github.com/charmbracelet/vhs#vhs-command-reference)).
3. Point `Output` at a new path under `docs/assets/screenshots/`.
4. Confirm the sleep margin covers the slowest cold-scan path on a populated `$HOME`.
5. Commit the tape alongside the PNG so the next regen produces a comparable image.

## Hard rules

- **Never edit `~/.tmux.conf`** or any user-global config from these scripts.
- **Never run `tmux kill-server`** — vhs uses its own pty, not tmux.
- **Don't seed `~/.claude/projects`** — the tapes read it directly; keep the real path off CI runners.
