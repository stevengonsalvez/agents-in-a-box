# Task-Create Parity + Board Completeness — Feature Spec

**Status:** interview-locked 2026-07-04 · **Follows:** converged-control-center-spec.md (v1 shipped)
**Driver:** cards today launch "random" tasks — no repo, no agent choice. Bring card-create to
full New-Session parity and make board runs production-real.

## Locked decisions

| # | Decision |
|---|----------|
| F1 | **Card-create = New-Session-parity overlay**: title · repo · agent (claude/codex/copilot) · profile · column. `@` anywhere in the repo field triggers autocomplete. |
| F2 | **Repo REQUIRED** — card cannot launch without one. The picker always offers `📁 scratch` first-class: a real git repo auto-created at `~/.agents-in-a-box/scratch/<slug>` on first use. Never silently random. |
| F3 | **`@` autocomplete source** = exactly New Session's: ★ favorites pinned first, then RepositoryCache scanned repos, fuzzy-filtered on the @-query, recency tiebreak. (Traps: favorites migrate-to-remote drops no-remote stars; scan cache cold on first open — pre-warm; see reference_new_session_test_traps.) |
| F4 | **Agent default cascade**: last-used → board default → workspace default → global config.toml. Picker pre-selects the cascade result; any pick updates last-used. |
| F5 | **Workdir = "same as New Session"**: volatile git worktree per task via the same `WorktreeManager` contract (`ainb-core/src/git/worktree_manager.rs`) — worktree under `~/.agents-in-a-box/worktrees`, branch `ainb/<slug>`, torn down on completion, kept if dirty. N tasks on one repo never collide. Daemon reaches it via a fleet-core seam if a crate cycle looms (established pattern). |
| F6 | **Card lifecycle v1**: cancel running (kill tmux + finalize) · rerun finished/failed · view logs/transcript from card WITHOUT attaching · **prettied session timeline rendered from the JSONL** (tool calls, durations, LAST REPLY — the agentpeek timeline, per-task) · PR + branch + CI/mergeable status on the card (pr_status/pr_url plumbing exists) · edit (title/repo/agent/profile) / delete / reorder. |
| F7 | **Dependencies**: beads-style `depends-on` between cards; a blocked card never auto-dispatches until blockers close; done-blockers make it runnable. |
| F8 | Copilot: picker shows it; dispatch lands when the provider runner grows the third backend (claude+codex first — D7 unchanged). |

## Build waves (all four locked in, this order)

1. **Wave T1 — task-create parity**: F1-F5 (overlay, @ autocomplete, scratch repo, agent cascade, worktree isolation at dispatch). Store: card gains repo_ref + agent + defaults tables/columns; RPC: extend board_card_create/run (append-only CTS).
2. **Wave T2 — worktree + PR surfacing**: F5 wiring end-to-end + branch/PR/CI on the card (execution-correctness layer).
3. **Wave T3 — card lifecycle**: F6 (cancel/rerun/logs/timeline/edit/delete/reorder).
4. **Wave T4 — squads + deps on the board**: F7 + assign-squad-to-card with leader routing (orchestration layer).
5. **Wave T5 — notification routing rules**: per-attention-kind channel routing (phone/web/OS/ATC-only), per-workspace rules.

## Validation contract (inherited)

Same as the ccc campaign: unseeded tripwires per behaviour (no seeded fixtures for interaction
paths — the class that hid lu5), real-provider journey legs at wave close, vhs frame-truth for
each new surface, design gates (style-guide + review + insta) per screen change, journey GIFs
into the catalogue.

## Acceptance journeys (per wave close, on film)

- T1: create card with `@`-picked favorite + codex agent + scratch fallback path → run → task
  executes in its own worktree on branch `ainb/<slug>` → two cards on the SAME repo run
  concurrently without collision.
- T2: card run pushes a branch → PR opened → card shows PR + CI status live.
- T3: cancel mid-run (session reaped, card back to runnable) → rerun → timeline view shows the
  full prettied JSONL story without attaching.
- T4: card B depends-on A; B refuses dispatch until A completes, then auto-runs; squad card
  fans out via leader.
- T5: an ASK routes to phone only; an error routes to OS-notify + web, per rules.
