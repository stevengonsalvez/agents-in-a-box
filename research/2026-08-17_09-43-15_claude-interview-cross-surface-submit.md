# Research: Claude Interview Answers Fail from Fleet and macOS

**Date**: 2026-08-17 09:43:15
**Repository**: agents-in-a-box--f-interview-surface--8a7ee35c
**Branch**: feat/daemon-version-health
**Commit**: ac532101
**Research Type**: Codebase and live-runtime investigation

## Research Question

Why does a Claude `/interview` appear in native Claude, Fleet, and the macOS app, but answers submitted from Fleet or macOS remain stuck and never resume native Claude?

## Executive Summary

Both external submissions reached Hangar with the correct session version and request fingerprint. Both failed inside the shared mirrored-picker driver because it uses the old `Down`/`Space`/`Enter` interaction model and never navigates between Claude's new question tabs. macOS then hides the terminal `FAILED` receipt by unconditionally claiming delivery; Fleet TUI has the correct failure reducer, but lacks a strong response-loss convergence path.

## Key Findings

- Live database contains two terminal `FAILED` receipts for the same exact Claude request: one macOS request and one Fleet TUI request. Both failed with `visible picker prompt does not match current question; native picker already advanced 2 key(s)`.
- Current Claude 2.1.233 picker says `Tab/Arrow keys to navigate`. Hangar's driver answers question 1, then immediately verifies question 2 without sending `Tab` or `Right`.
- macOS records the failed receipt, ignores its status, and displays `Delivered. Confirming Fleet state.`
- Current mirrored interview path deliberately bypasses approval broker. Stale notifyd exists and must be repaired, but did not cause this exact failure.
- Existing tests validate a generated old key array or historical blocking-broker path. No test proves a real four-tab native Claude picker resumes after Fleet/macOS submission.

## Prior Learnings

| Learning | Key Insight | Confidence |
|----------|-------------|------------|
| Fleet orchestration read/write split | Reads may come from transcripts and pane capture, but writes must reach the exact live tmux session and be verified from resulting pane state. Broker is fallback. | high |

## Detailed Findings

### Codebase Analysis

#### 1. Shared delivery failure is mirrored native-picker automation

