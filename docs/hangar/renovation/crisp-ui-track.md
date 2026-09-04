> Appendix to `docs/hangar/renovation/PLAN.md`. Planning pass output, unedited except for punctuation.

# Crisp UI track: hangar-tui plugin

Parallel to the spine renovation (ACP task execution, issue/card/task state collapse).
Ships in days. Plugin-only, plus two trivially-additive proto/daemon relaxations named below.
All anchors are `ainb-tui/crates/ainb-plugin-hangar/` unless prefixed.

Evidence: `docs/hangar/proofs/fullstack/p1-*.png` .. `p4-*.png`, `REPORT.md` defect ledger,
`docs/hangar/tui-keybindings.md`, exec-map section 2.

---

## 0. The one-line diagnosis

Every screen has the DATA and paints the ID. The plugin holds `hangar/agents_list`,
`hangar/issues_list`, `hangar/tasks_list` and `hangar/run_history` in memory, then renders
`agent:01M1FHM2YSRSXZQFR29ZAYF56V`, `Task started: 01M1GVN6MAF3121GEDM1E66KW5`, `◔0` and
`Project: 01M1FH6AG5YJF1S`. Exactly ONE screen (Kanban) resolves names, and it is the one
screen that reads crisp. The crispness gap is a resolution + composition gap, not a data gap.

Second theme: the plugin advertises 16 tabs, 18 router keys, up to 24 simultaneous hints and
three separate attention surfaces (Control `C`, Inbox `I`, the board `N need you` chip) for a
loop that has four verbs (author, run, answer, read the result).

---

## 1. Screen-by-screen crispness audit

### 1.1 Issues board (`1`) - `p1-2`, `p1-3`

```
┌ noise ─────────────────────────────┐  ┌ missing ──────────────────────────┐
│ 7 columns x ~250px, 85% dead space │  │ agent on the card                 │
│ `◇ None` priority chip on every    │  │ run state / elapsed               │
│   card (default P3 = "None")       │  │ PR badge (Kanban has it)          │
│ `◔0` assignee = first char of a    │  │ attention flag ("this asked you") │
│   raw ULID (defect 8 on the board) │  │ "N working" (chip exists, dead)   │
│ 11 footer hints + 4 filter chips   │  └───────────────────────────────────┘
└────────────────────────────────────┘
```

- `widgets/card_board.rs:165` `BoardCard` carries `display_id, title, priority,
  assignee_initial, linked, subtasks, not_dispatched`. No agent, no run state, no elapsed,
  no PR. p1-3 is a card mid-run that renders identically to an untouched backlog card.
- `widgets/card_board.rs:662` paints `◔` + `card.assignee_initial`, the FIRST CHAR of
  `agent:<ulid>`, hence `◔0` in every still.
- `widgets/card_board.rs:280` `PriorityChip::None.glyph() = '◇'`, label `"None"`
  (`:262`). Default priority 0 maps to `None` (`:243`), so `◇ None` prints on every
  untriaged card: 100% of cards in every still, zero information.
- Working chip: `widgets/working_chip.rs:30` renders, `issue_list.rs:2814` calls it,
  `app_screens.rs:649` declares `working_count` .. and **nothing ever assigns it**
  (`grep working_count src/` returns only the declaration + two reads). Always 0. The
  `issue_board_snapshot` .snap shows `⬡⬡` because the test seeds it directly.
- Wizard titled `New task` (`p1-2`) but it creates an ISSUE. Vocabulary collision with the
  `2 Task` tab.

### 1.2 Kanban / runs (`K`) - `p1-4`

The reference implementation, already crisp. `kanban.rs:509 card_title` composes
`agent · age · status · branch · PR ✓`; `kanban.rs:264 set_agent_names` resolves the roster;
`kanban.rs:555 card_pr_chip` renders the CI rollup.

Two flaws:
- Branch printed raw: `ainb/01M1FKF4BDNQ3JK5CQSS3N9GP8` (28 chars) wraps to a second line
  and pushes the PR chip off the tile (`kanban.rs:520`). The doc comment at `:502-508`
  predicts this exact failure and it happens anyway.
- Card footer still the generic `◇ None ◔i` from `card_board.rs`, duplicating the state
  already on the title line.

### 1.3 Boards / pipeline (`B`) - `p2-6`

