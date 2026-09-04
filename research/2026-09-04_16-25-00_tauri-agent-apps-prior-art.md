# Research: Open-source Tauri/Rust desktop apps that run multiple coding agents

**Date**: 2026-09-04 16:25:00
**Repository**: agents-in-a-box (worktree `agents-in-a-box--desktop-appc204d0f1--adea5f3b`)
**Branch**: desktop-appc204d0f1
**Commit**: 0c978090
**Research Type**: Web
**Companion**: `research/2026-09-04_14-10-02_desktop-app-shared-core.md` (section R3 covers Conductor, Vibe Kanban, cmux, Emdash, Superset, agent-deck, Multica, Sculptor; not repeated here)

## Research Question

Which open-source desktop apps on a similar stack (Tauri v2 + Rust core + web frontend) manage multiple coding agents, terminals, or worktree-per-task workflows, and what concrete engineering can `ainb-desktop` read and copy?

## Executive Summary

Eight OSS Tauri apps do some of what we want; none does all of it. opcode is the most adopted but pipes stdout rather than running a PTY. Termic is the closest sibling: `portable-pty`, worktree per task, WebdriverIO e2e, plus an in-binary egress proxy. Jean has the most complete worktree lifecycle. Codexia proves one Rust binary can be both the Tauri app and a headless web server, which is exactly our `ainb-web` + desktop overlap. Nobody has a documented command palette, and nobody shares a core with a TUI.

## Key Findings

- **Real PTY is the minority.** Only Termic (`portable-pty 0.8`) and Better Agent Terminal (`node-pty`) run a real PTY. opcode, OpenCovibe, and Codexia speak the agent's stream protocol instead. Our tmux-attach-over-WS path is more terminal-faithful than any of them.
- **Dual-mode binaries exist.** Codexia serves Tauri commands and a `/ws` broadcast from one binary; Jean runs a local HTTP server beside Tauri IPC for headless control. Both validate keeping `ainb-web`'s axum in-process for the desktop terminal.
- **Worktree lifecycle patterns to lift.** Jean: PR-checkout-as-worktree with auto-archive on merge. Termic: `~/termic/tasks/<project>/<name>/` layout plus broadcast-one-prompt-to-all.
- **Agent-done detection without heuristics.** Termic detects OSC 9;4 escape sequences to mark "agent finished". Relevant to our needs classifier's pane fallback.
- **Sandboxing is copyable.** Tempest ships bubblewrap / Seatbelt SBPL / Job Objects confinement; Termic ships an in-binary HTTPS CONNECT proxy for egress. `ainb-hangar-sandbox` already does Seatbelt; Tempest's Linux path is the missing half.
- **Rollback UX.** opcode's checkpoint/timeline system for undoing agent changes is novel and well-used (22k stars).
- **AskUserQuestion handling.** Claudette handles the pause/resume flow in-app; directly relevant to the ACP chat card.
- **Palette bindings are undocumented everywhere.** Jean confirms a Cmd+K palette exists; no app publishes its keymap. Our generated shortcuts page would be a differentiator.

## Detailed Findings

### External Research (fetched 2026-09-04)

