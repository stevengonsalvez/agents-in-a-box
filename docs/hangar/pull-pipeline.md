# Role-gated pull pipeline

**Status:** shipped. Schema from migration `0074_role_gated_pull_pipeline`.

Squads execute like an engineering team: work is PULLED stage by stage through
role-gated board columns by one owner at a time.

```
Backlog  ->  Triage  ->  Implement  ->  Review  ->   QA    ->  Done
  (-)       triager    implementer     reviewer    tester      (-)
 wip -        wip -       wip 2         wip 3       wip 1     wip -
                                      excl. prior  excl. prior
```

## What changed, and why

Squad dispatch used to BROADCAST: `assign_fanout` wrote the leader brief plus one
`agent_task_queue` task per distinct agent member, so one issue became N
simultaneous runs, each provisioning its own worktree for the same work and
racing the others onto branches off one repo. `SquadRepo::member_agent_ids` is
still documented "role-blind by design", which was exactly the problem: nothing
gated dispatch on role.

A squad dispatch now enqueues the card into the first role-gated column, and
exactly one eligible agent pulls it.

## How it works

```
board_card in a role-gated column        <- the pull queue
       |  PullService::pull_for_runtime
       v
agent_task_queue row, status 'queued'    <- the execution queue
       |  ClaimTaskService::claim_for_runtime  (unchanged)
       v
status 'dispatched' -> the runner
```

The daemon pulls once per tick, immediately before claiming. Pulling first means
a card enqueued by a tick is claimable in that same tick, so a handoff costs one
poll interval rather than two. A pull fault degrades the daemon to push-only
rather than downing the claim loop.

`agent_task_queue.agent_id` is `NOT NULL`, so a queued row always already names
its agent: the pre-existing model is PUSH. True pull needs the agent chosen at
claim time. Rather than make `agent_id` nullable (a full table rebuild on SQLite
against a populated database), the `board_card` row is the queue and the task row
is INSERTed once an eligible agent has been selected.

The whole selection-and-insert is ONE `INSERT ... SELECT ... RETURNING`. SQLite
serialises writes, so a concurrent puller's sub-select sees the committed row and
the one-owner guard excludes the card. Two agents can never take the same card,
across processes as well as across tasks.

## The predicates

A `(card, agent)` pair is pullable when ALL hold:

| # | Predicate | Meaning |
|---|---|---|
| 1 | Role gate | the agent HOLDS the column's `services_role` |
| 2 | WIP limit | the column is under `wip_limit` (NULL = unlimited) |
| 3 | One owner | the card has NO active task at all |
| 4 | Prior-agent | on `excludes_prior_agent`, no `done` task by this agent on this card |
| 5 | Blocked + capacity | no unfinished `card_dependency` blocker; agent under `max_concurrent_tasks` |

Ordering is `issue.priority DESC, column.ord DESC, card.ord, issue_id`: urgent
work first, then PULL FROM THE RIGHT (latest stage first), so the pipeline drains
rather than piling up mid-flight.

## Roles

An agent's roles come from `squad_member.role` (migration 0053). No agent-level
roles field exists; the existing writers are the way to grant one:

```bash
ainb hangar squad add-member <squad-id> --member agent:<agent-id> --role reviewer
ainb hangar squad member-role <squad-id> --member agent:<agent-id> --role "implementer,reviewer"
```

`role` is free text, matched as a COMMA-SEPARATED TOKEN set, case-insensitively
and ignoring spaces, so one membership can advertise several roles without a
second table. Matching uses `INSTR` over comma-delimited needles rather than
`LIKE`, so a role containing `%` or `_` cannot act as a wildcard.

Matching is token-exact: an agent holding `review` does NOT thereby hold
`reviewer`. A membership with an empty role holds nothing, so the pipeline is
opt-in.

## Getting started

```bash
ainb hangar pipeline init     # provision the six stages (idempotent)
ainb hangar pipeline show     # stages, roles, WIP caps, live card counts
```

`init` creates a NEW board named `Pipeline` rather than retrofitting an existing
one, so your current Kanban keeps working exactly as it did. Re-running never
rewrites a tuned WIP limit or a renamed stage.

Then dispatch as usual:

```bash
ainb hangar squad assign <squad-id> --issue <issue-id> --fanout
```

## Two behaviours worth knowing

**A stage with no eligible agent WAITS.** If only one agent exists and it
implemented the card, the Review stage does not claim it. The card sits visibly
queued rather than being self-reviewed. That is the intended direction of
failure.

**Every pipeline column has `fsm_state = NULL`, deliberately.** The pre-existing
`BoardRepo::auto_move_on_state` hook fires on every task FSM transition and moves
a card to the column whose `fsm_state` matches the new state. A pipeline column
carrying `fsm_state='done'` would jump the card straight there the moment the
FIRST stage completed, skipping Review and QA while appearing to work. NULL makes
that hook inert on these columns, while `auto_move = 1` keeps the per-column and
per-board kill-switch meaningful, because that is what the stage advance
consults. Turn `auto_move` off on a column to freeze cards there.

## Deliberate parallelism

Removing the broadcast did not remove intentional parallelism, it made it
explicit:

```bash
ainb hangar squad assign <squad-id> --issue <issue-id> --redundant 3
```

N independent attempts at one problem, or several reviewers over one artifact, on
up to N distinct squad agents. Every row is stamped with a shared
`agent_task_queue.run_group`, so "why are there three runs on this card" is
answerable from the row itself. `--redundant` is a ceiling, not a quota: asking
for more copies than there are eligible agents dispatches to all of them.

All copies share one generation, because they are one run. The card advances only
once the WHOLE cluster has drained, and a dependent stays blocked until then.

## Schema (migration 0074)

| Column | Meaning |
|---|---|
| `board_column.services_role` | the role gate. NULL = not a pull queue |
| `board_column.wip_limit` | max cards in this column holding an active task. NULL = unlimited |
| `board_column.excludes_prior_agent` | 1 = an agent with a `done` task on this card may not take this stage |
| `agent_task_queue.run_group` | clusters one deliberate `--redundant` fan-out |

Every default is inert. `services_role IS NULL` means "not a pull queue", so
every column that predates the migration keeps exactly its current behaviour and
no existing board changes.

`excludes_prior_agent` encodes the reviewer-is-never-the-implementer rule as
pipeline DATA rather than a hard-coded `services_role = 'reviewer'` string, so an
operator who names the stage "Peer check" still gets the guarantee, and a
one-agent deployment that genuinely wants self-review can express that by leaving
the flag off.

## Verifying it

The live proof drives one card through four stages across three real agents and
two real provider CLIs, asserting the database every 250ms:

```bash
cargo test -p ainb-hangar-daemon --features live-e2e --test live_e2e \
  live_pipeline_walks_four_stages -- --nocapture
```

It SKIPS CLEAN and loudly when a provider CLI is absent or unauthenticated. The
success signal is not the task status (a stub exits 0 by construction): each
stage must leave an unguessable nonce in its OWN worktree and record non-zero
token usage.

Unit and integration coverage:

- `ainb-hangar-store/tests/pull_role_gate.rs` (the predicates, each with a
  mutation proof that deletes exactly its clause and asserts the forbidden pull
  then succeeds)
- `ainb-hangar-store/tests/squad_no_broadcast.rs` (N members yield 1 run;
  `--redundant`; whole-cluster blocker drain)
- `ainb-hangar-store/tests/migration_0074_upgrade.rs` (the migration is inert on
  a populated database)