- **Two hint bars.** `boards.rs:2672 BOARDS_HINTS` (16 pairs) painted by
  `boards.rs:2692 render_hint_band` at the TOP, plus `chrome.rs:196` `Screen::Boards`
  (5 pairs) + 3 globals at the BOTTOM. 24 hints, overlapping subsets, different keys.
- Health chips `● daemon ok  ● roles covered  ○ wip 1/1 QA  ● 0 stuck` are the best line in
  the app and are weighted the same as everything else.
- Card in a role-gated column with a live run shows nothing about the run (same
  `card_board.rs` widget). `wip 1/1 QA` in the header is the only evidence a run exists.
- `◔T` = first char of the tester agent's id. Same defect 8.

### 1.4 Task / issue detail (`2`) - `p2-1`

- 6 header rows of key/value, 5 of which read `, ` or `unassigned`.
  `task_detail.rs:1113-1126` prints `Assignee: unassigned  Agent: , ` on a dispatched issue
  (defect 8).
- A **second, floating facet panel** flush-right at the same vertical position repeats
  `Status`/`Assignee`, adds `Project: 01M1FH6AG5YJF1S` (raw ULID) and `Notes: Add GET
  /api/tick` (mid-word truncation).
- `Repo:` prints the full absolute path `/home/claude/ainb-e2e-home/projects/boxtrack`.
- **~80% of the screen is empty.** No live run card, no transcript, no activity, nothing.
  A `TranscriptEntry` model exists (`task_detail.rs:158`) but nothing backfills it on open
  (defect 7).
- 8 footer hints including both `X:cancel` and `x:delete`.

### 1.5 Control Center (`C`) - `p3-5`

- Session row is a bare ULID `▸ASK 01M1GVN6MAF… 0s`. No issue, no agent, no title.
- `control_center.rs:809` paints `age 0s · tok: · tools: · Δ: · hook · host`: three of
  six fields are permanently `, `. Dead columns rendered as data.
- The OPTIONS block (`①②` + Recommended + descriptions) is genuinely good and is the thing
  the Inbox should inherit.
- 60% dead width, 85% dead height.

### 1.6 Inbox (`I`) - `p4-6`

Worst screen in the app, and the one multica says should be the single attention surface.

- 35 rows of `Task started: <26-char ULID>` / `Task finished (Success): <ULID>`.
  `inbox.rs:134 render_entry` paints `{kind}{summary}` verbatim from the daemon.
- No timestamps, no agent, no issue title, no unread markers per row, no grouping,
  no filters, no ordering beyond newest-first.
- Vocabulary: `(Success)` / `(Failure)` / `(Cancelled)` capitalised, versus the task FSM's
  `done`/`failed`/`cancelled` everywhere else.
- Real bug visible: `New issue: Boxtrack scaffold v2: TypeScript monorepo (api + webBoxtrack
  scaffold v2: TypeScript monorepo (api + web)` - the title is concatenated with itself.
- **`InboxEntryRow` already carries `subject_id`, `event`, `created_at`**
  (`ainb-hangar-proto/src/events.rs:1051-1076`). Everything needed to render a human line is
  in the plugin's own snapshots. Pure composition gap.

### 1.7 Fleet (`F`) - `p4-7`

- **Three stacked count rows saying the same thing** in two vocabularies:
  - row 0: `1 Needs input 0  2 Idle 0  3 Completed 0  4 Running 0  [5 All 0]` (`fleet.rs:376`)
  - row 1: `0 INPUT  0 RUN  0 IDLE  0 DONE` (`fleet.rs:2644`)
  - row 2: `ACTION QUEUE · 0 sessions · F5 refresh` + `0/0` (`fleet.rs:2596`)
- Empty state is broken grammar: `No all sessions` (`fleet.rs:2709`,
  `format!("No {} sessions", state.filter.label().to_lowercase())`).

### 1.8 Usage (`U`) - `p1-6`

- `per agent` row prints the raw ULID (`usage_dashboard.rs:305`,
  `put_str(.., &agent.agent_id, ..)`) - defect 8.
- `recent runs` rows print the PROVIDER (`claude`), not the agent, not the issue, no
  timestamp, no link (`usage_dashboard.rs:238 render_run_row`).
- Chronological, not failed-first. The one failed run is at the bottom.
- `total: ... (3 runs)` above a list of FOUR runs: the failed run has no usage row, so the
  count and the list disagree on screen.

### 1.9 Help (`?`) - `p4-1`

Monotone gold, centred, no boxes, full-screen replacement rather than an overlay. Complete
and accurate (`app_screens.rs:1509 HELP_LINES`, pinned by four tests). Lowest priority.

