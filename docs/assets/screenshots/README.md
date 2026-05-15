# Screenshots

Reproducible TUI screenshots for the project docs. Generated headlessly by [vhs](https://github.com/charmbracelet/vhs) — no real terminal needed, no on-screen pop-ups, output is byte-deterministic per binary build + theme.

## Files

| File | Tape | What it shows |
|---|---|---|
| `home.png` | `home.tape` | Cold ainb home screen with sidebar + welcome panel. Plugins disabled (`AINB_DISABLE_PLUGINS=1`). |
| `burndown.png` | `burndown.tape` | Full burndown dashboard post-Tab (Daily Activity, By Project, By Model, Live Tools, Budget). Plugins enabled — real subprocess pipeline against seeded fixture. |

## Regenerating

From `ainb-tui/`:

```bash
just stage-plugins                                  # rebuild + re-sign plugin binaries
vhs ../docs/assets/screenshots/home.tape            # → home.png
vhs ../docs/assets/screenshots/burndown.tape        # → burndown.png
```

Each tape drives the real `target/debug/ainb` binary inside vhs's virtual pty, sends scripted keystrokes, snaps a PNG, and exits cleanly.

## How they stay reproducible

- **Isolated `$HOME`** — `seed-and-run.sh` seeds a fresh dir under `/tmp/ainb-screenshot-home` with an onboarding-complete marker and a fixed-content Claude session JSONL. Your real `~/.claude/projects` is never read.
- **Onboarding stamp** — without this the wizard takes over the home screen on first run.
- **Synthetic session data** — four hand-rolled assistant messages across three sessions produce a small but populated dataset (~117K tokens, $0.52 cost) so every burndown panel has rows to render.
- **`Catppuccin Mocha` theme** — pinned in the tape; output is independent of your terminal style.

## Adding a new screenshot

1. Copy `burndown.tape` (or `home.tape`) as a starting template.
2. Adjust the keystroke sequence (`Type`, `Tab`, `Sleep`, etc. — see [vhs docs](https://github.com/charmbracelet/vhs#vhs-command-reference)).
3. Point `Output` at a new path under `docs/assets/screenshots/`.
4. If the new screenshot needs additional fixture data, extend `seed-and-run.sh` — keep edits backward-compatible so existing tapes don't drift.
5. Commit the tape alongside the PNG so the next regen produces the same image.

## Hard rules

- **Never edit `~/.tmux.conf`** or any user-global config from these scripts.
- **Never run `tmux kill-server`** — vhs uses its own pty, not tmux.
- **Don't paste your real `~/.claude/projects` into a tape** — the seed script is the only data path; keep it deterministic.
