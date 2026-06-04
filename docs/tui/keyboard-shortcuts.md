---
title: "Keyboard shortcuts"
---

Keys verified against the in-app help overlay (`?`) and the event handlers in `crates/ainb-core/src/app/events.rs`.

## Home screen navigation

| Key | Action |
|-----|--------|
| `s` | Sessions |
| `a` | Agents (agent selection) |
| `R` | Recovery |
| `i` | Stats (usage analytics) |
| `k` | Skills |
| `I` (or `Shift`+`i`) | Inbox (notifications) |
| `c` | Catalog |
| `C` | Config |
| `v` | Changelog |
| `n` | New session |
| `Tab` | Toggle focus (sidebar ↔ content) |
| `?` | Toggle help |
| `q` | Quit |

## Navigation (lists)

| Key | Action |
|-----|--------|
| `j` / `↓` | Move down |
| `k` / `↑` | Move up |
| `h` / `←` | Previous workspace |
| `l` / `→` | Next workspace |
| `g` | Go to top |
| `G` | Go to bottom |

## Session actions

| Key | Action |
|-----|--------|
| `n` | New session (local or remote) |
| `a` | Attach to session |
| `e` | Restart stopped session |
| `r` | Re-authenticate credentials |
| `d` | Delete session |
| `x` | Cleanup orphaned containers |
| `f` | Refresh workspaces |
| `Space` | Toggle multi-select |
| `Shift`+`D` | Delete all selected sessions |

## Recovery screen

| Key | Action |
|-----|--------|
| `Space` | Toggle select |
| `Shift`+`D` | Delete selected |

## Git actions

| Key | Action |
|-----|--------|
| `g` | Open the [Code Review](code-review.md) diff for the selected session |
| `p` | Commit & push |

### Within the Code Review diff

| Key | Action |
|-----|--------|
| `↑` / `↓` | Move across the file tree (file → scroll body) |
| `j` / `k` | Scroll the diff body |
| `n` / `N` | Next / previous hunk (`Hunk x/y` counter) |
| `Space` / `Enter` | Toggle a folder, or collapse/expand a file's diff block |
| `e` / `E` | Expand / collapse all folders |
| `z` | Reveal more context at the nearest gap |
| `[` / `]` | Previous / next file |
| Mouse | Wheel scrolls the diff; click a tree row to select/toggle |
| `Tab` | Cycle Review → Commits → Markdown |
| `Esc` / `q` | Back |

> The same surface is available standalone: `ainb diff-review [path]`.

## Usage / Stats screen

| Key | Action |
|-----|--------|
| `Tab` | Switch usage tab (Daily/Weekly/Project/Burndown/Optimize) |
| `1`–`5` | Today / Week / 30 days / Month / All |
| `p` | Cycle provider (All / Claude / Codex) |
| `/` | Add include filter |
| `x` | Add exclude filter |
| `d` | Enter custom `YYYY-MM-DD YYYY-MM-DD` range |
| `c` | Clear filters |
| `r` | Reload |

## General

| Key | Action |
|-----|--------|
| `?` | Toggle help overlay |
| `q` / `Esc` | Back / quit |
| `Ctrl`+`C` | Force quit |

## See also

- [Overview](overview.md)
- [CLI reference](cli.md)
- [Docs hub](../README.md)
