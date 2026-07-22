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
| `3` | Skill manager |
| `4` | Autopilots |
| `K` | Kanban |
| `B` | Boards |
| `C` | Control center |
| `S` | Squads |
| `P` | Profile editor |
| `D` | Daemon health |
| `U` | Usage dashboard |
| `L` | Logs |
| `I` | Inbox |
| `,` | Settings |
| `?` | Help overlay |
| `Esc` | Close the active modal |
| `q` | Quit |

## Issue list (`1`)

| Key | Action |
|-----|--------|
| `j` / `k` | Move the selection |
| `Enter` | Open the selected issue's task detail |
| `c` | Open the create-issue wizard |
| `a` | Open the agent picker for the selected issue (sets the assignee) |
| `x` | Delete the selected issue (confirm overlay; Enter confirms, Esc cancels) |
| `/` | Filter |

If a delete is refused because the issue still has active run(s), a
second-chance amber overlay offers to cancel the run(s) and delete
(`c` / `Enter` confirms, Esc backs out).

## Create-issue wizard (`c` on the Issue list)

A single centered form showing every field at once, with a focused-row cursor.
The rows, in focus order:

| Row | Required? | Notes |
|-----|-----------|-------|
| Title | required | A trimmed-blank title blocks create. |
| Brief | optional | Multi-line free text; becomes the issue description and the dispatched prompt. Enter here inserts a newline (it never fires create). |
| Linked issue | optional | Single-line upstream reference (a URL or `owner/repo#123`); stored as the issue's `external_ref` and appended to the dispatched brief. |
| Repo | required | `@` opens a fuzzy dropdown (favorites-first roster, `scratch` always offered); `←` / `→` cycle the roster. A repo-less create is impossible. |
| Source branch | — | The branch the run branches FROM (prefilled `main`; blank = repo default). |
| PR-into (target) branch | — | The branch a future PR lands INTO (prefilled `main`). |
| Agent | — | `←` / `→` cycle the workspace's named agents (or the provider chips `claude`/`codex`/`copilot` when no named agents exist). Always valid. |

| Key | Action |
|-----|--------|
| `↑` / `↓` / `Tab` / `Shift+Tab` | Move focus between rows (wrapping) |
| `←` / `→` | Cycle the focused picker row (Repo / Agent) |
| `@` | Open the repo fuzzy dropdown (on the Repo row) |
| `Enter` | On Brief: insert a newline. Anywhere else: create the issue AND dispatch it — only when Title and Repo are set, otherwise focus jumps to the missing required row |
| `Esc` | Cancel the whole wizard (from any row) |

### Brief-or-link run guard

A dispatch (wizard Enter, or a later run of the issue) is **refused by the
daemon when the issue has neither a Brief nor a Linked issue reference** — a
title-only card would run a useless prompt. The refusal surfaces as a note:
`add a brief or link an issue before running`. Add a Brief or a Linked issue
and re-run.

## Task detail (`2` / Enter on an issue)

The screen opens on the issue's **detail card**: title (with display id),
Status / Priority / Created, Assignee / Agent, Repo / Source → Target branches,
Labels, a `Linked: ⧉ <ref>` line when an upstream issue is linked, the wrapped
description, and a run-history line — with the live transcript below.