### 1.10 Cross-cutting inconsistencies

| axis | today | count |
|---|---|---|
| tab strip | 16 tabs, 138 cols, never fits 80 | `chrome.rs:45` |
| router keys | 18 | `router.rs:30` |
| attention surfaces | Control `C`, Inbox `I`, board `N need you` chip | 3 |
| boards | Issues (7 lifecycle cols), Kanban (4 task-status cols), Boards (user cols) | 3 |
| hint bars | footer everywhere + a second top band on Boards | 2 |
| max hints on one screen | Boards: 16 + 5 + 3 | 24 |
| run status words | `running/done/failed/cancelled` (task FSM), `Success/Failure/Cancelled` (inbox), `success/failed` (usage), `RUN/DONE` + `Running/Completed` (fleet) | 4 vocabularies |
| id rendering | `HGR-3` (issues), `#1J7MR7` (kanban), `#AKQ408` (boards), full ULID (control, inbox, usage, facet panel) | 4 |

---

## 2. The crisp target

### 2.1 Status vocabulary: one table, enforced by one helper

Add `src/vocab.rs` with two total functions and make every screen call them.

```
run/task:    queued · running · done · failed · cancelled     (the CHECK'd FSM, 5 tokens)
issue:       backlog · todo · in progress · in review · done · blocked · cancelled
attention:   ASK · ERR · IDLE · WAIT                          (3-4 letter codes, uppercase)
fleet lens:  needs input · running · idle · done              (one row, lowercase, delete row 1)
```

Rules: lowercase everywhere except the four attention codes. Never `Success`, never `DONE`,
never `Completed`. One glyph per token, shared: `○ queued  ◔ running  ● done  ✗ failed  ⊘ cancelled`.

### 2.2 Issue board card

Footer swaps by state instead of growing a row. `CARD_ROWS` (`card_board.rs:355`) stays 6,
so no geometry change and no column-capacity loss. (The planning pass wrote 5: that is the
count of a card's content elements — id, two title lines, footer, and one border — not of
its rows, which are top border, id, title, title, footer, bottom border.)

```
 idle card                            running card
╭──────────────────────────────╮     ╭──────────────────────────────╮
│ HGR-3                    ⧉   │     │ HGR-3                    ⧉   │
│ Add GET /api/version         │     │ Add GET /api/version         │
│ endpoint                     │     │ endpoint                     │
│ ◆ High                 ⊟ 1/2 │     │ ◔ impl-1 · running 2m · PR ✓ │
╰──────────────────────────────╯     ╰──────────────────────────────╯

 card that is asking you              card refused dispatch
╭──────────────────────────────╮     ╭──────────────────────────────╮
│ HGR-7                        │     │ HGR-9                  ⚠     │
│ Decide the sqlite location   │     │ Dependent B                  │
│                              │     │                              │
│ ● impl-1 · ASK 40s           │     │ ◇ blocked by MHAJBV          │
╰──────────────────────────────╯     ╰──────────────────────────────╯
```

`BoardCard` gains three fields: `run: Option<RunChip { agent, state, elapsed_ms }>`,
`pr: Option<PrChip>`, `attention: Option<AttentionKind>`. Footer priority: attention >
run > priority chip. `◇ None` never prints again (drop the chip entirely when priority is
the default, `card_board.rs:651-660`).

### 2.3 Issue / task detail

```
┌ HGR-5 · Ticket stats: GET /api/tickets/stats ────────────────────────────────┐
│ ◔ impl-1 is working · 7m 17s · 10 tools · $0.42       X cancel   a attach    │  live run card, sticky
│   ainb/…N9GP8 → main   ·   PR #8 ✓ checks green                              │
├──────────────────────────────────────────────────────────────────────────────┤
│ in progress · P2 · created 2026-09-02 · @boxtrack · main → main              │  ONE meta line (was 5)
│ Acceptance: 0/3  □ GET /api/tickets/stats covered by vitest and green        │
│                  □ npm run check green                                       │
│                  □ pull request opened with the exact issue title            │
│ Props: ◆ Sprint: S2  ◆ Owner: amy      Meta: -                               │
├──────────────────────────────────────────────────────────────────────────────┤
│ Add GET /api/tickets/stats to the hono api under api/: returns {total, …}    │  brief, 2 lines + fold
└──────────────────────────────────────────────────────────────────────────────┘
┌ transcript ───────────────────────────┬ activity ────────────────────────────┐
│ ● thinking  reading api/src/db.ts     │ 7m  impl-1 claimed the issue         │
│ ▸ tool      Edit api/src/routes.ts    │ 6m  todo → in progress               │
│ ✓ result    3 files changed           │ 2m  comment: started                 │
│ ● text      Registering the route be… │                                      │
│                             j/k scroll│                                      │
└───────────────────────────────────────┴──────────────────────────────────────┘
 R:retry  X:cancel  o:PR  a:criterion  ?:help  q:quit
```

