# Specification: /swarm-create v2 — Self-Sufficient Swarm with Watchdog

**Generated from:** interactive interview with Stevie
**Interview date:** 2026-05-15
**Version:** 1.0

## Executive Summary

Upgrade `/swarm-create` to ship every swarm with a cross-provider bash watchdog daemon that detects stuck panes, nudges them comprehensively, escalates to the leader, and writes a notify-only finalize report when the epic is done — without ever killing tmux or auto-merging worktrees. Adds opt-in `/loop` for Claude users (belt-and-suspenders) and a `/swarm-attach-watchdog` retrofit command for existing swarms.

## Objectives

### Primary Goals
- Swarms never get stuck without intervention (auto-nudge + auto-escalate)
- Works across providers (Claude Code, Codex, Copilot — anything in tmux)
- Finalize is always notify-only — humans own merges, PR ready-marking, and tmux teardown
- Existing swarms can be retrofitted without recreation

### Success Metrics
- Stuck panes recovered within 2 watchdog cycles (10 min at default 5-min cadence)
- Zero commits lost during finalize (no auto-kill, no auto-merge)
- One-flag opt-in for auto-PR; default is report-only
- /swarm-attach-watchdog can rescue an in-flight v1 swarm in <30 seconds

## Scope

### In Scope
- New file: `watchdog.sh` (bash, no LLM, runs in dedicated tmux session)
- New file: `finalize.sh` (notify-only finalize: report + optional draft PR)
- Provider-spinner regex table in `watchdog.sh`
- `team.json` schema additions (`provider`, `commands`, `ci`, `watchdog`)
- New CLI flags on `/swarm-create`: `--provider`, `--tick-min`, `--auto-pr`, `--use-loop`, `--no-watchdog`
- New command: `/swarm-attach-watchdog <team-id>`
- Worker-prompt addition: "auto-check inbox + bd ready on each tick"
- Auto-detect verify command (Cargo.toml / package.json / pyproject.toml / go.mod / Makefile) into `team.json`

### Out of Scope
- Auto-kill of any tmux session (explicit constraint from Stevie)
- Auto-merge of worktrees (only dry-run + report)
- Claude-only features (must work cross-provider)
- Cross-machine watchdog (single-host only in v2)
- `--verify-cmd` override flag (auto-detect is sufficient; user can edit team.json post-creation)

### Future Considerations (v3 candidates)
- launchd/systemd unit option for persistence-across-reboot
- Remote watchdog (e.g., GitHub Actions cron) for multi-host setups
- Cap-aware bd reassignment (move stuck-on-cap bd to a free worker)
- Provider auto-detection from pane scan

## Technical Requirements

### Architecture

```
            ┌────────────────────────────────────────────┐
            │  <team-id>-watchdog  (bash, no LLM)         │
            │   every 5min:                                │
            │   1. capture-pane each member                │
            │   2. detect stuck/idle/capped/active         │
            │   3. on stuck: Enter + "continue" + Enter    │
            │      + verify spinner; after 2 stuck cycles  │
            │      escalate to leader.jsonl as 'help'      │
            │   4. on epic-done: run finalize.sh           │
            └─┬──────────────────────┬───────────────────┘
              │ tmux send-keys       │ JSONL writes
              │                      │ to inbox/leader.jsonl
              ▼                      │ and inbox/agent-N.jsonl
   <team-id>-leader                  │
   (Claude/Codex/Copilot TUI)        │
   processes inbox + bd dispatches   │
              │                      │
              ▼                      ▼
   <team-id>-agent-{1..N}      (workers commit, write status)
```

### Components

| Component | Purpose | Technology |
|-----------|---------|------------|
| `watchdog.sh` | Periodic tick: capture panes, nudge stuck, escalate, check epic-done | Pure bash |
| `finalize.sh` | Run verify command, generate report, optional draft PR, dry-run worktree merge | Pure bash |
| `swarm-lib.sh` (extend) | Add `swarm_attach_watchdog`, `swarm_detect_provider`, `swarm_detect_verify_cmd`, `swarm_spawn_watchdog` | Pure bash |
| `team.json` (schema v2) | Persist provider, verify cmd, watchdog state, finalize policy | JSON |
| Worker prompt template | Add "auto-check inbox + bd ready every tick" instruction | Prompt text |
| `/swarm-create` (skill rewrite) | Accept v2 flags, auto-detect, write team.json v2, spawn watchdog | SKILL.md |
| `/swarm-attach-watchdog` (new skill) | Read existing team.json, spawn watchdog | SKILL.md |