Current AskUserQuestion hook stamps `fleet_delivery = mirrored`, publishes the Fleet event, and returns no override so Claude retains its native picker. Hangar therefore bypasses structured broker answering and enters `execute_claude_mirrored_picker` ([ATC hook route](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-core/src/cli/fleet/atc.rs#L1641), [mirrored branch](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs#L5070)).

Driver flattens every answer into `Down`, `Space`, and `Enter` keys, then appends final submit. It emits no `Tab` or `Right` transition between questions ([step builder](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs#L5226)). Before each key, verifier demands current question text/options remain visible ([step verifier](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs#L5310), [question verifier](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs#L5380)).

Live question 1 is multi-select. Driver sends `Space`, then `Enter`; next step assumes question 2 is visible. Claude 2.1.233 remains on tab 1 and expects explicit tab/arrow navigation, so verification fails after exactly two keys. This matches both durable receipt details.

Additional fragility:

- fixed 150 ms delay rather than waiting for expected state;
- cursor assumed to begin at option zero;
- pane verified before key, never after resulting transition;
- successful `tmux send-keys` treated as step delivery;
- partial mutation has no rollback or resumable cursor/selection state;
- separate clients can retry same version/fingerprint with new request IDs and mutate picker again.

Broken driver originated in `96429f00`; `9359344e` hardened timing/failure details; `e4e04d16` fixed one-question single-select only. Commit `b374f2af` taught visibility detector to recognize `Tab/Arrow keys`, but did not change navigation ([footer detector](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs#L5774)).

#### 2. Live evidence proves RPC identity and routing worked

Live session:

- session: `claude:b205ea04-49a3-4373-8fe5-1457dd90caa4`
- version: `11`
- request fingerprint: `fnv1a64:c0fcdb54cb22d9f2`
- tmux target: `tmux_agents-in-a-box--f-interview-test-4--1fa2614a_f_interview-test-4:1.1`
- Claude: `2.1.233`

Durable receipts:

| Surface | Request ID | Status | Detail |
|---------|------------|--------|--------|
| macOS | `EB12F418-3721-4AD8-AB1E-FFD943EF7FF6` | `FAILED` | visible picker prompt does not match current question; native picker already advanced 2 key(s) |
| Fleet TUI | `fleet-host-f27ed18b-31a3-4a51-816c-24909c3e4609` | `FAILED` | visible picker prompt does not match current question; native picker already advanced 2 key(s) |

Both actions passed session/version/fingerprint checks and reached native tmux execution. Broker pending list was empty, expected for mirrored route. Native picker remained open on first of four tabs.

#### 3. macOS presents failed receipt as success

macOS builds correct structured action with session version, request fingerprint, request identity, and full answers ([action construction](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/apps/ainb-fleet-macos/Sources/App/FleetStore.swift#L333), [wire encoding](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/apps/ainb-fleet-macos/Sources/FleetRPC/FleetWire.swift#L420)).

After RPC returns, `performStructured` records receipt but never switches on `receipt.status`; it always sets `Delivered. Confirming Fleet state.` ([status bug](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/apps/ainb-fleet-macos/Sources/App/FleetStore.swift#L403)). Contract explicitly supports `PENDING`, `DELIVERED`, `FAILED`, `UNKNOWN`, and `REJECTED` ([receipt contract](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/apps/ainb-fleet-macos/Sources/FleetRPC/FleetWire.swift#L495)).

Generic macOS RPC request also has no bounded timeout. Lost response on a still-open socket can retain `pendingIntentID` indefinitely ([connection request](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/apps/ainb-fleet-macos/Sources/FleetRPC/FleetConnection.swift#L287)).

#### 4. Fleet TUI handles terminal failure better, but needs convergence proof

Fleet TUI constructs exact answers and request identity ([submit builder](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-plugin-hangar/src/screen/fleet.rs#L1862)). Host/plugin maps only `DELIVERED` to success and maps every other receipt status to `ActionFailed` ([plugin response](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-plugin-hangar/src/plugin.rs#L2104), [core host response](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-core/src/app/events.rs#L3389)). Reducer resets `Confirming` to `Ready` on failure ([failure reducer](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-plugin-hangar/src/screen/fleet.rs#L1146)).

Therefore a literal persistent Fleet `submitting` label implies response/redraw did not converge, separate from daemon delivery failure. Current progress wording also says `broker accepted` even on mirrored route, which is misleading ([progress copy](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-plugin-hangar/src/screen/fleet.rs#L3715)).

#### 5. Runtime skew is real but secondary

Live process inventory:

- Hangar: Homebrew Ainb `1.21.1`, started Aug 17;
- notifyd/approval broker: dev Ainb `1.20.5` from unrelated checkout, started Aug 12;
- installed hook pointer: this worktree dev Ainb build from `e7150e04`, reports `1.20.5`;
- Claude: `2.1.233`.

Old notifyd owns `approve.sock`, but mirrored Ask route does not use broker. Current event and failed receipts prove hook event production and Hangar routing worked. Skew remains dangerous for intercepted interviews/approvals and validates daemon-version/doctor repair work.

#### 6. Tests encode obsolete behavior

Current multi-question unit test asserts old key array, including no inter-tab navigation ([old sequence test](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs#L4452)). It passes locally, demonstrating false confidence rather than correct current behavior: `1 passed; 0 failed`.

Fleet integration tripwire proves historical blocking-hook broker output, not current mirrored native picker ([broker tripwire](https://github.com/stevengonsalvez/agents-in-a-box/blob/ac532101/ainb-tui/crates/ainb-core/tests/tripwire_fleet_panel_opens_and_answers.rs#L475)). No macOS test checks failed structured receipt presentation. No cross-surface E2E drives current four-tab Claude picker through answer, review, submit, native resume, and ASK clearing.

### Documentation Insights

- Recent release/history describes mirrored interviews and footer recognition, but coverage stopped at event visibility, generated key sequences, and stale-card cleanup.
- Existing architecture epic `agents-in-a-box-igd` already targets Hangar as single broker/source for ASK, interviews, and approvals. This failure is direct evidence for replacing version-sensitive picker scripting with acknowledged lifecycle control.

### External Research

- Claude Opus xhigh investigation completed through tracked `cc` job `task-mswz5xrq-88lvs3`. Result remains available through `$cc:result task-mswz5xrq-88lvs3` under `cc` workflow.

## Code References

- `ainb-tui/crates/ainb-core/src/cli/fleet/atc.rs:1641` — mirrored hook route leaves native picker open.
- `ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs:5070` — mirrored branch bypasses broker.
- `ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs:5226` — obsolete flattened picker key plan.
- `ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs:5310` — pre-key verification and tmux send.
- `ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs:5774` — new footer detected without new navigation behavior.
- `apps/ainb-fleet-macos/Sources/App/FleetStore.swift:403` — terminal receipt status ignored.
- `apps/ainb-fleet-macos/Sources/FleetRPC/FleetConnection.swift:287` — unbounded request continuation.
- `ainb-tui/crates/ainb-plugin-hangar/src/screen/fleet.rs:1146` — TUI intended failure reset.
- `ainb-tui/crates/ainb-hangar-daemon/src/rpc/mod.rs:4452` — obsolete key-array unit test.

## Recommendations

1. Replace flattened key script with observed picker state machine: verify current tab/cursor/selections, apply one transition, poll until expected post-state, explicitly navigate `Tab`/`Right`, verify review page, submit, then confirm picker disappears or request fingerprint clears.
2. Serialize structured actions per session/request fingerprint. Do not permit blind retry after partial mutation; reconcile native selected state first or require native completion.
3. Keep receipt `PENDING`/accepted until Claude actually resumes. `tmux send-keys` success is transport acceptance, not answer delivery.
4. Make macOS branch on terminal receipt status immediately. Only `DELIVERED` may show confirmation; surface daemon detail for `FAILED`/`REJECTED`/`UNKNOWN`, and add bounded RPC timeout.
5. Add real four-question current-Claude tmux E2E covering both Fleet and macOS contracts, multi-select, tab navigation, review, submit, transcript tool result, ASK clear, duplicate surface race, and partial-failure retry.
6. Restart/repair stale notifyd against current Ainb after preserving live-session safety. Doctor/daemon screen should expose this skew and offer controlled restart.

## Open Questions

- Can Claude hook protocol remain blocking while native picker also renders, avoiding tmux automation entirely? Current mirrored route chooses native visibility over acknowledged answer delivery.
- Which stable native picker signals expose current tab, cursor, and checked selections across Claude releases?
- Should first terminal structured action claim request ownership so second surface becomes observer until completion?
- Should TUI recover from lost action response by querying receipt ID and resetting local `Confirming` state?