- The floating right-hand facet panel from `p2-1` is deleted. `Project:` (raw ULID) and the
  truncated `Notes:` go with it; `Status`/`Assignee` already live in the meta line.
- Keep the literal substrings `Acceptance: 0/3`, `Props:`, `Meta:`, `◆ Sprint: S2` - four
  tripwires assert them (section 4).
- `Repo:` renders `@boxtrack` (the label), not the absolute path.
- Terminal run: the live card becomes `● impl-1 finished · 2m09s · $0.58 · PR #6 ✓`.
- Interactive run: the transcript pane says `interactive run - attach with a to see output`
  and nothing else. There is no capture on that path (see section 5).

### 2.4 Inbox as the one attention surface

```
Inbox  member:me   [2 need you] [35 unread]    (all) asks  runs  issues        I inbox
┌ needs you ──────────────────────────────────────────────────────────────────┐
│▸● ASK  40s   impl-1 on HGR-7 Decide the Boxtrack sqlite file location        │
│   ① data/boxtrack.db (Recommended)   Repo-root data/ dir, outside api/src    │
│   ② api/app.db                       Sits next to api package                │
│   h/l pick · 1-9 or enter to answer                                          │
│ ● ERR  3m    rev-1  on HGR-5 Ticket stats · agent_error exit 65              │
└─────────────────────────────────────────────────────────────────────────────┘
┌ recent ─────────────────────────────────────────────────────────────────────┐
│ ✗ 9m   qa-1   failed   HGR-5 Ticket stats: GET /api/tickets/stats · exit 1   │
│ ● 2m   impl-1 done     HGR-3 Add GET /api/version endpoint · PR #6 ✓         │
│ ◔ 4m   impl-1 running  HGR-3 Add GET /api/version endpoint                   │
│ + 12m  new issue       HGR-8 Dependent B: must refuse to run while A is open │
└─────────────────────────────────────────────────────────────────────────────┘
 enter:answer  r:mark read  f:filter  ^P:search  ?:help  q:quit
```

- `needs you` block is `attention/list` open rows, sorted by age. Inline answer is
  `control_center.rs`'s existing option render + reducer, moved not rewritten
  (`control_center.rs:645 render_control_center`, the answer path from `p3-5`).
- `recent` block is `inbox_list` with each line recomposed locally:
  `subject_id` -> tasks snapshot -> agent name + issue title; `event` -> a lowercase verb
  from the vocab table; `created_at` -> relative age. Failed rows sort first inside their
  age bucket.
- Filters `(all) asks runs issues` are client-side over the cached rows, `f` cycles.
- The board's `N need you` chip and the Control tab both become views of THIS state.

### 2.5 Tab strip

```
before (16 tabs, 138 cols, wraps under 138, never fits 80):
[1]Issues [2]Task [3]Skills [4]Autopilots [K]Kanban [B]Boards [D]Daemon [U]Usage
[L]Logs [I]Inbox [C]Control [F]Fleet [S]Squads [P]Profiles [A]Agents [,]Settings

after (7 tabs, 72 cols of ink / cursor at 74; every label fits the 80x24 floor,
the right cluster yields until 93 - measured on the shipped strip, and pinned by
`the_seven_tab_strip_fits_the_eighty_column_floor`):
[1]Issues  [2]Task  [K]Runs  [B]Boards  [I]Inbox  [A]Agents  [,]Settings   ⬡⬡ 2 working  default ● online
```

Keys that survive in `ROUTER_KEYS` (`router.rs:30`): `1 2 K B I A , ? q` = **9** (from 18).

Demoted, 9 keys / 8 screens: `3` Skills, `4` Autopilots, `D` Daemon, `U` Usage, `L` Logs,
`C` Control, `F` Fleet, `S` Squads, `P` Profiles. Reachable two ways, both must land in the
same commit as the demotion:

