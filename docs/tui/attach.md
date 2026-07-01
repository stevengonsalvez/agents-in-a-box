---
title: "Attaching to tmux sessions"
description: "The two attach modes in ainb-tui — 'a' suspends the TUI into a full-screen tmux client, Shift+A turns the preview pane into a live embedded tmux client in-place (INTERACTIVE badge, Ctrl+Q to release). The tmux session always survives detach."
---

Every row in the session list — agent sessions, workspace shells, and foreign sessions under **Other tmux** — is backed by a real tmux session, and there are two ways to step inside it. Press **`a`** for a **full-screen attach**: the TUI suspends and the genuine tmux client takes over your terminal. Press **`A`** (Shift+A) for an **in-pane attach**: the right pane becomes a **live embedded tmux client** without leaving ainb — a **`● INTERACTIVE — Ctrl+Q release`** badge appears, the embed takes the pane exactly as your layout has it, and everything you type goes straight into the session. (Want maximum width? Press **`B`** first to collapse the sidebar to its thin rail — the embed honors whatever sidebar state you chose.) Either way, leaving the session **never kills it** — detach and release only disconnect the client; the session keeps running.

## Full-screen attach (`a`)

![Full-screen attach — 'a' suspends the TUI into the real tmux client, a command runs inside the session, Ctrl+B d detaches back to ainb](../assets/screenshots/attach-fullscreen.gif)

*Press `a` on a session: the TUI suspends and `tmux attach-session` fills the whole terminal — tmux status bar, scrollback, prefix keys, everything. Detach with `Ctrl+B` `d` and ainb resumes exactly where you left it, with the session still listed.*

```text
s                       open the session list (session selected under "Other tmux")
a                       full-screen attach — the TUI suspends, tmux takes the terminal
echo FULLSCREEN_OK ⏎    typed straight into the session
Ctrl+B d                detach — ainb resumes, the session keeps running
```

This is a plain `tmux attach-session` on your terminal: every tmux feature works (prefix bindings, copy mode, panes, mouse per your tmux config). Use it when you want to *work in* the session for a while.

## In-pane attach (`A`)

![In-pane attach — Shift+A turns the preview pane into a live embedded tmux client with an INTERACTIVE badge; typed input lands in the session; Ctrl+Q releases](../assets/screenshots/attach-in-pane.gif)

*Press `A` (Shift+A): the pane becomes the live session in-place. The `● INTERACTIVE — Ctrl+Q release` badge marks the handoff and keystrokes land in the session — the sidebar stays exactly as you had it (pre-collapse with `B` for a near-full-width embed). `Ctrl+Q` releases and the session keeps running.*

```text
s                       open the session list
A                       in-pane attach — INTERACTIVE badge, embed fills the pane
echo INPANE_OK ⏎        typed input lands in the embedded session, output renders live
Ctrl+Q                  release — badge gone, read-only preview back, the session survives
```

While the embed is interactive, **all input belongs to the session**: keys are encoded to terminal bytes for the embedded client, and the mouse is forwarded as SGR events — the wheel scrolls the session's scrollback (tmux copy-mode), clicks land where you point. `Ctrl+Q` is the single escape hatch ainb keeps for itself. Releasing kills only the ephemeral embedded client — never the tmux session — and the pane returns to its read-only preview.

Use the in-pane attach for quick interventions — answer an agent's prompt, nudge a stuck command — without losing the session list and the rest of ainb's chrome around you.

## The two modes side by side

| | `a` full-screen | `A` in-pane |
|---|---|---|
| Surface | whole terminal (TUI suspends) | right pane (ainb chrome stays) |
| Visual cue | tmux status bar fills the screen | `● INTERACTIVE — Ctrl+Q release` badge on the pane |
| Leave with | `Ctrl+B` `d` (tmux detach) | `Ctrl+Q` |
| Lands you | back in ainb, session still listed | read-only preview back |
| tmux session | survives | survives |

## Keys

| Key | Action |
|-----|--------|
| `a` | Full-screen attach to the selected session |
| `A` | In-pane attach — the preview pane becomes a live embedded tmux client |
| `Ctrl+B` `d` | Detach from a full-screen attach (standard tmux detach) |
| `Ctrl+Q` | Release the in-pane embed (only key ainb intercepts while interactive) |
| `1`–`9` | Quick-attach by the number badge next to each attachable row |
| `B` | Toggle the sessions sidebar — collapse to the rail before `A` for a near-full-width embed |
| **Mouse** | Forwarded into the embed while interactive; wheel scrolls tmux scrollback/copy-mode |

## How it works

The embed drives a real `tmux attach-session` inside a PTY sized to the pane interior: output is parsed by a vt100 screen and rendered in place, key events are encoded to the exact terminal byte sequences a terminal would send, and mouse events are translated into pane-local SGR (mode 1006) reports. The embed honors your sidebar layout — it takes whatever the pane gives it (collapse the sidebar with `B` first for near-full width). One sizing note: the embed is a real tmux client, and under tmux's default `window-size latest` the session's window adopts the newest client's size, so the embedded pane's dimensions become the session's dimensions until another client resizes it. The full-screen path is simpler still — the TUI suspends, hands the terminal to `tmux attach-session`, and resumes when the client detaches. In both modes the client is the only thing that dies on exit; the tmux session and whatever is running inside it are untouched.

## See also

- [Keyboard shortcuts](keyboard-shortcuts.md)
- [Overview](overview.md)
