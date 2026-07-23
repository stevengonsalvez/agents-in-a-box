# Squad entity: Multica vs Hangar

## 1. Multica Squad

### Schema (server/migrations/084_squad.up.sql:2-33, +085/086/087/088)

```
squad
  id, workspace_id, name, description, leader_id (→agent, RESTRICT),
  creator_id, created_at, updated_at
  UNIQUE(workspace_id, name)            -- 087 later DROPS this uniqueness
  + avatar_url (086), instructions TEXT DEFAULT '' (088), archived_at/archived_by (085)

squad_member
  id, squad_id (→squad CASCADE), member_type CHECK IN ('agent','member'),
  member_id, role TEXT DEFAULT '', created_at
  UNIQUE(squad_id, member_type, member_id)

issue.assignee_type CHECK IN ('member','agent','squad')   -- squad is a first-class assignee
autopilot.assignee_type CHECK IN ('agent','squad')          -- 096, autopilot can target a squad too
agent_task_queue.squad_id                                    -- 127, no FK (hot table), lets the
                                                              -- daemon inject the right briefing at claim time
```

Leader must be an agent (FK RESTRICT to `agent`, never `member`). Members can be `agent` or `member` (human). A squad with only the leader is valid.

### Leader/member model, roles (server/internal/handler/squad.go)