### team.json Schema (v2)

```json
{
  "team_id": "swarm-1778723020",
  "epic_id": "ai-coder-rules-x5b",
  "isolation": "shared|worktree",
  "config": { "max_members": 4 },
  "members": { /* existing */ },
  "leader": { /* existing */ },

  // NEW IN v2
  "provider": "claude|codex|copilot",
  "commands": {
    "verify": "cargo test --workspace --no-fail-fast",
    "lint": null,
    "build": null
  },
  "ci": {
    "auto_pr": false,
    "pr_template_path": null
  },
  "watchdog": {
    "enabled": true,
    "tick_min": 5,
    "session": "swarm-1778723020-watchdog",
    "pid_file": "<team-dir>/watchdog.pid",
    "log_file": "<team-dir>/watchdog.log",
    "use_loop": false
  },
  "finalize": {
    "policy": "notify-only",
    "report_path": "<team-dir>/shared/finalize-report.md"
  }
}
```

### Auto-Detect Verify Command Table

| File present in worktree | `commands.verify` |
|---|---|
| `Cargo.toml` | `cargo test --workspace --no-fail-fast` |
| `package.json` (with `test` script) | `npm test` |
| `pyproject.toml` or `pytest.ini` or `setup.cfg` | `pytest` |
| `go.mod` | `go test ./...` |
| `Makefile` with `test` target | `make test` |
| `mix.exs` | `mix test` |
| `Gemfile` with `rspec` | `bundle exec rspec` |
| (none of the above) | `:`  (no-op; user must edit team.json) |

### Provider-Spinner Regex Table

| Provider | Active-spinner regex |
|---|---|
| `claude` | `(✻\|✳\|⏺\|✿\|◐\|◑\|◓\|◒) (Cooked\|Sprouting\|Metamorphosing\|Proofing\|Worked\|Baked\|Sautéed\|...)` |
| `codex` | TBD on first codex spawn — capture spinner during test integration |
| `copilot` | TBD on first copilot spawn |
| `generic` | "pane-hash changed in last N captures" |

> Note: codex/copilot spinner regexes are stubbed in v2; user can override `team.json.provider = "generic"` to use hash-only detection.

### Stuck-Pane Detection Algorithm