| app | repo | stars | pushed | Tauri | frontend | terminal bytes | PTY | multi-session model | e2e | steal this |
|---|---|---|---|---|---|---|---|---|---|---|
| opcode (ex Claudia) | [winfunc/opcode](https://github.com/winfunc/opcode) | 22,395 | 2025-10-16 | 2 | React 18 + Vite 6 | spawns `claude`, pipes stdout (`src-tauri/src/claude_binary.rs`) | none | agents in separate processes (`src-tauri/src/process/`), per-agent permissions, no worktrees | `src-tauri/tests/` | checkpoint/timeline rollback (`src-tauri/src/checkpoint/`), visual sub-agent editor |
| Termic | [simion/termic](https://github.com/simion/termic) | 244 | 2026-09-04 | 2 | Vite/TS | PTY to xterm | `portable-pty 0.8` | worktree per task under `~/termic/tasks/`, broadcast prompt to all | `wdio.conf.ts` + vitest | in-binary HTTPS CONNECT egress proxy (`sandbox.rs`, `proxy.rs`); OSC 9;4 "agent finished" detection |
| Tempest | [tempestai-dev/tempest](https://github.com/tempestai-dev/tempest) | 166 | 2026-09-02 | 2 | Vite/TS | unconfirmed | unconfirmed | worktree + branch per agent (Claude Code, Aider, OpenCode, Copilot, Cline, Goose), shared code-knowledge graph | unconfirmed | three-way OS sandbox: bubblewrap, Seatbelt SBPL, Job Objects (`agent_hooks.rs`, `claude_bridge.rs`, `tasks_store.rs`) |
| Jean | [coollabsio/jean](https://github.com/coollabsio/jean) | 1,254 | 2026-09-03 | 2 | React 19 + Zustand + xterm.js | xterm over Tauri IPC, plus local HTTP server (`src-tauri/src/http_server.rs`) | unconfirmed | full worktree lifecycle, N sessions per worktree, PR checkout as worktree, auto-archive on merge | Playwright (`e2e/`) | PR-to-worktree + auto-archive state machine; Cmd+K palette with connection switching |
| Codexia | [milisp/codexia](https://github.com/milisp/codexia) | 910 | 2026-09-01 | 2 | React + Zustand + shadcn | Tauri commands and `/ws` broadcast (`src-tauri/src/web/router.rs`) | unconfirmed | Codex + Claude Code + any ACP agent; sessions persist across restarts | vitest only | one binary = Tauri app and headless web server |
| Better Agent Terminal | [tony1223/better-agent-terminal](https://github.com/tony1223/better-agent-terminal) | 493 | 2026-09-04 | 2 | React 18 + i18next | xterm via `host-api.ts` over IPC | `node-pty` | multi-workspace, tabs, worktree isolation, sidecar bridge | `test.sh` | sidecar-bridge process model for the agent SDK beside the PTY host |
| OpenCovibe | [AnyiWang/OpenCovibe](https://github.com/AnyiWang/OpenCovibe) | 262 | 2026-08-26 | 2 | Svelte 5 + SvelteKit | none, speaks Claude Code stream-JSON and Codex app-server JSON-RPC | none | per-run session actors, resumable, forkable, Team Dashboard | vitest | protocol-native, non-PTY reference |
| Claudette | [utensils/claudette](https://github.com/utensils/claudette) | 75 | 2026-09-01 | 2 | React | unconfirmed | unconfirmed | worktree per agent, checkpoints, workspace forking, AskUserQuestion handling | unconfirmed | AskUserQuestion pause/resume |

Also surfaced, not assessed: [Tura-AI/tura](https://github.com/Tura-AI/tura).

Ranked by "worth reading the code": opcode, Termic, Tempest, Jean, Codexia, Better Agent Terminal, OpenCovibe, Claudette.

### What maps onto the ainb-desktop spec

| spec item | read first | why |
|---|---|---|
| WS terminal + `PtyBridge` reuse | Termic (`portable-pty`), Codexia (`/ws` broadcast) | same PTY crate, same dual-plane split |
| WebdriverIO e2e on Tauri | Termic `wdio.conf.ts` | a working config on a terminal-heavy Tauri app |
| Playwright on Tauri | Jean `playwright.config.ts` | alternative harness if wdio disappoints |
| worktree lifecycle, PR as worktree | Jean | auto-archive on merge fits the hangar task FSM |
| ACP chat card, AskUserQuestion | Claudette, OpenCovibe | protocol-native rendering of question and permission turns |
| needs classifier pane fallback | Termic OSC 9;4 detection | one escape sequence beats scraping |
| Linux sandbox for `ainb-hangar-sandbox` | Tempest bubblewrap flags | `--unshare-pid --die-with-parent --unshare-net` |
| rollback UX (v2) | opcode `checkpoint/` | timeline of agent edits with restore |

## Code References

None in this repo; all references are external paths listed in the table above.

## Recommendations

1. Before P3 (hangar host + desktop terminal), clone Termic and Codexia into `explorations/` and read `sandbox.rs`, `proxy.rs`, `web/router.rs`; copy the WS frame handling only where it beats `ainb-web/src/terminal.rs`.
2. Lift Termic's OSC 9;4 detection into `ainb-fleet-core`'s pane fallback classifier; it is orthogonal to the desktop and helps the TUI today.
3. Use Termic's `wdio.conf.ts` as the starting point for the `@wdio/tauri-service` harness.
4. Read Jean's worktree lifecycle before designing the "PR as task" board column; do not build it in v1.
5. Skip opcode's stdout-piping model; keep tmux attach. Revisit its checkpoint system only for v2 rollback.

## Open Questions

- Termic's and Jean's exact palette keybindings (needs a source clone; no docs).
- Whether Jean's local HTTP server and Codexia's `/ws` authenticate at all; if not, do not copy their bind policy, keep `ainb-web`'s.