| Key | Action |
|-----|--------|
| `j` / `k` | Scroll |
| `c` | Compose a comment (Enter submits, Esc cancels) |
| `R` | Retry the task (only once terminal). Whether a retry spawns anything is decided by the failure reason — see [Task failures](#task-failures); the reasons marked **No retry** there do nothing. |
| `X` | Cancel the running task (confirm overlay) |
| `x` | Delete the bound issue (confirm overlay; the daemon rejects a delete with active tasks and the rejection surfaces as a note) |

Issue deletion here and on the Issue list mirrors the `ainb hangar issue delete`
CLI command (dry-run preview without `--yes`).

### Task failures

When a task lands in `failed`, the recorded `failure_reason` says why, and it
decides whether a retry (`R` here, or `ainb hangar task retry <id>`) does
anything. Retries classify three ways:

- **Resume** — the infrastructure failed, not the agent; the retry resumes the
  same provider session.
- **Fresh** — the conversation is poisoned (the model wedged on its own
  context); the retry starts a new session instead of resuming.
- **No retry** — the failure is deterministic or terminal by intent; a retry is
  refused (`not retried (non-retryable or attempts exhausted)`), so pressing
  `R` on these does nothing useful.

| Reason | What triggers it | Retry |
|--------|------------------|-------|
| `agent_error` | The agent ran and errored or gave up. | No retry |
| `user_cancel` | A human cancelled the run. | No retry |
| `timeout` | A TTL sweeper expired a stalled row (queued / dispatched / running TTL) with no recorded cause. | No retry |
| `spawn_error` | The provider binary could not be spawned — e.g. the configured `claude` / `codex` path does not resolve. The agent never started. | No retry |
| `spawn_timeout` | The running→spawn setup phase wedged past its 60s umbrella bound (`HANGAR_SPAWN_SETUP_TIMEOUT_MS` override) — e.g. a headless keychain read that never returns. A wedged environment does not self-heal on re-dispatch. | No retry |
| `provision_error` | Pre-run setup failed before any provider was reached — the issue's repo could not be cloned / worktree-added, or the exec environment could not be prepared. Deterministic: re-fails identically. | No retry |
| `provider_contract_drift` | The provider's structured event stream carried no recognised completion or error event — a CLI output shape the parser does not know. The fix is a parser update, not a re-run. | No retry |
| `unknown` | Unclassified failure. | No retry |
| `runtime_offline` | The runtime hosting the agent went offline mid-run. | Retry, resume session |
| `runtime_recovery` | The task failed during daemon recovery (orphan reclaim). | Retry, resume session |
| `iteration_limit` | The agent exhausted its per-run iteration budget without finishing. | Retry, fresh session |
| `api_invalid_request` | The provider rejected the request as malformed (e.g. an Anthropic 400 `invalid_request_error`). | Retry, fresh session |
| `semantic_inactivity` | The run stalled with no semantic progress (no new tool calls / output). | Retry, fresh session |

Every retry chain is capped by the task's `max_attempts` regardless of reason.

**Where the real error surfaces:** a `provision_error` writes the underlying
setup error (the failed clone / worktree message) into the task's `result`, so
the task-detail screen shows it directly. A `spawn_timeout`'s cause is logged by
the daemon — check the Logs screen (`L`) or the daemon log. For agent-side
failures the transcript on this screen carries the run output.

## Skill manager (`3`) — P6.5

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

Six stacked sections: Daemon, Providers, Keys, Workspaces, Members,
Notifications. `j` / `k` switch sections; `J` / `K` (and `↑` / `↓`) move the
row cursor within the focused section.

| Key | Action |
|-----|--------|
| `j` / `k` | Move between sections |
| `J` / `K` / `↑` / `↓` | Move the row cursor within the section |
| `Enter` / `Space` | Edit / toggle the focused row — Daemon only |
| `Space` / `t` | Toggle the selected kind×channel cell — Notifications only |

Edit/toggle is section-scoped. On **Daemon**, `Enter` or `Space` acts on the
focused config knob by type: toggle a bool, cycle an enum, or open the numeric
overlay for an int. On **Notifications**, `Space` / `t` toggles the selected
kind×channel cell (`h` / `l` move the channel column; `g` flips the edit scope
between the host-wide global rule and the active-workspace override). The
Providers, Keys, Workspaces, and Members panes are read-only here (their
mutations are CLI-first, e.g. `ainb hangar member set-role`).

## Agent picker (modal)

| Key | Action |
|-----|--------|
| `j` / `k` | Move the selection |
| `Enter` | Assign the selected actor (`hangar/issue_update`) |
| `Esc` | Close without assigning |
