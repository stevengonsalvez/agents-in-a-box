# Hangar TUI keybindings

The Hangar plugin TUI (opened with `g` from the `ainb` home screen) is a
tab-switched control plane. Keys split into **routing-layer** keys (tab switches,
help, quit — handled before any screen reducer) and **per-screen** keys (handled
by the active screen's pure reducer).

Hints are rendered next to the control they affect (per
`feedback_keybinding_hints_near_control`), not only in a global help bar.

## Routing layer (all screens)

| Key | Action |
|-----|--------|
| `1` | Issue list (landing) |
| `2` | Task detail (only when a task is selected) |
| `4` | Skill manager |
| `,` | Settings |
| `?` | Help overlay |
| `Esc` | Close the active modal |
| `q` | Quit |

## Issue list (`1`)

| Key | Action |
|-----|--------|
| `j` / `k` | Move the selection |
| `Enter` | Open the selected issue's task detail |
| `a` | Open the agent picker for the selected issue |
| `/` | Filter |

## Skill manager (`4`) — P6.5

The three panes: a left skill list, a middle file tree (collapses below ~100
cols), and a right detail/editor pane. The action-key hints
(`s sync · i attach · d detach · ⏎ open`) render on the chip row, beside the
filter chips they sit next to.

| Key | Action | Daemon RPC |
|-----|--------|------------|
| `j` / `k` | Move the list selection | — |
| `s` | Sync the curated toolkit skills into the workspace | `hangar/skills_sync` |
| `Enter` | Open the detail pane (SKILL.md body + file tree) for the selected skill | `hangar/skill_get` |
| `i` | Attach the selected skill to the selected agent | `hangar/skill_attach` |
| `d` | Detach the selected skill from the selected agent | `hangar/skill_detach` |
| `r` | Refresh / dismiss the remote-conflict banner | — |
| filter chips `All` / `Used` / `Unused` / `Mine` | Narrow the list | — |

All five skill RPCs are **workspace-scoped**: the daemon resolves the subscribed
workspace and threads it into the secured `SkillRepo`, so a skill or agent id from
another tenant can never be read or mutated. `i` / `d` reject a cross-workspace id
pair (`hangar/skill_attach` returns an error).

## Settings (`,`)

| Key | Action |
|-----|--------|
| `j` / `k` | Move between sections / rows |
| `s` | Set the selected workspace active (Workspaces section) |
| `d` | Toggle the selected workspace's default flag (Workspaces section) |

## Agent picker (modal)

| Key | Action |
|-----|--------|
| `j` / `k` | Move the selection |
| `Enter` | Assign the selected actor |
| `Esc` | Close without assigning |
