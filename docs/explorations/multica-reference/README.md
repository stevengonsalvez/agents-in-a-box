# Multica ↔ Hangar Parity Reference

The ground-truth reference for how far Hangar (ainb's control plane) is from
Multica's behavioral model, entity by entity. Drive the roadmap from this
README; dive into the per-entity files for field-level detail, schema
citations, and `file:line` references.

## Purpose & how to use

- **What this is:** a durable, verify-against-it map of what Multica does that
  Hangar does not (and the handful of things Hangar already does *better*).
  Each entity file was produced by a deep-dive over both codebases and carries
  full field tables, migration references, and source citations.
- **Source of the reference:** Multica — `github.com/multica-ai/multica`,
  fair-source Apache-2.0. This is a **behavioral reference, not a verbatim
  port**: read it for the *shape* of a feature (what fields exist, what the
  dispatch predicate decides, how a trigger fans out), then re-implement in
  Hangar's Rust/SQLite idiom. Do not copy source line-by-line.
- **The north star:** Multica's organizing principle is **"agents are team
  members, not tools."** Concretely that means a *polymorphic actor* model —
  every "who did this" column is `actor_type ('member'|'agent') + actor_id`, so
  agents own issues, post comments, receive inbox items, and get @mentioned
  exactly like humans. Most of Hangar's biggest gaps trace back to it being
  **agent-centric** (agents are the thing that does work) rather than
  **actor-polymorphic** (agents and humans are symmetric collaborators). When
  ranking work, "does this move Hangar toward actor symmetry?" is the tie-breaker.

## Entity files

| File | Entity | Rough parity | Headline gap |
|---|---|---|---|
| [issue.md](issue.md) | Issue | ~35% | No comment @mention dispatch; no subtasks |
| [squad.md](squad.md) | Squad | ~40% | Leader gets no briefing — "leader" is nominal |
| [workspace.md](workspace.md) | Workspace / membership | ~40% | Single singleton workspace; no human members |
| [agent.md](agent.md) | Agent | ~55% | No 2-dim presence; no agent builder |
| [autopilot.md](autopilot.md) | Autopilot | ~95% | #27 subscribers/collaborators (deferred while solo) |
| [task-flow.md](task-flow.md) | Task + dispatch flow | ~80% | No dispatch reason codes; no activity log |

Parity % is a rough eyeball, not a measured metric — it exists to signal where
the structural holes are (Issue/Squad/Workspace), versus where Hangar is already
close (Task-flow, Autopilot, where it is *ahead* on some axes).

> **These figures are the ORIGINAL 2026-07-23 assessment, kept as the baseline.**
> For where Hangar stands today see [Parity status
> (2026-07-27)](#parity-status-2026-07-27) immediately below; the master gap
> matrix and roadmap further down are likewise the original reference, not a
> current to-do list.

## Parity status (2026-07-27)

An audit of the parity-closure campaign, verified against **primary evidence on
`main`** (migrations, repo/daemon/plugin source, CI job conclusions) rather than
against the campaign's own PR descriptions. Where a gap was deliberately scoped
narrower than the reference's full facet, that is stated precisely — a
partially-closed gap is **not** counted as parity.

### Verification caveat — RESOLVED 2026-07-27

**The 2026-07-24 audit's headline caveat no longer holds.** It said the
authoritative end-to-end gate had been red on `main` for the entire campaign, so
"no gap has a green authoritative CI proof" and nine merged features were
*CI-unproven*. That has been repaired. What changed, and the primary evidence:

| Was (2026-07-24) | Is now (2026-07-27) |
|---|---|
| `Test (ubuntu/macos)` ran `cargo nextest run --lib` — every `tests/` integration proof invisible | Runs `cargo nextest run --lib --tests --all-features --no-fail-fast -E 'not binary(/^tripwire_/)'` (`ci.yml:106`) — the `rpc_*`, `repo_*`, `migration_*` proofs now build and run on BOTH OS legs |
| `hangar-e2e (ubuntu-latest)` failed on every PR #460–#471 (3 pre-existing tripwire failures) | **success** — full 34-tripwire matrix green |
| `Run hangar acceptance tests (framed-socket + CLI)` was `skipped`, so those proofs had never run in CI at all | **success** — `run_acceptance_tests.sh` auto-globs every non-`tripwire_*` integration target in the three hangar crates, so new acceptance tests are gated without editing the script |
| `Rustfmt` red on `main` for toolchain drift | **success** |

Primary evidence: CI run **30301591584** on `main` (HEAD `f407f3d4`), all nine
jobs `success` — `Rustfmt`, `CLI reference freshness`, `Test (ubuntu-latest)`,
`Test (macos-latest)`, `hangar-e2e (ubuntu-latest)`, `hangar-e2e (macos-latest)`,
`ainb-hooks` ×2, `Cargo Machete`. Per-step conclusions on the ubuntu
`hangar-e2e` job confirm both `Run all hangar tripwires` and `Run hangar
acceptance tests (framed-socket + CLI)` ran and passed (checked per-job and
per-step, not on the workflow's rolled-up conclusion).

**Two honest residual caveats.** (a) `hangar-e2e (macos-latest)` still runs a
launch-smoke SUBSET (`HANGAR_TRIPWIRE_SMOKE`), not the full 34 — the hosted
macOS runner cannot finish the serial tmux matrix even at 3× budget, so the
Linux leg is the authoritative one. (b) The `ainb` crate's other integration
targets (`tests/ui_tests.rs`, `tests/behavioral/`) carry pre-existing
`NewSessionState` compile drift, so `run_acceptance_tests.sh` names the two
hangar targets in that crate explicitly rather than globbing it. Neither
weakens the hangar proofs; both are stated so nobody reads "all green" as "all
of `ainb` is gated".

Rows below still marked *CI-unproven* were written under the old regime — the
tests they name are now executed by one of the repaired jobs unless the row says
otherwise.

### Closed / claimed-closed gaps (2026-07-24 audit detail, retained)

This table is the 07-24 audit's per-gap forensic detail — kept because it is
where each **deliberate divergence** from multica is written down. Four of its
rows have since been extended and should be read together with the
finish-run table immediately after it: **#1** ("no activity-log actor, gap #13
unstarted") is superseded by #13 and 1‑rest; **#2** ("missing: the
`mention://type/id` link form, member mentions, fallback routing, outcome codes,
preview") is superseded by 2‑rest; **#6** ("availability is *not* derived,
`unstable` unreachable") is superseded by 6‑rest; **#9**'s missing
`kind`/`system_key` columns are supplied by #23. Every *CI-unproven* marker
below predates the gate repair described above.

| # | Gap | PR | Merged | Scope actually closed vs the reference's full facet | Verified? |
|---|---|---|---|---|---|
| **1** | Polymorphic actors + human members | #460 | ✅ | **Partial.** The `(actor_type, actor_id)` substrate already existed pre-campaign (`hangar-core/src/actor.rs`, landed in the P0.6–P0.7 scaffold) and is used by `issue`, `comment`, `squad`, `task`. PR #460 (+497/−25, 4 files, **no migration**) added the missing piece the reference flagged: a path that actually **mints a second human** — `MemberRepo::add`, the `hangar member` CLI, an RPC handler, sidebar render. **Not closed:** `inbox_entry` (mig 0021) has no actor/recipient column at all — it is workspace-wide, so there is no per-human or per-agent inbox; and there is no activity-log actor (gap #13 unstarted). | Partial. PR states the TUI half was "verify-only" and the Part-B tmux tripwire was **descoped**. No test files in the diff. |
| **2** | Comment @mention auto-dispatch | **not a campaign PR** — landed pre-campaign in **#250** (commits `2fde74c7`, `946de10c`, `69945d9e`) | ✅ | **Partial (~30%), and the reference's own premise was wrong.** The matrix below says "Comments are inert text — zero side effects on write"; that is **false on `main`**. `daemon/src/mentions.rs::parse_mentions` scans bare `@handle` tokens, `rpc/snapshots.rs:1983::spawn_mention_tasks` resolves them against workspace agents and enqueues one task each (sharing a run generation), coalescing duplicates on the per-`(issue,agent)` unique index; wired at `comment_add` (`rpc/mod.rs:3722`). **Missing:** the `mention://type/id` link form, **member (human) mentions**, reply-parent / thread-owner / assignee-fallback routing, surfaced per-target outcome codes (`queued\|coalesced\|deferred\|blocked` — coalescing is silent), preview, and self-loop suppression. The gap-#8 private-agent gate **is now applied on this path** (8-rest follow-up): a mention of an agent the comment's author may not invoke spawns nothing, per-target. | ✅ In-source `#[cfg(test)]` tests (`snapshots.rs:2156+`) — genuinely run by the green `--lib` job. |
| **3** | Sub-issues: parent/child + stage barriers + child-done cascade | #463 | ✅ | **Closed as scoped.** Migration `0046` (`parent_issue_id` self-FK `ON DELETE SET NULL`, `stage INTEGER CHECK(>=1)`, `idx_issue_parent`); `store/src/service/child_done.rs::cascade_child_done`; `IssueRepo::list_children` + `IssueRepo::child_progress` roll-up; wizard carries `parent_issue_id` with a read-only `Sub-issue of …` banner. Reference facet **now matched (3-rest)**: multica's **batched multi-stage aggregation**. Migration `0065` adds the `issue_cascade_barrier` claim ledger (PK `(parent, stage_key)`); `closed_barriers` replaces the single-frontier stage check with a pure function of the FINAL sibling set returning every closed stage prefix (the MUL-4155 order-independence fix — a stage that finished early no longer has its close dropped forever); `cascade_children_done` claims the barriers and writes ONE aggregated comment per parent in the same transaction. Reached from `hangar/issues_batch_update` and `hangar issue batch-state`, and `issue create --stage` finally makes a staged sibling set authorable. Hangar deviation, deliberate: multica dedupes implicitly at the HTTP request boundary; hangar has no request boundary on the agent-completion path, so the dedupe is a DURABLE sqlite invariant instead — strictly stronger, and it makes the acceptance a `count(*)` assertion. **Still not matched:** no TUI multi-select / bulk state keybinding, so there is no in-product batch producer yet. | *CI-unproven.* Added `tripwire_hangar_subissue_cascade.rs` + 20 other `tests/` files — none executed by any green CI job. PR reports live tmux confirmation. |
| **4** | Multi-workspace create / delete | #465 | ✅ | **Closed as scoped.** `WorkspaceRepo::create` + `::delete` with `validate_slug` (reserved-slug validation present), CLI, TUI settings-screen switch, `workspace_multi_create_isolation.rs` proof. Reference facet **now matched (4-rest)**: the per-instance creation-lockdown flag — `daemon_config: workspace.creation_disabled` (+ the one-way `HANGAR_DISABLE_WORKSPACE_CREATION` env override), gated inside `WorkspaceRepo::create` so a locked instance writes nothing. PR itself notes live pushed events still target the old workspace after a switch until re-subscribe. | *CI-unproven* (tests in `tests/`). PR reports a tmux-verified end-to-end run after a fix cycle. |
| **5** | Task-level `squad_id` + claim-time briefing hook | #461 | ✅ | **Closed exactly as scoped — deliberately the column + hook POINT only, not the briefing BODY** (that was gap #7). Migration `0045` adds `agent_task_queue.squad_id`; the daemon stamps and reads it at claim. | ✅ Strongest evidence in the campaign: a `run_loop` test drives the **real** `execute_claimed` and asserts the hook line carries both `task_id` and `squad_id`, **mutation-verified** (neutering the call site turns it red). |
| **6** | Two-dimensional derived presence | #466 | ✅ | **Partial — 1 of 2 dimensions.** *Workload* is genuinely derived: `TaskRepo::live_workload_for_agent` → `Workload::derive` from live running/queued counts, batched in `agents_list`. *Availability* is **not** derived: `presence_from_status` (`snapshots.rs:767`) is a straight passthrough of `agent_runtime.status`, and **nothing anywhere in the tree ever writes `"unstable"`** — the wire variant `PresenceState::Unstable` and its amber dot are unreachable. `agent_runtime.last_seen_at` is stored but never folded, so the reference's **5-minute unstable grace window does not exist**. Availability remains the same binary online/offline the reference called out. | Workload: repo + rpc tests exist (in `tests/`, *CI-unproven*). Availability grace: nothing to verify — not implemented. |
| **7** | Squad leader briefing (Operating Protocol + Roster + Instructions) | #467 (+ 7‑rest #486) | ✅ | **Closed as scoped, with two divergences documented in-source** (`daemon/src/squad_briefing.rs`): (a) roster rows are `name — <agent\|human> — <id>` rather than multica's `[@Name](mention://<type>/<uuid>)`, because mention-**by-link** does not exist (gap #2 only parses bare handles); (b) the roster's skills segment advertises what the member will actually MATERIALISE — both `agent_skill.enabled` (mig 0051) and `agent.disabled_runtime_skills` are applied — where multica reads one live tool registry. The briefing is now the full three sections: protocol + roster (each member's `role` and skills) + `## Squad Instructions`, each fragment blank-omitted. Injected pre-spawn at claim (`run_loop.rs:859`). | *CI-unproven.* An extended claim tripwire asserts the on-disk `CLAUDE.md` contains the protocol/roster and that a member task's prompt has none, **RED-verified against the old no-op**. No test files in the PR diff (test edits are in-crate). |
| **8** | Agent invocation permissions | #468 (+ 8-rest) | ✅ | **Closed: the gate now wraps every enqueue.** Migration `0047`: `agent.permission_mode` (`private\|public_to`, CHECK) + `agent_invocation_target` allow-list + a lossless backfill from legacy `visibility`. `AgentRepo::can_invoke` implements the truth table (owner always admits; admin does not bypass private). Enforced on **all three dispatch paths** as of the 8-rest follow-up: the single-agent card-run enqueue, the SQUAD fan-out (`store/src/service/squad_assign.rs` gates the leader AND every member in the pre-flight resolve, before the transaction opens, so a refusal writes zero rows — the single seam that closes `run_card`, both squad RPCs and the CLI at once), and the `@`-mention dispatch (`rpc/snapshots.rs::spawn_mention_tasks`, gated FIRST per target for multica's enumeration-safety, per-target `continue` so one denied handle never suppresses the others). `SquadAssignParams` / `BoardCardRunParams` gained an append-only `invoker_user_id`. **Still not matched:** per-target outcome codes (gap #2), and originator resolution for true A2A attribution (multica 184/185) — an agent-authored mention passes `None` and fails closed except against a `public_to workspace` target. The `team` target_type passes the CHECK but no team table exists. | ✅ `can_invoke_truth_table` is an in-source test — genuinely run by the green `--lib` job. CLI surface (`hangar agent permission\|allow\|can-invoke`) reported tmux-provable. |
| **9** | Conversational Agent Builder | #469 | ✅ | **Partial by explicit design.** Ships the guided structured-draft wizard — `CreateDraft` in `plugin-hangar/src/screen/agents.rs`, provider/model/instructions/name collected across steps, reviewed on a confirm step, Enter creates. The module docstring states it is "multica's chat → structured-draft → confirm builder, **minus the LLM turn**". The **conversational half is not built**: there is no hidden `kind='system'` builder agent proposing the draft, and the `kind`/`system_key` columns it needs (gap #23) do not exist. | Draft/step reducer covered in-crate. The LLM turn is untestable because it is absent. |
| **10** | Structured / faceted issue filtering | #470 | ✅ | **Closed as scoped.** No migration and no new wire field — filtering is client-side over columns `IssueRow` already carries (`state`, `priority`, `labels`, `assignee`, `due_date`). `FacetFilters` (`plugin-hangar/src/screen/issue_list.rs`) mirrors multica's `issueTableQuerySpec.Filters`: multi-select per dimension, **OR within a facet, AND across facets**, with per-value drill-down counts computed with that facet's own selection removed (`without_kind`, 1:1 with multica's `issueTableQueryWithoutFacet`). **Reference facets not matched:** `Creators[]`, `ProjectIDs[]`, custom-`Properties` (unblocked by gap #17's wire fields; the facet itself is filed as `10-rest`), 2-level grouping, cursor pagination, `Scope{kind, Relation, Actor}` involves-filters, and the separate server-side `ListIssueTableFacets` endpoint — hangar computes facets client-side over the already-delivered `hangar/issues_list` snapshot, a deliberate architecture difference for a single-user local control plane. | ✅ **CI-gated on both OS legs.** `tripwire_issue_facets` (real plugin behind the real SDK server, one survivor among four decoys, every `DECOY` asserted ABSENT) runs in `run_all_tripwires.sh`; `rpc_issue_facets_sqlite` proves the same intersection against a REAL daemon + REAL sqlite and cross-checks the reducer's visible-row count against a raw SQL `COUNT(*)` (mutation-verified: neutering the facet toggles turns it red). The same PR closed the gate hole that let `ainb-plugin-hangar`'s ~35 non-`tripwire_*` integration targets be compiled by NOTHING in CI. |
| **11** | Acceptance criteria + context refs | #471 | ✅ | **Closed as scoped.** Migration `0048` adds `acceptance_criteria` and `context_refs` as JSON-array TEXT columns defaulting `'[]'` (same persistence shape as `labels`, mig 0014); repeatable `--acceptance` / `--context-ref` CLI flags; wizard authoring; detail-card render. **11‑rest closed:** migration `0054` promotes the column to the reference's structured shape — each criterion carries a stable `ac-…` id and a checked bit with `checked_at`/`checked_by` provenance. `hangar issue criteria list|check|uncheck` (by id or 1-based ordinal) and the `hangar/issue_criterion_set` RPC tick one off; the detail card renders `Acceptance: n/m` with ☑/☐ and binds `a`/`t`. The decoder accepts the legacy flat array, so the upgrade needs no flag day. | *CI-unproven* (tests in `tests/`). PR reports a CLI + sqlite acceptance run. |
| **24** | Per-agent skill enable/disable toggle | #482 | ✅ | **Closed as scoped.** Migration `0051` adds `agent_skill.enabled` (INTEGER 0/1, DEFAULT 1, partial index on the enabled links) and `agent.disabled_runtime_skills` (JSON-array TEXT, multica 206). `SkillRepo::set_enabled` / `agent_skill_links` are the new levers; `skills_for_agent` now filters `enabled = 1`, so `daemon/src/materialise.rs` — the single materialisation seam — never writes a disabled skill's directory. Wire: `hangar/skill_set_enabled` + `hangar/agent_skills_list`. Drivable from `ainb hangar skills attach\|detach\|toggle` + `skills list --agent`, and from `t` on the skill-manager screen. **Three deliberate divergences, documented in-source:** *D1* — `disabled_runtime_skills` is honoured at dispatch-time materialisation, not at a live tool registry (hangar has none); same observable outcome. *D2* — `attach` keeps `ON CONFLICT DO NOTHING` and never re-enables a disabled link, because seed/`templates use` re-attach on every re-run and would otherwise silently undo an operator's disable. *D3* — `used_skill_ids` (the `Used`/`Unused` chips) stays attachment-based, so a disabled link still reads `used`. | ✅ Mutation-verified: neutering `AND a.enabled = 1` turns three `materialise_skills_tests` cases RED. Plus a populated-DB upgrade test (a link written before the column existed backfills enabled and still materialises), a daemon RPC test asserting persistence by reading the daemon's own sqlite file, and a CLI e2e through the real binary. |

### Closed in the 2026-07-26 → 27 finish run

Every row re-derived from **primary evidence on `main`** (`gh pr view` for state
and merge SHA, then the migration file / RPC method constant / module that the
feature must have in order to exist). The campaign's own tracking sheet is *not*
the source — one entry in it was wrong and is corrected below.

| # | Item | PR | Merge SHA | Primary evidence on `main` |
|---|---|---|---|---|
| **12** | Dispatch reason codes | #496 | `d9dbbf9df7f0` | mig `0058_dispatch_attempt.sql`; `store/src/repo/dispatch_attempt.rs`; `hangar/dispatch_attempts_list`; `proto/tests/dispatch_reason_wire.rs` |
| **13** | Generic activity log + per-issue timeline | #497 | `056c618404a9` | mig `0059_activity_log.sql`; `hangar-core/src/activity.rs`; `hangar/issue_timeline` |
| **14** | Autopilot rule versioning + human attribution | #499 | `742a71d5a814` | mig `0061_autopilot_rule_version.sql`; `hangar/autopilot_versions` |
| **15** | Autopilot `api` trigger + `skipped` run status | #492 | `0b4dcf13427d` | mig `0057_autopilot_api_trigger_and_skipped_run.sql`; `hangar/autopilot_trigger_api` + `hangar/autopilot_set_api_trigger` |
| **17** | Custom property catalog + issue metadata scratch bag | #507 | `edeee909db40` | mig `0066_issue_properties_metadata.sql`; `hangar/property_define\|properties_list\|property_archive\|issue_property_set\|issue_property_clear\|issue_metadata_get\|_set\|_delete` |
| **18** | Workspace membership invite lifecycle | #503 | `8fe91c3967a4` | mig `0063_workspace_invitation.sql`; `hangar/invite_create\|accept\|decline\|revoke` |
| **19** | `blocked` + `cancelled` issue states | #480 | `97b6e8a48a1a` | mig `0049_issue_state_blocked_cancelled.sql` |
| **20** | Typed issue dependency graph | #489 | `8bd2ed33e095` | mig `0055_card_dependency_link_type.sql`; `hangar/issue_link_add\|issue_link_remove\|issue_links` |
| **21** | Issue origin provenance | #491 | `05edfd858a8e` | mig `0056_issue_origin_provenance.sql` |
| **22** | Issue subscribers + reactions | #501 | `79ccdaa0f762` | mig `0062_issue_subscriber_reaction.sql`; `hangar/issue_subscribe\|issue_unsubscribe\|issue_subscribers\|issue_reaction_add\|issue_reaction_remove` |
| **23** | Agent metadata (description/avatar/kind/service_tier/UNIQUE name) | #481 | `afec29bc1227` | mig `0050_agent_metadata.sql` |
| **25** | Squad per-member role + squad instructions | #484 | `6a6b77f0eb23` | mig `0053_squad_role_instructions.sql`; `hangar/squad_member_role_set` + `hangar/squad_instructions_set` |
| **26** | Archive audit trail (agent + squad) | #483 | `cdaeb60fa6b7` | mig `0052_archive_audit.sql`; `hangar/agent_archive` + `hangar/squad_archive` |
| **27** | Autopilot subscriber / collaborator model | #505 | `23dbee054539` | mig `0064_autopilot_subscriber_collaborator.sql`; `hangar/autopilot_subscriber_*` + `hangar/autopilot_collaborator_*` |
| **30** | `custom_env` redaction contract | #494 | `9c5cf05858b7` | no migration — `hangar-core/src/agent_env.rs` (the redaction seam) + the `has_custom_env`/key-count wire shape in `proto/src/events.rs` |
| **1‑rest** | Actor-polymorphic inbox | #498 | `763c4fe79a36` | mig `0060_inbox_recipient.sql` — `recipient_type CHECK IN ('member','agent')` + `recipient_id`, with both a chronological and a uniqueness index on `(workspace_id, recipient_type, recipient_id)` |
| **2‑rest** | Mention routing layer with per-target outcomes | #509 | `f407f3d4622a` | mig `0067_comment_thread_and_trigger.sql` (`comment.parent_id` self-FK `ON DELETE SET NULL` + `agent_task_queue.trigger_comment_id`); `daemon/src/mentions.rs` parses the `[@Label](mention://type/id)` link form; `MentionOutcomeRow` in `proto/src/snapshots.rs`; `hangar/comment_mention_preview`; member-mention + reply-chain routing tests in `daemon/src/rpc/snapshots.rs` |
| **3‑rest** | Batched multi-stage cascade aggregation | #506 | `8d00d0424e24` | mig `0065_issue_cascade_barrier.sql`; `hangar/issues_batch_update` |
| **4‑rest** | Per-instance workspace-creation lockdown | #495 | `97c2b5577f93` | no migration — `hangar-core/src/daemon_config.rs`, gated inside `store/src/repo/workspace.rs`; `store/tests/workspace_creation_lockdown.rs` + `ainb-core/tests/hangar_workspace_lockdown_cli.rs` |
| **7‑rest** | Squad-leader roster carries per-member skills | **#486** (*not* #487) | `68a7527aa214` | `daemon/src/squad_briefing.rs` reading `agent_skill.enabled` (mig 0051) + `agent.disabled_runtime_skills`, with the materialised-`CLAUDE.md` proof |
| **11‑rest** | Per-criterion id + checked state | #488 | `dfb17ad20a94` | mig `0054_acceptance_criterion_state.sql`; `hangar/issue_criterion_set` |

**Tracking-sheet correction.** The run sheet recorded `7-rest` against **PR
#487**. PR #487 is `test(ainb-core): kill the resume-tmux flake with a private
tmux server + spawn retry` — an unrelated flake fix. The SHA the sheet carried
(`68a7527a`) is PR **#486**'s merge commit, which is the real 7‑rest landing.
The number was wrong; the SHA was right.

Landed just before this run and folded in for completeness: **#28** wizard
priority/due/labels (#476, `6c4b9457e85c`), **#450 / #29** Boards `q:squad`
(#477, `6ba70f269b50`), **6‑rest** availability derived from the runtime
heartbeat (#478, `cae2c4b6a07c` — `PresenceState::Unstable` now has a producer
and the 5-minute grace fold lives in `proto/src/events.rs`, so the amber dot is
reachable), **8‑rest** the invoke gate applied to the squad fan-out and mention
dispatch (#479, `1b86d4335f92`), **#10** faceted filtering (#470,
`4f2e03d442f1`).

With those, **all 30 numbered gaps in the master matrix are closed at least as
scoped**, and every `-rest` follow-up except the four named below is closed too.
"Closed as scoped" is not the same as facet-complete — the per-gap rows above
and in the previous table state exactly where hangar deliberately diverged.

### Updated parity by entity

Three columns of history: the original 2026-07-23 eyeball, the 2026-07-24 audit,
and today. Percentages remain a **rough eyeball, not a measured metric** — they
exist to point at structural holes. A gap counted here is counted at the scope
hangar actually shipped, and every deliberate divergence is named.

| Entity | 07-23 | 07-24 | **Now** | What moved it this run | What still genuinely holds it back |
|---|---|---|---|---|---|
| Issue | ~35% | ~60% | **~85%** | #17 property catalog + metadata bag, #19 blocked/cancelled, #20 typed deps, #21 provenance, #22 subscribers/reactions, 11‑rest checkable criteria, 3‑rest batched cascade, and **2‑rest** — the mention routing layer (`mention://` links, member mentions, reply-parent/thread-owner fallback, per-target outcome codes, preview) | `10-rest` facets (`Creators[]`, `ProjectIDs[]`, custom-property facets, 2-level grouping, cursor pagination, `Scope` involves-filters); `17-rest` (the runtime-brief `## Issue Metadata` section and `@>` containment filtering); **no `hangar/comment_list` RPC**, so comment + cascade history is still invisible on reopen; 3‑rest has no in-TUI batch producer (no multi-select / bulk-state keybinding) |
| Squad | ~40% | ~65% | **~85%** | #25 per-member role + squad instructions, #26 archive audit, 7‑rest (the roster now advertises each member's *materialisable* skills, honouring `agent_skill.enabled` + `disabled_runtime_skills`), 8‑rest (the invoke gate now wraps the fan-out) | **#16** selective leader routing vs spray fan-out — a **product decision**, not a defect; **7‑cwd (F1)**, open as issue **#485**: the briefing is written into the TASK tree while a card run's provider `cwd` is its worktree, so a cwd-relative reader would not see it |
| Workspace / membership | ~40% | ~75% | **~90%** | 4‑rest per-instance creation lockdown (`daemon_config: workspace.creation_disabled` + the one-way env override, gated inside `WorkspaceRepo::create`), #18 invite → accept/decline/revoke/expire | Invite **delivery** is out of band — there is no email/notification channel, so an invite id is handed over by hand. The zero-workspace onboarding redirect and the `invitation:*` event stream stay multica-web concerns and are not being chased |
| Agent | ~55% | ~70% | **~90%** | #23 metadata columns, #26 archive audit, #30 `custom_env` redaction, 6‑rest availability derivation (the 5-minute `unstable` grace now has a producer, so the amber dot is reachable), 8‑rest gate on all three dispatch paths | **9‑rest**: the LLM turn behind the agent builder. #23 landed `kind`/`system_key`, so it is unblocked — but the builder is still a structured-draft wizard "minus the LLM turn" |
| Autopilot | ~75% | ~95% | **~98%** | #27 subscriber / collaborator model — the last enumerated autopilot facet, previously deferred "while solo" | Nothing enumerated. The 2% is honesty margin, not a known item |
| Task + dispatch flow | ~80% | ~80% | **~95%** | #12 dispatch reason codes (mig 0058 `dispatch_attempt` + `hangar/dispatch_attempts_list`), #13 generic activity log + `hangar/issue_timeline` | Nothing enumerated. Remaining delta is the "Deliberately NOT chasing" list below, which is architecture, not debt |

**Honest overall: ~55% → ~68% → ~88%.** Read that as: every one of the 30
numbered gaps is closed at the scope hangar chose, the two facets the 07-24
audit called out as most defining — **conversation-driven delegation** (2‑rest)
and **actor symmetry in the inbox** (1‑rest) — both landed this run, and the
authoritative CI gate that made the whole campaign unprovable is now green.

It is **not** 100%, and the missing 12% is not rounding. It is four named
residuals plus two sub-gaps (below), and the standing caveat that "closed as
scoped" repeatedly means a narrower facet than multica's — client-side facets
instead of a server-side `ListIssueTableFacets` endpoint, dispatch-time skill
materialisation instead of a live tool registry, roster text instead of
`mention://` roster links. Those choices are defensible for a single-user local
control plane and are documented in-source; they are still not parity.

### What genuinely remains

Four named residuals and two sub-gaps. Nothing here is "unstarted work on a
numbered gap" — every numbered gap has landed.

| # | Item | Effort | Why it is still open (the real blocker) |
|---|---|---|---|
| 16 | Squad selective leader routing vs spray fan-out | L | **Not a defect — an unmade product decision.** Multica's leader reads the issue and delegates to the fit member(s) by skill/role; hangar sprays to every agent member concurrently, each in its own worktree. Both are coherent. 2‑rest (the delegation mechanism) and 7‑rest (the roster the leader would route from) are now in place, so this is now blocked **only** on someone choosing. Deciding "spray is what we want" closes it at zero code cost |
| 9‑rest | The LLM turn behind the conversational agent builder | L | Was blocked on #23's `kind`/`system_key` columns; those landed (mig 0050), so the blocker is now **scope, not dependency**. It needs a hidden `kind='system'` builder agent that proposes a structured draft — i.e. a real inference loop inside the TUI wizard, which nothing else in hangar does yet |
| 10‑rest | The remaining issue facets: `Creators[]`, `ProjectIDs[]`, custom-property facets, 2-level grouping, cursor pagination, `Scope{kind,Relation,Actor}` involves-filters | M–L | Partly a **deliberate architecture split**: hangar computes facets client-side over the already-delivered `hangar/issues_list` snapshot and has no server-side `ListIssueTableFacets` endpoint. Grouping and the property facets are real work (#17's catalog unblocked the latter); cursor pagination is arguably moot at single-user scale and should be explicitly rejected rather than carried |
| 17‑rest | The runtime-brief `## Issue Metadata` section + `@>` containment filtering | S–M | Scoped out of #507 on purpose. The catalog, the columns, the RPCs and the CLI all landed; what is missing is (a) surfacing `issue.metadata` into the agent's brief so a pipeline agent can read its own stashed state back, and (b) JSON containment filtering, which SQLite makes awkward compared to multica's Postgres `@>` |
| 7‑cwd (F1) | Squad-leader briefing lands in the task tree, not the run's worktree | S–M | Tracked as **open issue #485**. Delivery mechanism, not briefing content — the briefing itself is correct and proven. Blocked on picking where a card run's provider `cwd` should read shared context from |
| sub-gap | No historical comment surface: there is **no `hangar/comment_list` RPC** | S | Confirmed still absent — the method constant does not exist in `proto/src/methods.rs` (111 constants, none of them a comment list). Live `CommentAdded` events interleave into an already-open transcript, so the render path exists; only hydration-on-open is missing. This is the cheapest remaining item and it makes three shipped features (#3's parent-wake comment, 2‑rest's mention trigger, #13's timeline) observable |
| sub-gap | Host router reserves `?`, `H`, `W` from plugin screens | S | Host contract, by design (`ainb-core/src/app/events.rs`). Documented so plugin authors stop rediscovering it. The disjointness invariant is now enforced by `no_screen_binds_a_reserved_key` |

**No longer a blocker:** the 07-24 audit's `P0 for the platform` item — repair the
`hangar-e2e (ubuntu-latest)` gate — is **done**. See the resolved caveat at the
top of this section: the full 34-tripwire matrix and the framed-socket + CLI
acceptance suite both run and both pass on `main`, and `Test` now builds
`--tests` so the integration proofs are no longer invisible.

### Sub-gaps discovered during the campaign (not in the original 30)

| Sub-gap | Evidence | Impact |
|---|---|---|
| **No historical comment surface in the TUI** | `TaskDetailState::new` (`plugin-hangar/src/screen/task_detail.rs:259`) starts with an empty transcript, and there is **no `hangar/comment_list` RPC** — `methods.rs` has only `HANGAR_COMMENT_ADD`. Live `CommentAdded` events *do* interleave into the transcript (`fold_event`, slate lane, `is_comment: true`), so the render path exists — but only for comments that arrive **while the screen is already open**. | Comment and cascade activity (including gap #3's parent-wake comment and gap #2's mention trigger) is invisible on reopen. A `comment_list` RPC + hydration on open is the fix; small, and it makes two shipped features observable. |
| **Host router reserves keys from plugin screens** | The host intercepts `?` and `H` (help) and `W` (statusline wire) globally for any non-text context before plugin delivery (`ainb-core/src/app/events.rs:1483–1555`); the generic `captures_text` gate only exempts plugin **text-input** modes. Plus Ctrl+C per the host contract. | Plugin screens cannot bind these keys, which is why hangar features have had to take lowercase bindings. *Unverified:* the specific claim that uppercase `S`/`D` are also stolen — no such global handler appears in the pre-plugin block, so that one needs a live tmux check before being treated as fact. |
| **#450 `q:squad` — FIXED** | Issue #450 was auto-closed by a cross-reference from PR #459 (2026‑07‑23T19:47:08Z, no comment, no closing commit) while still broken. Now genuinely fixed: the global router keeps bare `q` (the only keyboard escape hatch off Boards), and the squad picker moved to `s` (`board_nav_event`), depends-on to `w`, column reorder to `<`/`>`. The reserved sets live once in `screen/router.rs` (`ROUTER_KEYS` / `HOST_RESERVED_KEYS` / `is_reserved_key`) and `no_screen_binds_a_reserved_key` (`screen/app_screens.rs`) enforces disjointness in both directions. | Resolved. Same sweep also un-stole Kanban `H`/`L`, Fleet `A`/`B`, Settings `K` — see `every_boards_hint_band_key_is_reachable` and `footer_hint_keys_never_collide_with_reserved_router_keys`. |

## Master gap matrix

> **Status as of 2026-07-27: all 30 rows below are closed at least as scoped.**
> This table is retained as the ORIGINAL reference — the multica-vs-hangar
> field-level comparison and the effort/impact ranking that drove the campaign.
> Do **not** read the "Hangar has" column as current state; read [Parity status
> (2026-07-27)](#parity-status-2026-07-27) for that, and the four named
> residuals (16, 9‑rest, 10‑rest, 17‑rest) plus 7‑cwd/#485 for what is left.

One ranked table across all entities. Overlapping gaps (the polymorphic-actor
gap surfaces in Workspace + Issue + Squad; comment-dispatch underpins Squad
coordination too) are **stated once** at their foundational home and
cross-referenced. Ranked by "changes what the product *is*" first, then
user-visible impact, then effort. Effort: **S** ≈ days, **M** ≈ 1–2 weeks,
**L** ≈ multi-week / touches many surfaces.

| # | Entity | Gap | Multica has | Hangar has | Impact (what it unlocks) | Effort |
|---|---|---|---|---|---|---|
| **1** | Cross-cutting | **Polymorphic actors + human members** | `actor_type (member\|agent)` on assignee/creator/comment/inbox/activity; humans are first-class collaborators | Agent-centric; no human assignee/commenter/inbox concept; `user`+`member` tables exist but nothing mints a 2nd human | The whole "agents are team members" identity. Every collaboration surface (assign, comment, mention, inbox) becomes symmetric. Foundation the mention-dispatch and squad-human-members gaps sit on | L |
| **2** | Issue / Squad | **Comment @mention auto-dispatch** | `[@x](mention://type/id)` parsed → routes to explicit-mention / reply-parent / thread-owner / assignee-fallback; per-target `queued\|coalesced\|deferred\|blocked` outcomes; preview; self-loop + private-agent gates; merge-into-pending dedup | Comments are inert text — zero side effects on write | Hand off work by *conversation*. This is the single biggest behavioral feature Hangar lacks — it is also the mechanism squad leader→member delegation and re-trigger loops are built on | L |
| **3** | Issue | **Subtasks: parent/child + stage barriers + child-done cascade** | `parent_issue_id` self-FK; `stage` barrier groups; child terminal→parent wake comment; batched multi-stage aggregation | Closed (#463 + 3-rest): parent/child, stage barriers, and the batched multi-stage aggregation (mig 0065 barrier ledger) | Decompose an issue into tracked sub-issues with roll-up progress and automatic parent wake when a stage closes | L |
| **4** | Workspace | **Multi-workspace (create / switch / delete)** | `CreateWorkspace` API + `/{slug}/…` nav, reserved-slug validation, per-instance creation lockdown flag | Create/delete/switch landed (#465); the creation-lockdown flag landed (4-rest) | More than one project/tenant at all. Everything below (invites, roles, per-workspace config) is moot with one workspace | L |
| **5** | Squad | **Task-level `squad_id` + claim-time leader briefing hook** | `agent_task_queue.squad_id` (mig 127) so the daemon injects briefing at claim | Task rows carry no `squad_id`; issue has `squad_id` but the task doesn't — no claim-time hook to key briefing off | Structural prerequisite for a real squad leader (gap #7). Cheap column, but unlocks the whole leader-coordination model | S |
| **6** | Agent | **Two-dimensional derived presence** | `Availability` (online/unstable/offline/archived, 5-min "unstable" grace) × `Workload` (working/queued/idle from live task counts) — pure derivation | `agent.archived` bool + `agent_runtime.status` binary online/offline; no grace window, no workload signal | Every list/card dot. Users can tell "runtime blipped" from "dead", and "queued/stuck" from "idle" — the signal Multica's whole list UI is built on | M |
| **7** | Squad | **Leader briefing (Operating Protocol + Roster + Instructions)** | System-prompt injection at claim: coordinator role + 6 responsibilities, ready-to-paste mention roster with skill names, user-authored routing instructions | Nothing — a fanned-out leader runs like a solo agent, no idea it's a leader | Makes "squad leader" a real role instead of nominal. Depends on #5 (task `squad_id`) + #2 (mention dispatch for delegation) | L |
| **8** | Agent | **Invocation permissions (private / public_to + allow-list)** | `permission_mode` + `agent_invocation_target` (workspace/member/team, OR-matched); admin does NOT bypass private | `visibility` (workspace/private) only; no allow-list, no `canInvokeAgent` gate | Share one agent with a *subset* of people; an auditable invoke gate. Depends on #1 for member targets to exist | M |
| **9** | Agent | **Conversational Agent Builder** | Chat with a hidden `kind='system'` agent that proposes name/description/instructions/model/skills/permission as a structured draft the user confirms | Bare name-input field (TUI) or flag-driven CLI; no assisted authoring | Non-technical users create good agents without hand-writing instructions or guessing model ids. Needs `kind`/`system_key` first | L |
| **10** | Issue | **Structured / faceted filtering** | status/priority/assignee/creator/label/property/date-range facets, facet-value-with-counts, 2-level grouping, cursor pagination | 4 fixed chips (All/Members/Agents/Mine) + free-text substring | Actually find issues at scale. No filter by status/priority/label/date exists today | L |
| **11** | Issue | **Acceptance criteria + context refs** | `acceptance_criteria` JSONB structured list + `context_refs` JSONB | Single free-text `external_ref` string | Give an agent a structured definition-of-done and multiple linked context items instead of one URL | M |
| **12** | Task | **Dispatch reason codes** | `dispatch.ReasonCode` enum (`invocation_not_allowed`/`target_unavailable`/`runtime_offline`/`attribution_blocked`/`already_active`/`self_trigger_suppressed`/…), wire-shared | Only `FailureReason` (post-hoc run failure); nothing for admission-time skips | "Why didn't this run?" observability for an issue that never produced a task at all | M |
| **13** | Task | **Generic activity log / audit trail** | `activity_log(actor_type, actor_id, action, details)` fed by bus listeners → per-issue timeline | `run_history` (task-execution-scoped only) | A per-issue narrative of every assign/label/comment/status event, not just run outcomes | M |
| **14** | Autopilot | **Rule versioning + human attribution** | `autopilot_rule_version` append-only per substantive publish; `originator` vs `accountable_user` split | None | Who is accountable for an unattended run — matters once autopilots are shared. Related to #1/#13 | M |
| **15** | Autopilot | **`api` trigger kind + `skipped` run status** | 3rd trigger kind (bare programmatic fire); `skipped` a first-class run row | schedule + webhook only; policy-skip is a log event, not a queryable row | CI/other-tool fire without minting a webhook secret; reporting can tell "skipped" from "never happened" | S |
| **16** | Squad | **Selective leader routing vs spray fan-out** | Leader reads issue, picks the *fit* member(s) by skill/role, delegates via @mention; others get no task | `assign_fanout` sprays to *every* agent member concurrently, no selection | Product decision, not a pure gap: mediated routing vs parallel-worker fan-out. Depends on #2 + #7. Decide before "fixing" | L |
| **17** | Issue | **Custom properties + metadata scratch** | `issue_property` typed catalog (select/multi_select, archivable) + flat `metadata` KV bag for agent pipeline state | **LANDED** — migration 0066 `issue_property` catalog + `issue.properties` / `issue.metadata` columns, `IssuePropertyRepo` / `IssueMetadataRepo`, `hangar/propert*` + `hangar/issue_metadata_*` RPCs, `ainb hangar property define\|list\|archive` + `issue property set\|clear` + `issue meta list\|get\|set\|delete`, `Props:` / `Meta:` on the task-detail card. Deferred as `17-rest`: the runtime-brief `## Issue Metadata` section and `@>` containment filtering | User-defined fields (Linear/Notion-style) + a place for agents to stash pipeline state (PR#, status) | M–L |
| **18** | Workspace | **Membership lifecycle (invite → accept/expire)** | `workspace_invitation` (7-day expiry, one-pending-per-email, stale sweep), auto-stub user, role-at-invite | **LANDED** — migration 0063 `workspace_invitation` + `InvitationRepo` create/accept/decline/revoke/list/sweep, `hangar/invite_*` RPCs with `pending_invites` on `members_list`, `ainb hangar member invite\|invites\|accept\|decline\|revoke`, Members-pane render | Add a second human: accepting an invite is what creates the membership | M |
| 19 | Issue | **`blocked` + `cancelled` states** | 7-state lifecycle with DB CHECK | 5 states, no `blocked`/`cancelled`, no CHECK (free text) | Represent blocked/cancelled as first-class states | S |
| 20 | Issue | **Typed dependency graph** | `issue_dependency` `blocks`/`blocked_by`/`related` | `card_dependency` single untyped blocks-edge (cycle-checked, auto-run) — core mechanic already parity | "related"/reverse edges + a browsable graph | S–M |
| 21 | Issue | **Origin provenance** | `origin_type`/`origin_id` (autopilot/quick_create/lark/slack/agent_create) | None | Trace who/what caused an agent-created issue | S–M |
| 22 | Issue | **Subscribers + reactions** | `issue_subscriber` (reason-tagged) + `issue_reaction` | Neither | Notification subscriptions + emoji reactions | M |
| 23 | Agent | **Metadata columns** | `description` (255-cap), `avatar_url`, `kind`(user/system), `service_tier`, `UNIQUE(workspace,name)` | None of these | Blurb/avatar in lists; no silent duplicate names; Codex service-tier control. `kind`/`system_key` also unblocks #9 | S–M |
| 24 | Agent | ~~**Per-agent skill enable/disable**~~ **DONE** | `agent_skill.enabled` + `disabled_runtime_skills` | ~~Attach/detach only, no toggle~~ — closed by migration 0051 | Temporarily disable a skill for one agent without detaching | S |
| 25 | Squad | **Per-member `role` + `instructions` + archive** | Free-text `role` (leader routes by fit), `squad.instructions`, `archived_at`/`archived_by` w/ transfer-on-archive | None of these columns | Route by stated specialty; per-squad routing guidance; safe squad retirement | S–M |
| 26 | Agent / Squad | **Archive audit trail** | `archived_at` + `archived_by` (who/when) | `archived` boolean only (agent); no archive at all (squad) | Accountability for who retired an agent/squad and when | S |
| 27 | Autopilot | **Subscriber / collaborator model** | `autopilot_subscriber` (auto-subscribe to spawned issues) + `autopilot_collaborator` write-grants | Single-owner only | Team-shared autopilots. Depends on #1. Fine to defer while solo | S–M |
| 28 | Issue | **Surface existing priority/due/labels in create wizard** | `CreateIssueRequest` accepts priority/status/labels/dates directly | Schema has priority/due/labels since mig 0014 — **wizard never surfaces them** | Cheapest real win: three more wizard rows, columns already exist | S |
| 29 | Squad | ~~**#450: Boards `q:squad` hotkey unreachable**~~ **DONE** | (n/a) | ~~Global router steals bare `q` as quit before Boards' `q → AssignSquad`~~ — fixed: the squad picker is `s`, depends-on `w`, reorder `<`/`>`; `q` stays the global escape hatch | Rejected the Boards-no-overlay guard (it traps the user: Boards has no Esc and `?`/`H` are host-eaten). Instead: one reserved key set in `screen/router.rs`, screens rebound off it, disjointness enforced by `no_screen_binds_a_reserved_key` | S |
| 30 | Agent | **`custom_env` redaction contract** | Never serialized; `has_custom_env`/key-count only; audited GET/PUT endpoint | Stored/returned plain JSON | Secrets hygiene if Hangar ever grows multi-user/remote | S |

## Deliberately NOT chasing

The task-flow deep-dive flagged several "gaps" that are **moot given Hangar's
architecture** (single-process daemon colocated with SQLite, one TUI client, no
browser/remote client). Listing them so nobody re-opens them:

| Multica feature | Why it's not a gap for Hangar |
|---|---|
| `EmptyClaim` Redis short-circuit cache | SQLite claim is one local statement — no network round-trip to amortize |
| `reconcileBroadcaster` one-slot replay | Daemon + DB are colocated; no daemon↔server WS reconnect to recover from |
| `FinalizeTaskClaim` split-commit + requeue-on-payload-fail | Claim + dispatch are same-process; no network boundary between "claimed" and "daemon has payload" |
| Two-hub fan-out (browser realtime.Hub + daemon wakeup) | No browser client to fan out to; the TUI plugin is the only subscriber and gets full payloads directly |
| Authoritative-vs-estimated cost split (`cost_usd_ticks`) | `run_history` + `cost_rollup` VIEW is adequate at Hangar's scale; cosmetic |
| Generic DB-backed lease/scope scheduler | Purpose-built single-daemon scheduler is fine; no multi-instance lease contention |

And two axes where **Hangar is ahead of Multica**:

- **`concurrency_policy` (skip/queue/replace)** — Hangar's is fully wired and
  tested (`scheduler.rs` + `scheduler_concurrency_policies.rs`); Multica's is a
  **dead schema column** read nowhere.
- **Squad fan-out** — Hangar dispatches leader + every agent member concurrently
  (each its own worktree); Multica only dispatches the leader. (Whether spray or
  mediated routing is *wanted* is gap #16, a product call.)

Parity axes already reached: autopilot `execution_mode`, autopilot webhook
delivery-audit + event-filters, the per-(issue,agent) claim guard + concurrency
cap, the dispatched/reclaim/timeout sweeper ladder.

## Prioritized roadmap

> **Historical.** Every P0/P1/P2 item below has landed. The live to-do list is
> the "What genuinely remains" table in [Parity status
> (2026-07-27)](#parity-status-2026-07-27).

Grouped by tier. Within a tier, items are roughly dependency-ordered.

### P0 — Foundational (structural; other work depends on these)

| # | Item | Effort | Unblocks |
|---|---|---|---|
| 1 | Polymorphic actors + human members (member\|agent everywhere) | L | #2, #8, #18, #25-human, autopilot attribution |
| 5 | Task-level `squad_id` + claim-time briefing hook | S | #7 (leader briefing) |
| 3 | Subtasks: `parent_issue_id` + stage barriers + child-done cascade | L | roll-up progress, staged completion |
| 4 | Multi-workspace (create/switch) | L | #18 invites (landed), per-workspace config, slug validation |

Cheapest P0 to land first: **#5** (one column + a claim hook) makes the squad
leader briefing (#7) possible and is nearly free.

### P1 — High-impact (the features that make agents feel like team members)

| # | Item | Effort |
|---|---|---|
| 2 | Comment @mention auto-dispatch (route + outcomes + preview + gates) | L |
| 7 | Squad leader briefing (protocol + roster + instructions) | L |
| 6 | Two-dimensional derived agent presence (availability × workload) | M |
| 8 | Invocation permissions (private/public_to + allow-list) | M |
| 9 | Conversational Agent Builder | L |
| 10 | Structured / faceted issue filtering | L |
| 11 | Acceptance criteria + context refs | M |
| 12 | Dispatch reason codes | M |
| 13 | Generic activity log / audit trail | M |
| 14 | Autopilot rule versioning + human attribution | M |
| 16 | Squad selective routing (product decision first) | L |

### P2 — Polish / cheap wins (several are UI-only; schema already has the column)

| # | Item | Effort | Note |
|---|---|---|---|
| 28 | Surface priority/due/labels in issue create wizard | S | **schema already has these (mig 0014)** — UI-only |
| 29 | ~~Fix #450 Boards `q:squad` hotkey~~ **DONE** | S | squad picker rebound to `s`; reserved-key invariant now enforced by test |
| 19 | `blocked` + `cancelled` issue states | S | |
| 23 | Agent metadata columns (description/avatar/kind/service_tier/unique-name) | S–M | `kind`/`system_key` also unblocks #9 |
| 26 | Archive audit trail (agent + squad `archived_at`/`archived_by`) | S | |
| 25 | Squad per-member `role` + `instructions` + archive | S–M | `instructions` feeds #7 briefing |
| 20 | Typed issue dependency graph (`blocked_by`/`related`) | S–M | core auto-run mechanic already parity |
| 21 | Issue origin provenance (`origin_type`/`origin_id`) | S–M | |
| 15 | Autopilot `api` trigger + `skipped` status | S | |
| 30 | `custom_env` redaction contract | S | |
| 17 | Custom properties + metadata scratch | M–L | larger; promote to P1 if agent-pipeline-state is needed |
| 22 | Issue subscribers + reactions | M | |
| 27 | Autopilot subscriber/collaborator | S–M | depends on #1; defer while solo |
| 18 | Membership invite lifecycle | M | **landed** (was: depends on #4; deferred while single-workspace) |

**Cheap-wins where the schema already has it and only the UI is missing:**
issue **priority / due_date / labels** (columns since mig 0014, wizard omits
them — #28), and the `card_dependency` auto-run mechanic (#20 core already
built, only the typed-edge variants + graph browse are missing).