- `CreateSquad` (squad.go:225-318): any workspace member can create a squad → becomes `creator_id`. Leader must be an agent in-workspace, and a non-admin creator can only wire an agent they can themselves `@`-trigger (`memberCanWireAgent`, squad.go:134-140) — stops smuggling an inaccessible agent into a squad. Leader auto-added as member with `role="leader"` (squad.go:297-303).
- Management is **creator-scoped**: owner/admin manage every squad; a regular member manages only squads they created (`canManageSquad`, squad.go:118-123). Visibility is workspace-wide regardless (`ListSquads` unfiltered).
- `AddSquadMember` / `RemoveSquadMember` / `UpdateSquadMemberRole` (squad.go:690-881): role is a free-text label ("owns the migrations") the leader reads when delegating. Leader cannot be removed (must change leader first, squad.go:810-813).
- `ListSquadMemberStatus` (squad.go:579-688): derives a per-member `working/idle/unstable/offline/archived` status from runtime + active-task rows — squad members get live presence in the UI.
- Docs confirm (`apps/docs/content/docs/squads.mdx:12`): "members can be agents **or human members**" — humans are first-class squad members, they just can't be leader and don't get dispatched-to by the fan-out mechanism (they're delegation *targets* via @mention only, handled by the normal comment/mention pipeline, not a special squad-fanout).

### Assignment + briefing/kickoff fan-out

Key design: **a squad-assigned issue enqueues ONE task — for the LEADER only** (`enqueueSquadLeaderTask`, squad.go:1027-1087). There is no member fan-out at assignment time in multica — that's the crucial difference from hangar (see §3). The leader is the sole dispatch target; it then *delegates by posting a comment that @mentions members*, which independently triggers each mentioned member through the ordinary mention-trigger pipeline.

On claim, `buildSquadLeaderBriefing` (squad_briefing.go:164-177) appends 3 sections to the leader's system instructions:

1. **Squad Operating Protocol** (squad_briefing.go:23-144, hard-coded, not user-editable): explains the leader's coordinator role, 6 numbered responsibilities —
   1. Read the issue, pick the best member by matching to their listed **skills**/role.
   2. Delegate by @mention — exact `[@Name](mention://agent|member/<uuid>)` markdown, terse (don't restate issue body).
   3. Record evaluation via `multica squad activity <issue-id> <action|no_action|failed> --reason "..."` — mandatory every turn.
   4. Stop after dispatching — no implementation work.
   5. Re-evaluate on each re-trigger (member reply, mention, etc).
   6. **Own parent issue status** — but ONLY if this squad actually owns the issue (`ownsIssueStatus` flag, squad_briefing.go:73-105): move to `in_progress` on first turn, `in_review` when overall goal met, never `done` (left to human/PR-merge). If the leader was pulled in via an `@squad` mention on someone else's issue, responsibility 6 flips to "do NOT change status."
   Hard rules section (squad_briefing.go:107-134) reiterates: mention markdown is mandatory (no plain "@name"), don't @mention non-members, one delegation comment per turn, don't double-fire (assignment-trigger vs mention-trigger collision warning).
2. **Squad Roster** (`buildSquadRoster`, squad_briefing.go:181-227) — leader self-row + one row per non-archived member, each with literal ready-to-paste mention markdown and (for agents) their **skill names** (`agentSkillsRosterSegment`, squad_briefing.go:299-307) so the leader can match capability→task. Archived agents are silently skipped.
3. **Squad Instructions** (squad_briefing.go:170-175) — the user-authored `squad.instructions` field (routing rules, escalation policy), omitted entirely if blank.

### Coordination / result-aggregation back to leader

There is no explicit "aggregation" step — coordination is entirely comment-driven:
- Leader posts delegation comment(s); each mentioned member gets its own task via the ordinary mention-trigger machinery (shared with direct agent mentions).
- Leader is **re-triggered** on: a non-member comment, a squad member's progress update with no @mention (re-evaluate), an issue cross-reference-only comment. Leader is **not** re-triggered on: its own comment (`shouldSuppressSquadLeaderSelfTrigger`, squad.go:985-1002), or when anyone else's comment explicitly @mentions someone else (that mention *is* the routing signal — no double-trigger).
- `squad_no_action.go` — `HasSquadLeaderNoActionEvaluationForTask` dedups so a `no_action` evaluation isn't recorded twice for the same task.
- `RecordSquadLeaderEvaluation` (squad.go:888-981) — CLI-driven activity-log entry (`squad_leader_evaluated`), gated so **only the squad leader agent** can record it for an issue actually assigned to that squad (security check squad.go:923-930), tied to the exact task via `X-Task-ID` header.

### Create-inputs for a squad

- `CreateSquad`: `name` (required), `description` (optional), `leader_id` (required, must resolve to an in-workspace agent), `avatar_url` (optional).
- `UpdateSquad`: any of `name`, `description`, `instructions`, `leader_id` (re-validates + auto-adds new leader as member), `avatar_url`.
- Member add: `member_type` (`agent`|`member`), `member_id`, `role` (free text).
- Squads are also assignable to **autopilots** (096 migration) — Path A "squad-as-leader": autopilot dispatch resolves squad→leader.leader_id, same semantics as manual assign.
- Archive (`DeleteSquad`, squad.go:423-481): transfers assigned issues AND autopilots to the leader agent (so nothing goes silent), then soft-deletes (`archived_at`/`archived_by`); rejects new assignments to an archived squad.

---

## 2. Hangar Squad (ainb)

### Schema (crates/ainb-hangar-store/migrations/0017_squad.sql, 0035_card_squad_assignment.sql)

```
squad
  id, workspace_id (→workspace), name,
  leader_type CHECK IN ('member','agent'), leader_id, created_at
  UNIQUE(workspace_id, name)

squad_member
  squad_id (→squad), member_type CHECK IN ('member','agent'), member_id
  PRIMARY KEY (squad_id, member_type, member_id)      -- no role column, no created_at

issue.squad_id  TEXT   -- nullable, no FK, orthogonal axis to issue.agent_kind / repo_ref (0035)
```

No `description`, no `instructions`, no `avatar_url`, no `archived_at`, no per-member `role` column, no `creator_id` — a much thinner shape than multica's fully-evolved (7-migration) squad. `leader_id` has no FK (SQLite `PRAGMA foreign_keys` off in this crate by convention).

### Service layer (`SquadAssignService`, crates/ainb-hangar-store/src/service/squad_assign.rs)

Two operations:
- `assign_to_leader` (lines 156-203): resolves squad→leader agent→leader's runtime, enqueues ONE task keyed to `(leader_agent_id, leader_runtime_id)`. This mirrors multica's leader-only assignment exactly.
- `assign_fanout` (lines 240-347): **hangar goes further than multica here** — it fans a card/issue out to the LEADER **and** every distinct `agent` member (human members skipped, no runtime to route to) in a single all-or-nothing transaction, each member getting its own task on the same issue, each provisioning its own worktree (`repo_ref`/`agent_kind` stamped per task, tcp T4/F7). Cross-workspace member refs are rejected (`agent_runtime_in_ws`, lines 379-391); dangling member refs abort the whole fan-out atomically (tested at lines 738-788); leader-listed-as-member is deduped (lines 690-736).

This is architecturally the *opposite* shape from multica: hangar dispatches leader+members concurrently up front; multica dispatches leader-only and lets the leader's own @mention comment trigger members one at a time. Hangar's fan-out has no analogue to multica's "leader decides who, and only who's needed" routing — every agent member gets a task regardless of fit.

### What's missing: briefing / kickoff / roster / instructions

Grep across `ainb-hangar-store` and `ainb-hangar-daemon` for `briefing|roster|instructions` (scoped to squad code) returns **nothing** outside the doc-comment in `squad_assign.rs`. There is no equivalent of multica's `buildSquadLeaderBriefing`:
- No system-prompt injection telling the leader "you are a squad LEADER, delegate by @mention, don't do the work yourself."
- No Squad Roster block with ready-to-paste mention markdown or skill names.
- No per-squad `instructions` field to even hold user-authored routing guidance (schema doesn't have the column).
- Nothing analogous to `squad_no_action.go` / `RecordSquadLeaderEvaluation` — no evaluation/audit trail of what the leader decided each turn.

The `agent_task_queue.squad_id` column multica added specifically so the daemon could inject briefing content at claim time (migration 127 comment) has **no hangar equivalent** — hangar's task rows carry no `squad_id`, so even if briefing were added, there's no claim-time hook to key it off today (a task only knows its own `agent_id`/`issue_id`).

Net effect: a hangar squad leader that claims a fanned-out task receives **no instruction that it's a leader**, no roster, nothing telling it to coordinate rather than just do its own slice of work directly. Since hangar's fan-out already dispatches to every agent member concurrently (unlike multica's leader-mediated one-at-a-time), the "leader" role in hangar today is nominal — it gets a task like everyone else, with no special system prompt distinguishing it.

### TUI surface (`crates/ainb-plugin-hangar/src/screen/squads.rs`)

Pure reducer + width-aware render, hotkey `S` for the Squads screen:
- `n` create an agent inline (helps clear the "no agent available" gate), `c` create a squad (name-only, leader chosen by the glue from cached agents — no leader picker UI, no description/instructions/avatar inputs), `a` add a member (glue auto-picks "next cached agent not already on the squad" — no member picker, no role input), `d` remove selected member row, `x` assign current issue to squad (fires `squad_fanout`).
- No archive/delete action exposed in this screen at all (schema has no `archived_at` to archive into anyway).
- Renders leader + members with live presence dots; member rows tagged `agent`/`human`.

### Known bug: issue #450 — Boards' `q:squad` hotkey unreachable

Confirmed via `gh issue view 450` (OPEN): Boards screen advertises `q:squad` in its footer to open the assign-squad picker on a focused card (`BoardsEvent::AssignSquad`), but the **global key router** (`ainb-plugin-hangar/src/plugin.rs::on_key` → `routing_event`) matches bare `'q'` unconditionally as quit for *every* screen, before `boards.rs`'s screen-local `'q' → AssignSquad` mapping is ever reached. Root cause: no "Boards screen, no overlay open" guard exists alongside the other screen-specific capturing-input guards (Squads create-input, Boards overlay input) that already protect their own keys from the router. Reproduced via tmux: pressing `q` on a focused, unrun card closes the whole Hangar panel back to Sessions; the card persists with `squad_id = NULL`. Impact: **card-level squad fan-out is unreachable via keyboard** — the only working path is the CLI `ainb hangar squad assign <squad_id>`, which is leader-only (no fan-out), not equivalent to the card's documented fan-out semantics.

---

## 3. GAPS

| # | Multica has | Hangar has | Gap | Effort |
|---|---|---|---|---|
| 1 | Leader system-prompt briefing (Operating Protocol + Roster + Instructions) injected at claim time, keyed via `agent_task_queue.squad_id` | Nothing — no briefing text, no injection hook, task rows carry no `squad_id` | **Biggest gap.** A hangar squad leader has no idea it's a leader or who's on the roster; it just runs like a solo agent. Needs: `squad_id` column on hangar's task table (mirroring migration 127), a `SquadBriefing` builder, and a claim-time injection point in the daemon (`ainb-hangar-daemon`) | L |
| 2 | Per-member free-text `role` column, surfaced in the roster so the leader matches task→member by fit | `squad_member` has no `role` column at all | Leader (once briefing exists) can't route by stated specialty; TUI `a` add-member also has no role input | M (schema + repo + TUI input) |
| 3 | User-authored `squad.instructions` field (routing rules, escalation policy) | No `instructions` column | Even with briefing, no per-squad custom guidance channel | S (schema + briefing template once #1 lands) |
| 4 | Leader-mediated, **selective** dispatch — leader reads issue, picks the *right* member(s) by skill/role, delegates via one @mention comment; members not needed get no task | `assign_fanout` dispatches to **every** agent member unconditionally, concurrently, no selection logic | Hangar's fan-out is "spray to all," not "route to the fit one." Architecturally this may be an intentional different pattern (parallel worker fan-out vs mediated routing) rather than a straight gap — worth a product decision before "fixing" | L (if selective routing is wanted, needs briefing + comment-driven re-trigger machinery, effectively porting multica's whole coordination model) |
| 5 | Human (`member`) squad members are legitimate delegation targets (via @mention, same as agents) | Human members are stored (`member_type='member'`) but structurally **excluded** from fan-out (no runtime → skipped) and there's no comment/mention system to delegate to them another way | Human squad members are currently inert in hangar — schema allows adding them but nothing routes work to them | M/L (depends on whether hangar has any comment/mention pipeline to hook into — needs investigation) |
| 6 | Evaluation/audit trail per turn (`squad_leader_evaluated` activity, `no_action` outcome, dedup) | None | No visibility into whether/why a leader acted; no dedup mechanism for repeat evaluations | M |
| 7 | Result-aggregation is comment-driven: leader re-triggered on member replies, re-evaluates, decides next step or marks `in_review` | No re-trigger/coordination loop — hangar's fan-out is fire-and-forget, no mechanism for the leader to see member results and react | Once dispatched, hangar has no loop closing the leader back in when members finish — needs a trigger/wake mechanism (comment system or task-completion webhook) | L |
| 8 | Squad is soft-deletable (`archived_at`/`archived_by`), issues/autopilots auto-transferred to leader on archive, rejects assignment to archived squad | No archive concept in schema or TUI at all | Deleting/retiring a squad has no safe path; stale squads accumulate, no transfer-on-delete safety net | S/M |
| 9 | Squad assignable to **autopilot** (`autopilot.assignee_type='squad'`) | No autopilot-squad linkage found | Squads can't be targeted by hangar's automation/autopilot equivalent (if one exists) | Unclear — needs autopilot-equivalent audit first |
| 10 | Card-level squad-fan-out reachable via keyboard (assign-squad picker) | **Broken**: `q:squad` hotkey stolen by global router (issue #450, OPEN) — fan-out only reachable via CLI (leader-only, not fan-out-equivalent) | Immediate, scoped, well-understood regression — fix before any of the above | **S** (add a Boards-no-overlay guard in `plugin.rs::on_key`, same pattern as existing screen guards) |

**Priority read:** #10 is a quick, isolated fix and should land first (it blocks the *existing* fan-out feature from being reachable at all). #1 (briefing) is the structural gap that most changes what a hangar squad leader actually *is* — everything else (role column, instructions, evaluation trail) is downstream of having a briefing mechanism to put them in. #4/#5 are product-shape questions (selective routing vs spray-fanout; human members) worth a decision before investing further engineering.
