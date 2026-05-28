# Handover · HolyClaude as ainb container backend for bossmode

**Generated**: 2026-05-28 18:24
**Branch**: `feat/holyclaude` (volatile worktree)
**Last commit on branch**: `da76e94` (merge of worktree-explain-to-me-nobanana, unrelated to this work)
**Status**: Research + design exploration phase **complete**. Ready for `/interview` lock-in → `/plan` → `/implement`.
**Owner handoff**: Future-Stevie OR another contributor picking this up

---

## TL;DR

ainb's "boss mode" (non-interactive Docker-backed Claude tasks) is being refactored to back onto **HolyClaude** — a pre-built Docker image at `coderluii/holyclaude:v1.2.2` that bundles claude / codex / gemini / cursor / 3 more agent CLIs + CloudCLI web UI. Goal: stop maintaining ainb's own Dockerfile (`docker/agents-dev/`), pull HolyClaude as a release-version dependency, keep a clean trait-shaped abstraction so we can swap to something else later.

**Three explainers + a research doc** capture the full state. **Four decisions remain** before code can start.

---

## Artifacts produced

### Published explainers (here.now · permanent)

| # | Topic | URL | Local path |
|---|---|---|---|
| 1 | **Options paper** · HolyClaude integration trade-off · 4 tracks scored (host-side / wasm plugin / own image / do nothing) | https://sleek-dahlia-vxsn.here.now/ | `explainers/holyclaude-ainb-integration.html` |
| 2 | **Operating modes** · 8 modes for containerised coding agents w/ topology SVGs, capability matrix, hard constraints | https://lively-falcon-pfz4.here.now/ | `explainers/bossmode-operating-modes.html` |
| 3 | **CloudCLI streaming architecture** · How `siteboon/claudecodeui` actually plumbs chat + shell pipes | https://noble-raven-krzq.here.now/ | `explainers/cloudcli-streaming-architecture.html` |

### Research

| Document | Path | Purpose |
|---|---|---|
| HolyClaude integration research | `research/2026-05-15_18-08_holyclaude-as-ainb-plugin.md` | Full integration brief — what HolyClaude offers, ainb plugin contract mapping, integration tracks, risks |

---

## Decisions locked

| Decision | Lock | Why |
|---|---|---|
| **Backend**: HolyClaude as release-version Docker image dep | `coderluii/holyclaude:v1.2.2` pinned (env-overridable via `AINB_HOLYCLAUDE_TAG`) | Don't maintain Dockerfile; treat as third-party runtime |
| **Track**: Host-side `ainb container` subcommand (Track A from options paper) | Not the plugin route, not own image, not do-nothing | Plugin route blocked on Phase 7+ `host/process/spawn`; own image fights the stated goal |
| **Abstraction shape**: Rust trait (`BossBackend`) | Concrete impl `HolyClaudeBackend` lives behind the trait | Compile-time swap, easy test mocks; cross-language alts can shim later |
| **Tests**: Live Docker tests required | testcontainers-rs candidate; tagged so CI can skip if Docker unavailable | User constraint from initial brief |
| **Bind-mount paths**: under `~/.agents-in-a-box/holyclaude/` | `{compose.yaml, .env, claude/, workspace/}` — absolute paths in rendered compose | Per-user, shared across worktrees (volatile-worktree memory) |
| **OAuth wiring**: ainb stays out of it | Bind `~/.agents-in-a-box/holyclaude/claude/`, open browser to `:3001`, let CloudCLI handle login | Simplest path; CloudCLI's paste-code OAuth works in-container |

---

## Open decisions (need /interview before code)

Four blockers. Previous attempt at AskUserQuestion was redirected to re-clarify. Need to re-formulate based on what's now in the operating-modes explainer.

| # | Decision | Why it gates implementation |
|---|---|---|
| 1 | **Container granularity** — per-task ephemeral (mode 1) vs sticky single-task (mode 2) vs hybrid w/ `--keep-alive` flag | Drives every lifecycle decision: when to spawn, when to reap, how many CloudCLI ports exposed |
| 2 | **Day-1 mode coverage** — which of the 8 operating modes does v1 of `ainb container` support? My current recommendation: mode 1 default + `--keep-alive` for mode 2; modes 3–7 are later features | Scopes the v1 PR. Without this lock, scope creep is inevitable |
| 3 | **Alt-backend candidates** the trait must accommodate (multi-select): custom Dockerfile / local subprocess / devcontainers / cloud sandboxes | Shapes the trait API surface — what methods, what return types |
| 4 | **Test strategy organisation** — testcontainers-rs vs spawn-docker-compose-in-test vs both; CI tagging; how to handle "Docker not present" gracefully | Drives test crate layout and CI matrix |