1. `^P` palette gains a plugin-local `Go: <screen>` entry family, matched client-side and
   merged ahead of the daemon's `hangar/search` results (`command_palette.rs`,
   `SearchEntryKind` untouched - the Go entries are a separate local vec).
2. `,` Settings gains a `More screens` section listing all nine with their palette names.

`K` is relabelled `Runs` (it is the only run-centric board and `Kanban` names the widget,
not the content). Hotkey unchanged, so muscle memory and `docs/hangar/tui-keybindings.md`
survive with a label edit.

### 2.6 Hint bar grammar

```
<= 5 contextual hints, then exactly 3 globals: ^P:search  ?:help  q:quit
key:verb          verb is one bare imperative word, lowercase
                  no two hints on a screen share a verb
                  never advertise a router key (already enforced, chrome.rs:368)
overflow          goes to the context menu (context_menu.rs) and `?`, never to a second bar
```

- Delete `boards.rs:2672 BOARDS_HINTS` and `boards.rs:2692 render_hint_band` outright. The
  16 hints move to the existing card/column context menu. Boards footer becomes
  `↵:run  a:attach  t:timeline  c:card  x:cancel` + globals.
- Issues footer 9 -> 5: `c:create  ↵:open  a:assign  /:filter  x:delete`.
- Task detail 5 -> 4: `R:retry  X:cancel  o:PR  a:criterion`. `x:delete` moves to the
  context menu (`X` and `x` on one bar is a misfire waiting to happen).
- Read-only panes (`Usage`, `Daemon`, `Logs`) keep the empty contextual list they already
  have (`chrome.rs:207`).

---

## 3. Quick wins on data already in plugin memory

Each is small, independent, and needs no new daemon call except where marked.