1. Capture last 15 lines of pane with `tmux capture-pane -t <target> -p -S -15`
2. If contains `5h \[██████\] 100%` or `You've hit your limit` → **CAPPED** (skip, don't nudge)
3. If contains provider-specific spinner regex → **ACTIVE** (good, no action)
4. If `^❯ [^ ]` (text in prompt without active spinner) → **STUCK**
5. If pane-hash unchanged from last capture AND no spinner AND no stuck-text → **IDLE** (don't nudge, just note)

### Comprehensive Nudge Sequence (per Stevie's "do all of your options")

When a pane is detected as STUCK, watchdog does ALL of the following in order:

```bash
# Step 1: Try plain Enter
tmux send-keys -t "$pane" C-m
sleep 1
tmux send-keys -t "$pane" Enter
sleep 5
recapture; if active or prompt cleared → DONE

# Step 2: Send "continue" + Enter
tmux send-keys -t "$pane" "continue"
sleep 1
tmux send-keys -t "$pane" Enter
sleep 5
recapture; if active or prompt cleared → DONE

# Step 3: Write help message to leader.jsonl
echo '{"type":"help","from":"watchdog","subject":"agent stuck","agent":"<name>","stuck_text":"<snippet>","stuck_cycles":<n>,"ts":"<iso>"}' \
  >> <team-dir>/inbox/leader.jsonl

# Step 4: Increment stuck-cycle counter in watchdog state
# After 2 consecutive STUCK detections → already in help-inbox
# After 4 consecutive → log NUDGE_FAILED + write to shared/incidents.log
```

State persistence between watchdog ticks: `<team-dir>/watchdog.state` (JSON: per-pane last hash, stuck-cycle counter, last action).

### Epic-Done Detection

Each watchdog tick:

```bash
ALL_CLOSED=$(BEADS_DIR=... bd list --json | \
  jq --arg epic "$EPIC_ID" \
    '[.[] | select(.parent_epic == $epic or .id == $epic)] |
     all(.status == "closed")')

if [[ "$ALL_CLOSED" == "true" ]] && [[ ! -f "$FINALIZED_MARKER" ]]; then
  bash <skill-dir>/finalize.sh "$TEAM_ID"
  touch "$FINALIZED_MARKER"
fi
```

`$FINALIZED_MARKER = <team-dir>/.finalized` ensures finalize runs only once.

### Finalize Behavior (notify-only)

`finalize.sh <team-id>`:

```
1. cd to worktree
2. Run commands.verify (from team.json). Capture exit code + output.
3. If worktree-isolation:
   For each agent worktree:
     git fetch
     git merge --no-commit --no-ff <agent-branch>
     If conflicts: record + git merge --abort
     If clean: record + git merge --abort (still abort — dry-run only)
4. Write shared/finalize-report.md:
   - Total commits per agent
   - Test result (pass/fail/output)
   - Conflict report per worktree
   - Time elapsed
   - bd closure summary
   - Next steps for human (merge cmd, PR cmd)
5. If team.json.ci.auto_pr == true AND verify passed:
   gh pr create --draft --title "..." --body-file shared/finalize-report.md
6. NOTIFY: write a single-line summary to leader.jsonl as 'finalize_done'
7. NEVER kill tmux. NEVER actually merge.
```

### Integration Points

- **bd**: read/write epic state, in_progress, closed; assignee changes
- **tmux**: send-keys for nudge, capture-pane for state, new-session for watchdog spawn
- **gh** (optional): PR creation when `auto_pr` true
- **git**: dry-run merge, fetch, branch listing

## User Experience

### CLI Flow

```bash
# Basic v2 (cross-provider)
/swarm-create --epic <epic-id> --agents 3 --provider claude

# Override tick interval
/swarm-create --epic X --agents 3 --provider codex --tick-min 3

# Belt-and-suspenders for Claude users
/swarm-create --epic X --agents 3 --provider claude --use-loop

# Auto-PR on finalize
/swarm-create --epic X --agents 3 --provider claude --auto-pr

# Escape hatch (v1 behavior)
/swarm-create --epic X --agents 3 --provider claude --no-watchdog

# Retrofit existing swarm
/swarm-attach-watchdog <team-id>
# Asks for --provider if not stored in team.json
```

### What the user sees at /swarm-create completion

```
========================================
Swarm Created: swarm-XXXXXXXXXX
========================================
Epic: <epic-id>
Provider: claude
Isolation: shared
Leader: swarm-XXXXXXXXXX-leader
Agents: 3
Watchdog: swarm-XXXXXXXXXX-watchdog (5-min ticks)
Verify cmd: cargo test --workspace --no-fail-fast (auto-detected)
Auto-PR: off

Commands:
  Attach to leader:    tmux attach -t swarm-XXXXXXXXXX-leader
  Attach to watchdog:  tmux attach -t swarm-XXXXXXXXXX-watchdog
  Watchdog log:        tail -f <team-dir>/watchdog.log
  Status:              /swarm-status swarm-XXXXXXXXXX
  Shutdown:            /swarm-shutdown swarm-XXXXXXXXXX
========================================
```

### Edge Cases

| Scenario | Expected Behavior |
|----------|-------------------|
| Watchdog crashes mid-loop | tmux session ends; user gets no nudges; user can `/swarm-attach-watchdog` to respawn |
| Provider regex misses an active spinner | Watchdog falsely flags as STUCK → sends Enter → may "submit" mid-thought. Mitigation: spinner regex tested per provider in integration tests |
| All workers capped (5h limit) | Watchdog detects all CAPPED → writes 'all_capped' note to leader.jsonl, skips nudge for capped panes, continues to monitor for un-cap |
| Epic-done but verify command fails | finalize.sh writes report with FAILED status; does NOT mark finalized; does NOT open PR; watchdog continues ticking (lets human fix and re-trigger) |
| User runs /swarm-create without --provider | Default to 'claude' with a warning. Document in skill that provider should be explicit. |
| Existing v1 swarm | User runs /swarm-attach-watchdog. Tool reads existing team.json, asks for provider (no flag yet), spawns watchdog. team.json gets upgraded in place. |
| Stuck-cycle counter persists across watchdog restarts? | Yes — `watchdog.state` lives in team-dir. New watchdog reads it on startup. |

## Constraints & Dependencies

### Technical Constraints
- **No auto-kill** of tmux sessions ever (Stevie's hard constraint)
- **No auto-merge** of worktrees ever (Stevie's hard constraint)
- Must be cross-provider — no Claude-specific features in critical path (only opt-in via `--use-loop`)
- Pure bash for watchdog (no python/node runtime requirement)
- Single host (no remote watchdog in v2)

### External Dependencies
- `tmux` (for sessions + send-keys + capture-pane)
- `jq` (for JSON parsing in bash)
- `bd` (beads CLI, already required by v1)
- `gh` (optional, for `--auto-pr`)
- `git` (for worktree dry-run + branch listing)

### Timeline Constraints
- v2 design locked: 2026-05-15
- v2 implementation: ~1 session focused work (estimate ~150-200 LOC bash + skill prompt rewrite)
- /swarm-attach-watchdog usable on current in-flight swarm: same day as implementation

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Provider spinner regex misses active state → false-nudge | Medium (could submit mid-thought) | Medium (regex coverage gaps) | Per-provider test fixtures; conservative regex; fallback to pane-hash heuristic |
| Watchdog tmux session dies silently | Medium (swarm reverts to v1 stuck behavior) | Low (bash loops are robust) | watchdog.sh writes heartbeat to team.json every tick; /swarm-status surfaces "watchdog stale" warning |
| Inbox JSONL race conditions (concurrent writes) | Low (file appends are atomic for <4KB) | Low | Standard `>>` append; size limit check before write |
| Auto-PR creates noisy PRs across many swarms | Low | Low | Opt-in only via flag |
| User runs /swarm-attach-watchdog twice → two watchdog sessions | Medium (duplicate nudges) | Medium | Check tmux has-session before spawn; abort if `<team-id>-watchdog` already exists |
| Verify command takes >1 hour at finalize | Medium (blocks finalize) | Low | finalize.sh runs verify with `timeout 3600` wrapper; fails report if exceeded |

## Decisions Made

### Key Trade-offs

- **Decision:** Cross-provider bash daemon as primary, /loop as Claude opt-in
- **Alternatives considered:** Claude-only /loop (rejected — locks out Codex/Copilot users); launchd/systemd (rejected for v2 — adds OS-specific complexity)
- **Rationale:** Bash + tmux is the lowest-common-denominator that works across all current and future agent runtimes. /loop is bonus for Claude users via `--use-loop`.

- **Decision:** Notify-only finalize, never kill tmux, never auto-merge
- **Alternatives considered:** Full auto-finalize (rejected — Stevie explicit no-kill); auto-merge if clean (rejected — verify command might not be exhaustive enough to gate a merge)
- **Rationale:** Cost of a missed nudge is "user has to manually intervene". Cost of a wrong auto-merge or auto-kill is "lost work". Asymmetric risk demands conservative finalize.

- **Decision:** Comprehensive nudge sequence (Enter → "continue" Enter → leader-help-inbox)
- **Alternatives considered:** Simple Enter-only (rejected — we observed Enter alone not flushing); aggressive bd-reassign (deferred to v3)
- **Rationale:** Stevie picked "do all of your options". Belt-and-suspenders pattern minimizes stalled cycles.

- **Decision:** team.json explicit `provider` field, no auto-detect
- **Alternatives considered:** Auto-detect from pane content (rejected — detection logic is brittle, misdetection causes broken nudges)
- **Rationale:** User knows what they're spawning. One question at /swarm-create is cheap. Wrong detection at runtime is expensive.

- **Decision:** Worker prompt gets explicit "tick-aware" instruction
- **Alternatives considered:** Watchdog-only (rejected — workers benefit from self-awareness too)
- **Rationale:** Reduces dependency on watchdog correctness; workers self-recover when they can.

### Deferred Decisions
- Codex / Copilot spinner regex calibration: defer until first integration test with those providers; ship v2 with regex stubs that fall back to pane-hash heuristic
- Cap-aware bd reassignment: v3 candidate
- Cross-machine watchdog: v3 candidate
- `--verify-cmd` flag: not selected; auto-detect + team.json edit is sufficient

## Implementation Notes

### File Layout (after v2 ship)

```
{{HOME_TOOL_DIR}}/skills/swarm-create/
├── SKILL.md                  (rewritten — v2 instructions)
├── watchdog.sh               (NEW — bash daemon)
├── finalize.sh               (NEW — notify-only finalize)
└── v2-spec.md                (this file)

{{HOME_TOOL_DIR}}/skills/swarm-attach-watchdog/
└── SKILL.md                  (NEW — retrofit command)

{{HOME_TOOL_DIR}}/utils/swarm-lib.sh  (extended with new functions)

{{HOME_TOOL_DIR}}/swarm/<team-id>/
├── team.json                 (v2 schema)
├── watchdog.state            (NEW — per-pane stuck counters)
├── watchdog.log              (NEW — tick log)
├── watchdog.pid              (NEW — daemon PID)
├── .finalized                (NEW — sentinel after finalize ran)
├── inbox/
│   ├── leader.jsonl
│   ├── agent-1.jsonl
│   └── ...
└── shared/
    ├── finalize-report.md    (NEW — written by finalize.sh)
    └── incidents.log         (NEW — for nudge failures)
```

### Priority Order

1. **Phase A — watchdog.sh** (highest priority, unblocks everything)
   - Cross-provider stuck/cap/active detection
   - Comprehensive nudge sequence
   - State persistence
   - Heartbeat
2. **Phase B — swarm-lib.sh extensions**
   - `swarm_detect_provider`, `swarm_detect_verify_cmd`, `swarm_spawn_watchdog`, `swarm_attach_watchdog`
3. **Phase C — /swarm-create SKILL.md rewrite**
   - New flags, auto-detect prompts, watchdog spawn
4. **Phase D — finalize.sh**
   - Verify run, report generation, dry-run merge, optional auto-PR
5. **Phase E — /swarm-attach-watchdog skill**
   - Read team.json, ask provider if missing, spawn watchdog
6. **Phase F — worker prompt augmentation**
   - Add tick-aware instruction to `swarm_spawn_agent` template

### Technical Debt Accepted
- Codex/Copilot spinner regexes are stubs in v2 (fall back to pane-hash). Calibrate in v3.
- No remote watchdog; single-host only.
- No cap-aware bd reassignment; cap == watchdog-skips-nudge.

## Open Questions

- [ ] Should `watchdog.sh` write to a structured log (JSONL) for future tooling, or stay plain-text? (Default: JSONL for parseability.)
- [ ] Finalize report — markdown only, or also write JSON for tooling? (Default: both — `finalize-report.md` + `finalize-report.json`.)
- [ ] Provider regex for Codex / Copilot — capture during first real-world spawn of each, or hold v2 until calibrated? (Default: ship with stubs + pane-hash fallback; calibrate in follow-up.)

## Next Steps (post-spec)

User decides A/B/C from prior recommendation:
- **A** — spec only (this file) — DONE
- **B** — full ship (watchdog.sh + finalize.sh + skill rewrite + /swarm-attach-watchdog) on toolkit repo
- **C** — incremental: build /swarm-attach-watchdog first, retrofit THIS active swarm, validate, then upstream

---

*This specification was generated through structured interview on 2026-05-15.*