**Recommended interview tool**: re-invoke `AskUserQuestion` with these 4 questions, each option carrying concrete negative impacts (per `feedback_options_with_negative_impacts.md` memory).

---

## Key research findings (5 most load-bearing facts)

### 1. HolyClaude is image + compose only — no CLI, no SDK

CoderLuii/HolyClaude publishes `coderluii/holyclaude:latest|:slim|:1.2.2` to Docker Hub. The product is the image + a `docker-compose.yaml` template. There is no `holyclaude` CLI, no library. Integration = `docker compose` + bind mounts + env vars. **Implication**: ainb shells out to `docker` directly — there's nothing else to bind to.

### 2. Current ainb bossmode location + execution model

- Creation: `crates/ainb-core/src/app/state.rs:7940-8030`
- Container entrypoint: `docker/agents-dev/scripts/startup.sh:139-170`
- Invocation: `claude --print --output-format stream-json --verbose "$ENHANCED_PROMPT"` (`startup.sh:150-160`)
- TUI log parsing: `crates/ainb-core/src/components/live_logs_stream.rs:506-602`
- Per-session ephemeral container w/ worktree bind mount
- **Critical**: Hardcoded prompt prefix concatenated to every user prompt (`startup.sh:150`) — design gap to fix in refactor

### 3. Recent bossmode fixes (May 2026)

Commits that addressed the "broken a bit" Stevie mentioned:
- `115dbe9` — decouple Boss + Interactive workspace loading (separate timeouts)
- `7ddc682` — cap Boss-mode load in manual refresh path (BOSS_MODE_TIMEOUT)
- `fb8277d` — don't mark orphaned worktrees as Boss sessions
- `09c3168` — O(N+M) workspace dedup + raw-path fallback

Path canonicalization in dedup is still fragile per the file diff. New refactor should harden it.

### 4. CloudCLI's surprise: Claude is NOT a subprocess

`siteboon/claudecodeui` (vendored as CloudCLI inside HolyClaude) calls the Claude SDK **in-process** via `@anthropic-ai/claude-agent-sdk`'s `query()` async generator. No PTY, no `spawn()`, no stdout piping for the chat pane. Only Gemini/Cursor/Codex spawn child processes. The shell tab uses real `node-pty`.

**Implication for ainb**: if Track A uses `docker exec claude -p`, you're bypassing CloudCLI entirely. CloudCLI's wire format only matters if you decide to proxy its WebSocket. The cleanest path is to ignore CloudCLI for non-interactive bossmode and only use it as an optional interactive escape hatch.

### 5. Session continuity works via bind-mounted jsonl

`claude` CLI's `-c` (continue) and `--resume <session-id>` read JSONL from `~/.claude/projects/<hash>/<uuid>.jsonl`. Because HolyClaude bind-mounts `~/.claude` from `./data/claude/` (which ainb will mount from `~/.agents-in-a-box/holyclaude/claude/`), session continuation **survives container restart**. Fire-and-forget mode + resume = effectively multi-turn without sticky containers.

---

## Path-A architecture sketch (for the eventual /plan)

```
~/.agents-in-a-box/
└── holyclaude/                           # per-user, shared across worktrees
    ├── compose.yaml                       # rendered by `ainb container up`
    ├── .env                               # HOLYCLAUDE_HOST_* + API keys if --api-key
    ├── claude/                            # → /home/claude/.claude (auth, memory)
    └── workspace/                         # → /workspace (code)

crates/ainb-core/src/
└── boss/
    ├── mod.rs                             # public BossBackend trait
    ├── trait.rs                           # trait def: spawn, stream, terminate, resume
    └── backends/
        ├── holyclaude.rs                  # HolyClaudeBackend impl
        └── (future) subprocess.rs         # local-fallback impl
```

**CLI surface (proposed for v1)**:
```
ainb container up [--profile slim|full] [--api-key <key>]   # render compose, docker pull, compose up -d
ainb container down                                          # docker compose down
ainb container exec -- <cmd>                                 # docker exec
ainb container logs --follow [--session <id>]                # docker logs / jsonl tail
ainb container ui                                            # open localhost:3001
ainb container update                                        # docker compose pull && up -d
ainb container status                                        # health + active session count
```

**Snapshot bus topic** to publish from day 1 even in Track A (so a future plugin migration is a swap, not a rewrite):
- `containers.status` — payload: `{id, image, state, ports, started_at, active_sessions}`

---

## Risks / things to watch