| # | defect | fix | anchor |
|---|---|---|---|
| Q1 | 8 | Task detail `Assignee: agent:<ulid>` / `Agent: , ` -> resolve through the existing `agent_names(&actors)` map | `app_screens.rs:1241` (helper exists, Kanban's only caller is `kanban.rs:264`); apply at `task_detail.rs:1113-1126` |
| Q2 | 8 | Usage `per agent` raw ULID -> agent display name, same map | `usage_dashboard.rs:305` |
| Q3 | 8 | Board card `◔0` -> `◔ impl-1`, and drop `◇ None` when priority is default | `widgets/card_board.rs:651-670` |
| Q4 | 7 | Transcript backfill on task-detail open: fire `hangar/board_card_timeline` and feed `jsonl_timeline::parse_timeline` into `TaskDetailState`. The whole pipeline already exists for Boards. **Needs one trivially-additive change**: make `BoardCardParams.board_id` an `Option<String>` and skip the board-membership guard when absent, so an uncarded issue resolves | plugin `plugin.rs:4936` (open path, fires nothing today), reuse `plugin.rs:1811 apply_board_card_timeline`; daemon `ainb-hangar-daemon/src/rpc/mod.rs:11226-11231` |
| Q5 | 9 | `R` on a done task silently no-ops -> `push_system_line("this run finished; R only retries a failed or cancelled run")` before raising the intent | `task_detail.rs:573` (`TaskDetailIntent::RetryTask`), note sink `task_detail.rs:363` |
| Q6 | 6 | Repo roster fetched once at connect, so a repo added later is unpickable -> re-fire `HANGAR_REPO_LIST` on wizard open | fetched at `plugin.rs:2592`; wizard opens at `issue_list.rs:1980 enter_create_mode` |
| Q7 | 12 | Wizard `@` filter narrows but Enter picks `scratch`. **Root cause confirmed**: `repo_candidates` unconditionally prepends `RepoOption::scratch()` at index 0 regardless of the query, and the cursor resets to 0 on every keystroke. Fix: include scratch only when the query is empty or scratch fuzzy-matches | `boards.rs:451-459` (prepend), `issue_list.rs:2426-2431` (cursor reset) |
| Q8 | 21 | Boards rename input types `u` for Ctrl+U -> add a `BoardsKey::ClearLine` arm | `boards.rs:1921 column_rename_key`; the key decode is `app_screens.rs:~2720 key_char` |
| Q9 | - | Inbox lines: ULID -> `<agent> <verb> <HGR-n> <title>` + relative age + per-row unread dot, using `subject_id`/`event`/`created_at` already on the wire | `inbox.rs:134 render_entry`, row shape `ainb-hangar-proto/src/events.rs:1051` |
| Q10 | - | Failed-first ordering in run lists (usage `recent runs`, inbox `recent`) | `usage_dashboard.rs:225-235` loop, `inbox.rs:120` loop |
| Q11 | - | "N working" chip is dead code: `working_count` is declared and read but never assigned. Assign it from the tasks snapshot's running column and render `⬡⬡ 2 working` instead of bare glyphs | `app_screens.rs:649` (declared), `issue_list.rs:2814` (read), `widgets/working_chip.rs:30` (render) |
| Q12 | - | Presence dots on agent rows: `widgets/presence_dot.rs:17` exists and is used only by its own test. Wire it into the Agents roster + agent picker rows alongside a workload chip (`N running / max_concurrent`) | `widgets/presence_dot.rs:17`, `widgets/actor_row.rs:76`, `screen/agents.rs` |
| Q13 | 5 | Dispatch-refusal note: de-duplicate the text and name the blocking row (`a run is already active: 01M1FK… (running)`) resolved from the tasks snapshot. The phantom-on-terminal-rows half is daemon-side and stays open | note sink `boards.rs:3927` / `issue_list` note path |
| Q14 | - | Kanban branch elide: `ainb/01M1FKF4BDNQ3JK5CQSS3N9GP8` -> `ainb/…N9GP8` so the PR chip stops falling off the tile | `kanban.rs:513-520` |
| Q15 | - | Fleet: delete the duplicate count row and fix the empty-state grammar (`No all sessions` -> `no sessions yet · press 5 for all`) | `fleet.rs:2644` (row to delete), `fleet.rs:2709` (grammar) |
| Q16 | - | Control Center stat strip prints three permanent em-dash columns (`tok: · tools: · Δ , `). Drop the unknown columns rather than rendering placeholders | `control_center.rs:809` |

---

## 4. Sequencing: 5 PR-sized steps

Each step is independently shippable and independently recordable. Sizes are S (half day)
and M (one to two days).

### Step 1 (M) - Resolve every id to a name

**Does:** Q1, Q2, Q3(name half only), Q4, Q5, Q6, Q7, Q8, Q9, Q10, Q13, Q14, Q16.
**Touches:** `task_detail.rs`, `usage_dashboard.rs`, `inbox.rs`, `kanban.rs`, `boards.rs`,
`issue_list.rs`, `plugin.rs`, `widgets/card_board.rs`, `control_center.rs`, plus
`ainb-hangar-proto/src/snapshots.rs` + `ainb-hangar-daemon/src/rpc/mod.rs:11226` for the
optional `board_id`.
**Tripwires:** none of the seven. `tripwire_issue_acceptance_tick` asserts `Acceptance: 0/3`
on task detail - **do not move that row in this step**.
**Other tests to refresh:** `snapshot_kanban_layout__render_full_board_snapshot.snap`
(branch elide), `usage_dashboard.rs:428 renders_totals_and_per_agent_rows`, `inbox.rs`
in-module render tests, `boards.rs:3664 rename_column_edits_and_commits` (new Ctrl+U arm),
`transcript_render_snapshot.rs`, `snapshot_control_center__*.snap` (3, stat strip).
**Risk:** the `board_id` relaxation is the only non-plugin edit. Guard the workspace tenant
check; only the board-membership check is skipped.

### Step 2 (M) - Cards and hint bars

**Does:** section 2.2 card footer state machine, section 2.6 hint grammar, Q11 working chip,
Q15 fleet rows, `src/vocab.rs` and its first callers.
**Touches:** `widgets/card_board.rs` (`BoardCard` +3 fields, `render_card` footer),
`issue_list.rs` (card mapping + working count), `chrome.rs:169 footer_hints`,
delete `boards.rs:2672 BOARDS_HINTS` + `:2692 render_hint_band`, `fleet.rs`.
**Tripwires:** none of the seven directly. `tripwire_issue_blocked_cancelled` asserts
`Blocked (1)` / `Cancelled (1)` / `Move to` (column headers + context menu, untouched);
`tripwire_issue_properties_render` asserts `◆ Sprint: S2` on the properties row, not the
card footer - verify the new footer glyph does not collide.
**Other tests to refresh:** all 13 card snapshots (`issue_board_snapshot`,
`snapshot_boards__*` x7, `snapshot_kanban_layout`, `autopilots_screen_snapshot`,
`skill_screen_snapshot`), `fleet.rs:5321/5325/5518` count-row assertions.
**Two chrome tests will FAIL and need a decision, not a mechanical update:**
`chrome.rs:~434` pins `footer_hints(IssueList).contains(("x","delete"))` (keep it, `x` is
in the 5) and `chrome.rs:~449` pins `("f","facets")` (drop the assertion, `f` moves to the
context menu and stays in `HELP_LINES`).

### Step 3 (M) - Inbox becomes the one attention surface

**Does:** section 2.4. Left list (needs-you + recent), right/inline answer pane lifted from
Control Center, client-side filters, failed-first ordering.
**Touches:** `inbox.rs` (324 -> ~650), reuse `control_center.rs:645` render + the answer
reducer verbatim, `plugin.rs` to feed `attention/list` into inbox state.
**Tripwires:** none of the seven.
**Other tests:** new `inbox.rs` render tests; `snapshot_control_center__*.snap` stay green
because Control remains a screen until step 5.
**Note:** this step unifies the SURFACE only. Do not merge the `attention` /
`fleet_confirm` / `fleet_action_receipt` stores (section 5).

### Step 4 (M) - Detail screen

**Does:** section 2.3. Live run card, one meta line, transcript pane wired to Q4's backfill,
activity pane, delete the floating facet panel.
**Touches:** `task_detail.rs` (header collapse + panes), `widgets/facet_panel.rs` (drop the
task-detail call site, keep the widget for the issue list), `widgets/transcript.rs`,
`widgets/jsonl_timeline.rs`.
**Tripwires - two bite here:**
- `tripwire_issue_acceptance_tick` (`tests/tripwire_issue_acceptance_tick.rs:560-597`)
  asserts `TARGET acceptance issue`, `Acceptance: 0/3`, `1/3`, `3/3`. Keep those literal
  substrings in the new layout.
- `tripwire_issue_properties_render` (`:577-599`) asserts `Props:`, `Meta:`,
  `◆ Sprint: S2`, `◆ Owner: amy`. Keep the Props/Meta row.
**Other tests:** `facet_panel_snapshot.rs`, `transcript_render_snapshot.rs`,
`pr_badge_snapshot.rs`, `transcript_reducer_test.rs`.

### Step 5 (S) - Tab strip, help, palette

**Does:** section 2.5. 16 tabs -> 7, 18 router keys -> 9, `Go:` palette family, Settings
`More screens` section, `HELP_LINES` rewrite, `Kanban` -> `Runs` label.
**Touches:** `chrome.rs:45 PRIMARY_TABS`, `router.rs:30 ROUTER_KEYS` + `:46 is_router_key` +
`:93 reduce_key` (delete 9 arms in lock-step), `app_screens.rs:1509 HELP_LINES`,
`command_palette.rs`, `settings.rs`, `docs/hangar/tui-keybindings.md`.
**Test changes, precisely:**
- `router.rs:218 router_keys_all_have_a_reduce_key_arm` - **passes unchanged** provided the
  const, `is_router_key` and `reduce_key` shrink together. It iterates `ROUTER_KEYS`, so a
  smaller set is a smaller loop.
- **Add** `palette_reaches_every_demoted_screen`: assert each of the 9 demoted `Screen`
  variants has a `Go:` palette entry. Without this, the shrink can strand a screen and no
  existing test notices.
- `app_screens.rs:2664 help_overlay_names_every_router_key` - passes with fewer keys. The
  `screens` HELP_LINES rows must still name all 7 survivors as `<key> <label>` pairs.
- `app_screens.rs:2749 help_overlay_screen_keys_are_not_router_keys` - the new `more` block
  lists `3 skills`, `U usage` etc. Those are no longer router keys, so `is_router_key`
  returns false and the test passes. Widen the `in_screens` skip to also match a first
  token of `more`, for intent.
- `app_screens.rs:2694 help_overlay_screen_rows_pair_every_label_with_a_key` - the `more`
  row is key/label pairs, passes.
- `app_screens.rs:2786 help_overlay_clips_to_a_short_pane_from_the_top` - counts non-blank
  lines, adjusts automatically.
- `chrome.rs:368 footer_hint_keys_never_collide_with_reserved_router_keys` - gets strictly
  looser (9 fewer reserved chars). Its screen list at `:378-389` enumerates `Screen`
  variants, which all still exist. Passes.
- `chrome.rs:~467-472` width test comments reference a "fifteen-tab strip"; update the
  comment and the width assertion (the 7-tab strip is 74 cols, so it now fits 80).
- `tests/screen_router_test.rs` - update any assertion that presses a demoted key.
- `tests/tripwire_hangar_plugin_connects.rs` - asserts connect state, no screen text. Green.

---

## 5. Recording plan

One vhs tape per step, before/after in the same gif, driven on the isolated instance the
proving run used. Reuse `scripts/hangar/record-fullstack-p1.sh` as the harness template (it
already bakes the iso `HOME`, `TMUX_TMPDIR`, `AINB_PLUGIN_ROOT`, `AINB_BIN` env and the
`Wait+Screen@60s /navigate/` gate). Output under `docs/hangar/proofs/crisp/`.

| step | tape | the shot that proves it |
|---|---|---|
| 1 | `c1-names.tape` | task detail showing `Agent: impl-1`, usage showing `impl-1` not the ULID, inbox showing `impl-1 done HGR-3 Add GET /api/version`, and a task-detail open that immediately paints a transcript |
| 2 | `c2-cards.tape` | one Issues board with an idle card, a running card (`◔ impl-1 · running 2m · PR ✓`) and an ASK card side by side; Boards with ONE hint bar |
| 3 | `c3-inbox.tape` | the full P3 human loop driven entirely from `I`: ASK appears in `needs you`, answer inline with `2`, row clears, `recent` gains the finished run |
| 4 | `c4-detail.tape` | `2` on a live run: run card ticking, transcript streaming, acceptance still reading `Acceptance: 0/3`, then the same screen after the run finishes |
| 5 | `c5-tabs.tape` | the 7-tab strip at 80 cols, then `^P` -> `usage` -> Usage screen, proving nothing was stranded |

Every tape ends with a `Screenshot`, so the operator judges crispness from a still and the
motion from the gif. Record green runs only, as the proving run did.

---

## 6. Do NOT attempt in this track

These depend on the spine work. Half-building any of them here creates a second thing to
unwind when ACP and the state collapse land.

1. **Collapsing issue / card / task state.** Four stores, synchronised by a fan of
   advance-only best-effort writes that log-and-swallow (`ainb-hangar-daemon/src/board.rs`
   x9 functions; exec-map section 3.2). The plugin must keep rendering three vocabularies
   until the daemon collapses them. Map them in `vocab.rs`, do not merge them.
2. **Any plugin-side write that is not an existing RPC**, and no polling to paper over
   defect 15 (CLI writes the store directly with no event, `cli/hangar/mod.rs`, 18 direct
   `Store::open_default()`, zero `HangarEvent`). A "refresh every N seconds" workaround
   hides the missing event and makes the single-writer fix harder to justify.
3. **Anything that touches the answer delivery path.** `attention/answer` ->
   `answer.rs:218 deliver` -> `capture_pane` + `send-keys` is load-bearing for correctness,
   with four documented refusal conditions and a live-probed picker behaviour that changed
   between Claude Code 2.1.257 and 2.1.258 (REPORT.md:72, :87). The Inbox in step 3 renders
   and raises the same `attention/answer` RPC the Control Center raises today, byte for
   byte. Do not add a "structured answer" affordance; that is ACP.
4. **A live transcript for interactive runs.** There is no capture on that path:
   `run_loop.rs:1559 run_interactive` produces no stream-json and `runner.rs:1327` sets
   `structured = false`. The tmux pane is the sole artifact. Render `attach with a` and stop.
5. **Unifying the attention / fleet_confirm / fleet_action_receipt stores.** Step 3 unifies
   the surface. Two parallel "a human must answer" mechanisms plus a third receipt table is
   a daemon-model problem (exec-map section 3.4).
6. **Acceptance criteria gating done.** A task finalises `done` with 0/3 ticked by design
   (REPORT.md:30). Render the ratio, do not enforce it.
7. **Board card authoring: brief stage, or create-without-dispatch** (defects 16, 17). Both
   need new RPC and brief-composition rules (`run_loop.rs:2548 build_prompt`).
8. **PR badge capture** (defect 11). `pr_url` is parsed daemon-side from the bounded stdout
   tail (`run_loop.rs:1708 parse_gh_pr_create_stdout`) and misses under stream-json. The
   plugin renders whatever `pr_url` arrives and renders nothing when it is absent. Do not
   synthesise a PR link from the branch name.
9. **Deleting `Screen` variants** for the nine demoted screens. Only the router key goes.
   The screens, their reducers, their snapshots and their tests all stay.
10. **Daemon log noise** (defect 13, `Codex managed transport degraded` every 16s). Daemon
    side. The Logs screen may add a rate-limit fold, but do not filter the row out.
