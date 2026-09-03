# Hangar TUI keybindings

The Hangar plugin TUI (opened with `g` from the `ainb` home screen) is a
tab-switched control plane. Keys split into **routing-layer** keys (tab switches,
help, quit — handled before any screen reducer) and **per-screen** keys (handled
by the active screen's pure reducer).

Hints are rendered next to the control they affect (per
`feedback_keybinding_hints_near_control`), not only in a global help bar.

## Routing layer (all screens)

Nine keys, down from eighteen (crisp B5). The tab strip carries the seven
screens the loop runs through; the rest moved behind the command palette.

| Key | Action |
|-----|--------|
| `1` | Issue list (landing) |
| `2` | Task detail (only when a task is selected) |
| `K` | Runs (the task board; the screen and its widget are still `kanban`) |
| `B` | Boards |
| `I` | Inbox |
| `A` | Agents (roster + guided create wizard) |
| `,` | Settings |
| `Ctrl+P` | Command palette: `Go: <screen>` first, then `hangar/search` as you type; Enter jumps |
| `?` | Help overlay |
| `Esc` | Close the active modal |
| `q` | Back to the `ainb` home screen (press `q` again there to quit) |

### Screens behind the palette

These nine kept their screens, their reducers and their per-screen keys; only
the tab hotkey went. Reach one with `Ctrl+P` and its word, or read the list off
the Settings screen's **More screens** section.

| Type | Screen |
|------|--------|
| `^P skills` | Skill manager |
| `^P autopilots` | Autopilots |
| `^P daemon` | Daemon health |
| `^P usage` | Usage dashboard |
| `^P logs` | Logs |
| `^P control` | Control center |
| `^P fleet` | Fleet (every live session: lenses, stop / interrupt / continue / kill, chat) |
| `^P squads` | Squads |
| `^P profiles` | Profile editor |

The freed keys (`3` `4` `C` `D` `F` `L` `P` `S` `U`) now reach the active
screen's reducer, so a screen-local binding on one of them is live rather than
eaten by the router.

`?` and `H` belong to the plugin on every hangar screen, typed or not: the host
never reserves them here, so a title or brief containing `H` reads through
verbatim (they used to toggle the host help and then swallow every key until
Esc).

## Issue list (`1`)

| Key | Action |
|-----|--------|
| `j` / `k` | Move the selection |
| `Enter` | Open the selected issue's task detail |
| `c` | Open the create-issue wizard |
| `a` | Open the agent picker for the selected issue (sets the assignee) |
| `s` | Create a sub-issue with the selected issue as parent |
| `d` | Mark the selected issue Done (cascades to a parent's child-done barrier) |
| `x` | Delete the selected issue (confirm overlay; Enter confirms, Esc cancels) |
| `y` | Activity timeline modal for the selected issue (`j`/`k` scroll, `r` refresh) |
| `/` | Filter |
| `f` | Faceted filter panel (arrows / `hjkl` move, Space toggles, `C` clears, `f` / Esc closes) |
| `Tab` / `Shift+Tab` | Cycle the filter chips All → Members → Agents → Mine |

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
| Accept | optional | Acceptance criteria, one per line (Enter inserts a newline). They render as checkboxes on the task detail (`a` / `t` there) and do NOT gate `done`. |
| Context | optional | Context references, one per line. |
| Priority | — | `←` / `→` cycle P3 … P0. |
| Due | optional | `YYYY-MM-DD`. |
| Labels | optional | Comma-separated. |
| Repo | required | `@` opens a fuzzy dropdown (favorites-first roster, `scratch` always offered); `←` / `→` cycle the roster. A repo-less create is impossible. The roster is read from the host's New Session scan cache + favorites when the plugin connects. |
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

The screen is the **execution view**. Top to bottom: a sticky run card for the
EXPANDED run (`◔ impl-1 is working · 7m 17s · 10 tools · $0.42`) with its branch
and PR beneath it; the issue's **detail card** — title (with display id), one
meta line (status · priority · assignee · created · `@repo` · source → target),
Labels/Due when set, a `Linked: ⧉ <ref>` line when an upstream issue is linked,
acceptance criteria, properties and the wrapped description; then the
**execution log** of every run of this issue (running on top, then failed,
newest first inside each bucket); then that run's **transcript**; with the
issue's **activity** narrative in a right-hand column.

The transcript is the issue's NEWEST run: expanding an older attempt says it has
no readable transcript rather than showing another run's lines.

| Key | Action |
|-----|--------|
| `j` / `k` | Scroll the transcript |
| `Enter` | Expand the next run in the execution log (wraps; a no-op with fewer than two runs) |
| `c` | Compose a comment (Enter submits, Esc cancels) |
| `R` | Retry the task (only once terminal). This is the OPERATOR override: it force-requeues a `failed` or `cancelled` task whatever its `failure_reason` (an `agent_error` that the automatic chain would never retry included), as a child attempt chained by `parent_task_id`. On a `done` task it does nothing. The reason still decides whether the child RESUMES the parent's provider session or starts FRESH — see [Task failures](#task-failures). |
| `X` | Cancel the running task (confirm overlay). The process group is killed (an interactive run's tmux session by exact name); a dirty worktree is kept. |
| `x` | Delete the bound issue (confirm overlay; the daemon rejects a delete with active tasks and the rejection surfaces as a note) |
| `a` | Walk the acceptance-criteria cursor |
| `t` | Toggle the selected acceptance criterion |
| `o` | Open the captured PR URL in the host browser (only when a PR was captured) |

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
- **No retry** — the failure is deterministic or terminal by intent; the
  AUTOMATIC chain refuses (`not retried (non-retryable or attempts exhausted)`).
  The operator's `R` (and `ainb hangar task retry <id>`) still force-requeues
  such a task as a FRESH attempt; use it once the cause is fixed.

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
the daemon — check the Logs screen (`^P logs`) or the daemon log. For agent-side
failures the transcript on this screen carries the run output.

## Skill manager (`^P skills`) — P6.5

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

Seven stacked sections: Daemon, Providers, Keys, Workspaces, Members,
Notifications, More screens. `j` / `k` switch sections; `J` / `K` (and `↑` /
`↓`) move the row cursor within the focused section.

**More screens** is read-only: the nine screens the tab strip dropped, each
beside the `^P` word that reaches it.

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

## Boards (`B`)

| Key | Action |
|-----|--------|
| `←` `→` `↑` `↓` / `hjkl` | Move the focus |
| `[` / `]` | Previous / next board |
| `b` | Create a board |
| `n` / `r` / `x` | Add / rename / delete a column |
| `m` | Toggle auto-move |
| `c` | Create a card: title → repo (`@` dropdown, `↑`/`↓` pick, Enter) → agent → assignee profile. The TITLE is the run prompt; a card has no brief field. |
| `e` | Edit the focused card |
| `Enter` | `Run ▾`: `↑`/`↓` pick Headless (`claude -p`) or Interactive (a real, attachable tmux session `tmux_hangar-<task>`), Enter launches |
| `a` | Attach to the focused card's live session (tmux popup; needs the TUI inside tmux) |
| `X` | Cancel the focused card's run |
| `t` | Run timeline overlay |
| `s` | Assign the focused card to a squad |
| `w` | Add a depends-on blocker (Tab cycles the link kind) |
| `R` | Toggle auto-run |
| `Shift+↑` / `Shift+↓` | Reorder cards |
| `Shift+←` / `Shift+→` | Reorder columns |
| `d` | Remove the card from the board (the issue survives) |

A role-gated Pipeline board (`ainb hangar pipeline init`) pulls a card through
its stages by itself; there is no key for that. Its columns carry no FSM
mapping, so cards move only when a stage finishes.

## Control center (`^P control`)

| Key | Action |
|-----|--------|
| `j` / `k` | Move between cards (needs-input first, newest first) |
| `h` / `l` | Move the option cursor on an ASK |
| `Enter` | Answer with the highlighted option |
| `1` … `9` | Answer with that option directly |

An ASK answer is routed by option POSITION into the agent's live picker and the
title row confirms delivery by dropping the card; when the daemon refuses
(ambiguous target, no live session, delivery failed, already answered elsewhere)
the reason is painted on the title row in red and the card stays.

## Squads (`^P squads`)

| Key | Action |
|-----|--------|
| `n` | Create an agent (name) |
| `c` | Create a squad |
| `a` / `d` | Add / remove a member |
| `r` | Edit the selected member's role (comma-separated tokens; the pipeline's role gates match these) |
| `i` | Edit the squad instructions |
| `x` | Fan the CURRENT issue (the one selected on the Issue list) out to the squad: onto the pipeline's first stage when one exists, else a leader-only brief |

## Agents (`A`)

| Key | Action |
|-----|--------|
| `n` | Guided create: Name → Description → Provider (`←`/`→`) → Model → Instructions → confirm (Enter) |
| `x` | Delete the selected agent (confirm) |

## Runs (`K`)

| Key | Action |
|-----|--------|
| `←` `→` `↑` `↓` / `hjkl` | Move the focus |
| `Shift+←` / `Shift+→` (or `<` / `>`) | Move the card to the adjacent column |
| `R` | Force-requeue a focused failed / cancelled card |

## Logs (`^P logs`) · Inbox (`I`) · Profiles (`^P profiles`)

| Screen | Keys |
|--------|------|
| Logs | `a` all · `i` info · `w` warn · `e` error (level floor; re-reads the newest daemon log file) |
| Inbox | `j` / `k` move the attention row · `h` / `l` move the option cursor · `Enter` or `1`-`9` answer the focused ASK · `r` mark all read · `f` cycle the filter (all → asks → runs → issues) |
| Profiles | `j` / `k` move · `t` cycle the tier premium → balanced → fast |

## Agent picker (modal)

| Key | Action |
|-----|--------|
| `j` / `k` | Move the selection |
| `Enter` | Assign the selected actor (`hangar/issue_update`) |
| `Esc` | Close without assigning |