1. **GPL-3.0 vendored CloudCLI** inside HolyClaude image — pulling from Docker Hub is fine (third-party redistribution), but if ainb ever ships pre-built images derived from HolyClaude, legal review needed.
2. **5-week silent gap** on HolyClaude commits before April 2026 — pin to `:v1.2.2`, monitor cadence, have fallback plan (Track C from options paper = own thin Dockerfile).
3. **Restricted CI environments** — HolyClaude needs `SYS_ADMIN + SYS_PTRACE + seccomp=unconfined + shm_size: 2g` for Chromium. Pre-flight should detect and refuse cleanly on GitHub Actions, Fargate, Cloud Run, etc.
4. **CloudCLI patches** in HolyClaude are `sed`/`perl` against minified JS — fragile, break on every upstream CloudCLI version bump. Not ainb's problem directly but affects HolyClaude's stability.
5. **"Continue in Shell" CloudCLI button** permanently broken upstream (race condition in `useShellConnection.ts:197-203`). Document this in user-facing docs if we expose CloudCLI UI.

---

## Resumption instructions

For whoever picks this up — including future-me:

1. **Read** the three explainers in this order: options paper (decision context) → CloudCLI architecture (mental model) → operating modes (UX scope).
2. **Re-invoke the interview** to lock the 4 open decisions. Use `AskUserQuestion` directly — one batched call w/ all 4 questions, each option labelled with concrete negative impacts not generic pros/cons.
3. **Produce a /plan** based on the locked decisions. Likely milestone shape:
   - M1: BossBackend trait + HolyClaudeBackend skeleton, no real wiring
   - M2: `ainb container up` happy path (pull, compose up, status snapshot)
   - M3: docker exec wired for fire-and-forget mode
   - M4: log streaming via stream-json parser (re-uses existing `live_logs_stream.rs` logic)
   - M5: testcontainers integration tests, green CI
   - M6: deprecate `docker/agents-dev/` (or keep as fallback if mode coverage diverges)
4. **/implement** following the plan. Hand off to `superstar-engineer` agent if the Rust changes get large per the [delegate-large-rust-changes memory](~/.claude/projects/-Users-stevengonsalvez--agents-in-a-box-repos-github-com-stevengonsalvez-agents-in-a-box/memory/feedback_delegate_large_rust_changes.md).

---

## Files referenced

Canonical copies are committed in this self-contained bundle (here.now URLs are convenience mirrors that will eventually expire — the in-repo copies are authoritative):

```
docs/explorations/holyclaude-bossmode/
  README.md                                             (bundle index)
  handover.md                                           (this file)
  research/
    2026-05-15_18-08_holyclaude-as-ainb-plugin.md       (full integration brief, ~600 lines)
  explainers/
    holyclaude-ainb-integration.html                    (options paper, 4 tracks scored)
    bossmode-operating-modes.html                       (8 operating modes + matrix + Q&A)
    cloudcli-streaming-architecture.html                (siteboon/claudecodeui deconstruction)
```

Open an explainer locally: `open docs/explorations/holyclaude-bossmode/explainers/<file>.html`

Code paths in ainb to read before starting:
```
crates/ainb-core/src/app/state.rs:7940-8030             (current bossmode creation)
crates/ainb-core/src/components/live_logs_stream.rs:506-602  (JSON stream parsing — REUSE)
crates/ainb-core/src/components/new_session.rs:2804-3283     (bossmode UI flow)
docker/agents-dev/scripts/startup.sh:139-170            (current container entrypoint — DEPRECATE)
docker/agents-dev/Dockerfile                            (current image — DEPRECATE)
```

---

## Vocab cheat sheet

| Term | Meaning |
|---|---|
| **HolyClaude** | `CoderLuii/HolyClaude` Docker image bundling 7 agent CLIs |
| **CloudCLI** | `siteboon/claudecodeui` — the web UI vendored inside HolyClaude |
| **Track A** | Host-side `ainb container` subcommand (the picked option) |
| **Track B** | Wasm plugin (`ainb-plugin-holyclaude`) — blocked on Phase 7+ host/process/spawn |
| **bossmode** | ainb's non-interactive Docker-backed agent execution mode |
| **stream-json** | Claude CLI flag `--output-format stream-json` producing parseable event log |
| **Mode N** | Operating mode N from the operating-modes explainer (mode 1 = fire-and-forget, etc.) |
| **CTS** | Compatibility Test Suite — not directly relevant here but mentioned in plugin memories |

---

## Honest gaps in this handover

- **No interview answers captured yet** — the 4 decisions above are still open. Next session must lock them first.
- **No /plan exists** — only the architecture sketch above. /plan should run after interview.
- **HolyClaude project-health monitoring** not set up — would be useful to know if commits resume (last commit was 2026-04-10).
- **No PoC** — nothing has been built yet. Pure research + design.
