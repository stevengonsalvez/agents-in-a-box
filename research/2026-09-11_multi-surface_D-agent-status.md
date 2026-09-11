Mode: focused-query

# Reference Implementation Analysis — agent status detection and the dashboard

**Repo** local clone of the reference implementation (a desktop "run many coding agents in parallel" app)
**Commit** `9aa0f7e77d366c23a3cc8de2da32ae550d397dc0`, authored `2026-09-11 12:36:35 +0000`
**Research query** How does the reference implementation know an agent's status (working / waiting for input / finished / errored / idle) reliably across many agent CLIs, and how is that surfaced as a dashboard?
**Analyzed** 2026-09-11
**Citations** all paths are relative to the clone root. Every substantive claim is tagged `[fact]` (I read the cited code) or `[inference]`.

> Note on quoted strings: the tooling that mirrored this repo into my session redacts some long identifiers and string literals (they appear as `n` / `ln` in raw greps). Where an exact literal matters I say so and point at the file:line instead of pasting it.

---

## 0. The answer in one sentence

It does **not** classify terminal text. It makes each agent CLI *push* a structured status event over authenticated loopback HTTP into a single main-process store, and everything else (sidebar, dashboard, CLI `ps`, mobile, notifications, dock/tray) is a subscriber to that one store. Terminal text is used only for a small set of named fallbacks. `[fact]` — `src/main/agent-hooks/server/server-lifecycle.ts:53-157`, `docs/reference/agent-status-store.md:43-83`

---

## 1. Architecture, diagram first

```
                        ┌──────────────────────────────────────────────────┐
                        │        ONE agent CLI process in ONE pane         │
                        │  env: PANE_KEY, TAB_ID, WORKTREE_ID,             │
                        │       LAUNCH_TOKEN, HOOK_PORT, HOOK_TOKEN,       │
                        │       HOOK_ENDPOINT (file), HOOK_VERSION         │
                        └───┬───────────┬───────────────┬──────────────────┘
                            │           │               │
             (1) managed hook script    │ (3) PTY bytes │ (4) OSC title
                 / native plugin        │  OSC 9999     │    e.g. "✻ Claude"
                            │           │  JSON frame   │
                            ▼           ▼               ▼
        ┌───────────────────────────────────────────────────────────────────┐
        │  POST http://127.0.0.1:<ephemeral>/hook/<source>                  │
        │  hdr: X-...-Hook-Token: <uuid minted per server start>            │
        │  body: form-urlencoded  OR  raw JSON + base64 metadata header     │
        └────────────────────┬──────────────────────────────────────────────┘
                             │        ┌──────────────────────────────┐
       (2) spool file  ──────┤        │ relay (SSH / WSL host)       │
       when nobody listening │        │ normalizes, then JSON-RPC    │
                             │        │ `agent.hook` notification    │
                             │        └──────────────┬───────────────┘
                             ▼                       ▼
 ┌───────────────────────────────────────────────────────────────────────────┐
 │  SHARED LISTENER  src/shared/agent-hook-listener/                         │
 │  request-body → hook-envelope → source-routing → provider-dispatch        │
 │  per-provider normalizer (18 sources) → ParsedAgentStatusPayload          │
 │  { state: working|blocked|waiting|done, prompt, toolName, toolInput,      │
 │    interactivePrompt, lastAssistantMessage, model, subagents[], ... }     │
 └───────────────────────────────┬───────────────────────────────────────────┘
                                 ▼
 ┌───────────────────────────────────────────────────────────────────────────┐
 │  THE STORE (main process)  src/main/agent-hooks/server/                   │
 │                                                                           │
 │   ingest*: hook / remote / terminal(OSC) / structured                     │
 │       │                                                                   │
 │       ▼  getAgentStatusDisposition → accept | restart | suppress          │
 │       ▼  applyNormalizedStatus  ── the ONLY writer                        │
 │          · resolveAgentStatusIdentity  (agent-type inheritance)           │
 │          · monotonic clock vs connection watermark                        │
 │          · attachStatusTiming  (receivedAt / evidenceObservedAt /         │
 │                                 stateStartedAt)                           │
 │          · stampObservation    (origin, authorityId, incarnation,         │
 │                                 revision, boundary, kind)                 │
 │          · lastStatusByPaneKey.set(paneKey, enriched)                     │
 │          · scheduleStatusPersist (250 ms debounce → last-status.json)     │
 │          · notifyStatusChangeListeners + emitEnrichedStatus               │
 └───────┬───────────┬────────────┬─────────────┬────────────┬───────────────┘
         │           │            │             │            │
         ▼           ▼            ▼             ▼            ▼
   agentStatus:set  worktree ps  mobile        power-save   plugins /
   IPC → renderer   (CLI)        session.tabs  blocker      stats recorder
         │                        + push
         ▼
 ┌───────────────────────────────────────────────────────────────────────────┐
 │  RENDERER (zustand)  agentStatusByPaneKey                                 │
 │  33 ms burst → ONE transaction → ONE publication                          │
 │  reader-side policy only: 30-min decay, ack, dismissal, unread            │
 └───────┬──────────┬───────────┬──────────────┬─────────────────────────────┘
         ▼          ▼           ▼              ▼
   sidebar rows  dashboard   dock badge /    completion coordinator
   + spinner     buckets     tray dot        → OS notification / sound / push
                 attention|working|done|idle
```

---

## 2. Relevance to the query

The query has two halves and the reference implementation answers them with two different mechanisms:

| Half of the question | Mechanism | Where |
|---|---|---|
| "How does it KNOW the status" | Push from the agent CLI over authenticated loopback HTTP, normalized per provider, plus four named fallbacks | `src/shared/agent-hook-listener/`, `src/main/agent-hooks/` |
| "How is it surfaced as a dashboard" | One main-process store fanned out over IPC into a zustand map; every surface is a pure projection of that map | `src/main/agent-hooks/server/`, `src/renderer/src/store/slices/agent-status*.ts`, `src/renderer/src/components/dashboard/` |

The contrast with pane-text regex classification is stark and deliberate. The header of `src/shared/agent-status-types.ts:1-3` states the rule outright: status comes from hooks, **never** inferred from terminal titles, with one narrow interrupt fallback. `[fact]`

---

## 3. Key findings

### 3.1 The transport: loopback HTTP with a per-start bearer token

`[fact]` `src/main/agent-hooks/server/server-lifecycle.ts:36-157`

- `this.token = randomUUID()` on every `start()`; the listener binds `listen(0, '127.0.0.1')`, i.e. an ephemeral port on loopback only.
- Every request must be `POST` (else 404) and must carry the token header (else 403), checked **before** the body is read: "authenticate before spending work reading an untrusted body" (`:59-64`).
- Request time is capped by a slowloris guard, and a self-inflicted destroy is tracked separately so the cap cannot be misread as external interference (`:65-71`, `:121-131`).
- Order of startup is load-bearing: hydrate `last-status.json` → capture hydrated authority commitments → drain the offline spool → **then** bind the listener, so replay can never race a live hook (`:39-52`).
- On malformed payloads the server **fails open** with `204`: "a broken hook never blocks the agent" (`:128-130`). This matters because several CLIs treat a non-zero hook exit or empty stdout as a hard deny.

The port/token pair is published two ways so a PTY that outlives a restart can recover:

| Channel | Detail | Cite |
|---|---|---|
| PTY env | `PANE_KEY`, `TAB_ID`, `WORKTREE_ID`, `LAUNCH_TOKEN`, `HOOK_PORT`, `HOOK_TOKEN`, `HOOK_ENV`, `HOOK_VERSION`, `HOOK_ENDPOINT` injected at spawn | `src/main/ipc/pty/ipc/spawn-env.ts:24-120`, `src/main/startup/main-process-runtime-service.ts:130` |
| Endpoint file | Atomically written `endpoint.env` (POSIX) / `endpoint.cmd` (Windows) under userData, mode `0600` in a `0700` dir, so a hook script can `. "$file"` / `call "%file%"` natively and pick up a **new** port/token after an app restart | `src/shared/agent-hook-listener/endpoint-publication.ts:9-86` |

The endpoint file values are validated against a shell-safe charset before write, and the writer refuses rather than emitting something a sourced shell could misparse (`endpoint-publication.ts:14-54`). `[fact]`

`[fact]` The posted body has two shapes, chosen by a transport env var (`src/main/agent-hooks/hook-post-command.ts:10-36`):
- **raw JSON** with pane metadata packed into a base64 header (unit-separator joined), used when `base64` and `tr` exist;
- **form-urlencoded** fallback with `paneKey`, `tabId`, `launchToken`, `worktreeId`, `env`, `version`, `payload` as fields.

Both use `--noproxy 127.0.0.1`, `--connect-timeout 0.5`, `--max-time 1.5`. So a wedged listener costs the agent at most ~1.5 s per event.

### 3.2 The offline spool: hooks do not lose events when the app is down

`[fact]` `src/shared/agent-hook-spool.ts:1-60`, `src/main/agent-hooks/hook-stdin-contract.ts`

When the endpoint is unreachable, the POSIX hook script appends a JSON line to a per-pane spool file instead of dropping the event. Caps: 5 MiB per file, 1024 files, 7-day age. On the next `start()` the server drains the spool **before** binding, and fences each record against the persisted launch-token hash for that pane so a stale generation cannot overwrite hydrated state (`server-lifecycle.ts:46-51`). A torn trailing line is left unconsumed so a writer mid-flush is not truncated away (`agent-hook-spool.ts:55-60`).

This is a genuinely good idea and cheap: the spool reader reconstructs exactly the HTTP body shape (`buildSpoolHookBody`, `agent-hook-spool.ts:34-45`), so there is one ingest path, not two.

### 3.3 The provider layer: 18 wire sources, one normalized payload

`[fact]` `src/shared/agent-hook-relay.ts:37-58` declares the source enum; `src/shared/agent-hook-listener/source-routing.ts:6-25` maps URL pathname → source. The 18 sources:

`claude`, `codex`, `gemini`, `antigravity`, `amp`, `opencode`, `mimo-code`, `cursor`, `pi`, `omp`, `prime-agent`, `droid`, `command-code`, `grok`, `copilot`, `hermes`, `devin`, `kimi`.

Dispatch is an **exhaustive `switch`** in `src/shared/agent-hook-listener/provider-dispatch.ts:51-151`, and three other per-provider switches are deliberately exhaustive for the same reason, stated in comments: "a new AgentHookSource fails typecheck here instead of falling through to false" (`provider-event-routing.ts:39`, `:137`). `[fact]` That is the single most copyable structural decision in this subsystem: adding a CLI is a compile error until you answer every per-provider question.

The three questions each provider must answer:

| Function | Question | Cite |
|---|---|---|
| `normalize<Provider>Event` | event name → `working \| blocked \| waiting \| done \| null` | `providers/*-events.ts` |
| `isNewTurnEvent(source, eventName)` | is this a user-initiated turn boundary? | `provider-event-routing.ts:38-82` |
| `extractToolFields(source, eventName, payload)` | which payload fields carry the tool name / input / assistant text? | `provider-event-routing.ts:131-174` |

`null` is a first-class answer meaning "this event says nothing about status" — used heavily to avoid phantom rows.

### 3.4 The status vocabulary and state machine

`[fact]` `src/shared/agent-status-types.ts:26-27`

```
AGENT_STATUS_STATES = ['working', 'blocked', 'waiting', 'done']
```

There is **no explicit `idle` and no explicit `error` state on the wire.** Both are derived:

```
                    hook says                       reader derives
   ┌──────────┐   working   ┌──────────┐
   │  (none)  │────────────▶│ working  │──┐
   └──────────┘             └────┬─────┘  │  stale > 30 min
        ▲                        │        │  ┌──────────────────────┐
        │                   done │        └─▶│ unverifiable         │ pane still has live PTY
        │                        ▼           │ "No update in 34m"   │
        │                   ┌──────────┐     └──────────────────────┘
        │                   │  done    │     ┌──────────────────────┐
        │                   └──────────┘  └─▶│ idle                 │ no live PTY, or
        │                        ▲           └──────────────────────┘ restoredUnconfirmed
        │        blocked/waiting │
        │                   ┌──────────┐
        └───────────────────│ waiting  │  ⟵ AskUserQuestion, permission gate
            answered/escape │ blocked  │
                            └──────────┘
```

- `working` / `done` are the turn boundary. `[fact]`
- `waiting` and `blocked` are **both** "needs a human"; providers pick one. Claude, Codex, Grok, Droid, Kimi and the OpenCode family emit `waiting` (`providers/claude-events.ts:109`, `codex-events.ts:123`, `grok-events.ts:52`, `droid-events.ts:37`, `kimi-events.ts:42`, `opencode-family-events.ts:27`); Copilot and the Pi family emit `blocked` (`copilot-events.ts:40`, `pi-family-events.ts:42`). Both land in the same dashboard bucket and the same attention class, so the split is cosmetic at the reader. `[fact]` `src/renderer/src/components/sidebar/smart-attention.ts:161-165`, `src/renderer/src/components/dashboard/dashboard-card-bucket.ts`
- **`errored` is not a state.** Error handling is folded into `done` with flags, or into `working` when recoverable. Copilot's `ErrorOccurred` maps to `working` if `recoverable === true`, else `done` (`copilot-events.ts:52-56`). Claude-family `StopFailure` maps to `done` — registered explicitly because one CLI variant "skips normal Stop hooks after API/model errors and emits StopFailure instead; without this hook ref leaves the turn spinning" (`src/main/claude/hook-settings.ts:50-57`). `[fact]`
- `interrupted?: true` marks a `done` reached by cancellation rather than completion (`agent-status-types.ts:140-141`). Interrupted `done` is deliberately **not** attention and fires no notification (`smart-attention.ts:165-170`).
- `sessionBoundary?: true` marks a `done` that is a session connect/resume/clear landing idle, **not** a completed turn. "Consumers that react to completions (notifications, automation runs, unread badges, finished timestamps) must ignore it" (`agent-status-types.ts:180-187`). `[fact]` This is the cleanest single idea in the whole model: without it, every resumed session would fire a fake "agent finished" alert.
- `workingMode: 'monitoring'` marks work that does not need foreground agent execution (background tasks / crons still registered); it renders as a distinct icon rather than a spinner (`agent-status-types.ts:90-92`, `src/renderer/src/components/sidebar/StatusIndicator.tsx:50-59`).

### 3.5 Three timestamps, not one — and why

`[fact]` `src/main/agent-hooks/server/server-status-application.ts:19-80`, `src/shared/agent-status-freshness.ts:1-54`

| Stamp | Meaning | Why separate |
|---|---|---|
| `receivedAt` / `updatedAt` | delivery/ordering clock; forced monotonic against a per-connection watermark | Renderer rejects older rows with a strict `<`; a relay reconnect must restamp to stay monotonic |
| `evidenceObservedAt` | when the evidence was actually observed | A replay restates old evidence. Measuring staleness against `updatedAt` would push the deadline out by a full window on every reconnect (`server-status-application.ts:55-60`) |
| `stateStartedAt` | when the current `state` was first reported | Tool/prompt pings reset `updatedAt` but must not move the "working since" clock (`agent-status-types.ts:100-101`) |
| `mirroredEvidenceReceivedAt` | replica-local receipt clock for a row mirrored from another host | Subtracting a remote host's stamps from local `now` is off by clock skew in an unknown direction (`agent-status-freshness.ts:19-28`) |

Freshness is one function: `isFreshNonDoneAgentStatus` (`agent-status-freshness.ts:31-54`), gate `AGENT_STATUS_STALE_AFTER_MS = 30 * 60 * 1000`. A row is fresh iff it is not `done`, not `restoredUnconfirmed`, and either host-owned-structured or within the 30-minute window.

### 3.6 Debouncing and batching, at three layers

| Layer | Mechanism | Value | Cite |
|---|---|---|---|
| Disk persist | trailing-edge debounce, plus a synchronous flush at quit | 250 ms | `server-constants.ts:15`, `server-lifecycle.ts:161-162` |
| Renderer IPC | first event immediate; events inside the window folded into **one** zustand transaction → one publication | 33 ms | `docs/reference/renderer-agent-status-performance.md:124-131` |
| Statusline (usage) | per-pane client-side floor on the usage POST | 15 s | `src/shared/claude-statusline-rate-limits.ts:7-10` |
| Completion notify | quiet window before a `done` becomes a notification, cancellable by later work | 1500 ms | `src/renderer/src/components/terminal-pane/agent-completion-notification-controller.ts:15`, `:262` |
| Interrupted-`done` suppression | late `working`/`done` after a Ctrl-C is swallowed | 15 s | `server-constants.ts:8`, `server-status-update.ts:145-171` |

The renderer batching is measured, not asserted. The benchmark in `docs/reference/renderer-agent-status-performance.md:278-296` reports, for one 2,000-update burst at 100 mounted worktrees:

| Measure | Sequential | Batched |
|---|---|---|
| status publications | 2,000 | 1 |
| store action time | 3,692.0 ms | 188.7 ms |
| renderer mean CPU | 36.2% | 2.9% |
| p95 long task | 4,653 ms | 216 ms |

The stated cost model is `burst work ≈ status events × store listeners × selector work` (`:27-30`), and the measured baseline listener count at that scale was **9,279** (`:235-247`). Both halves get attacked: batching kills the first factor, cohesive shallow-equality selectors cut the second to a pinned per-surface budget (`:68-88`).

### 3.7 Terminal escape sequences: what is and is not used

I searched for OSC 133 (semantic prompt), OSC 633 (VS Code shell integration), OSC 9;4 (progress), and BEL.

| Sequence | Used for agent status? | Evidence |
|---|---|---|
| **OSC 9999** (custom) | **Yes.** The one structured in-band channel. Payload is the same canonical JSON as the HTTP body | `src/shared/agent-status-osc.ts:5,121`; origin enum member `'osc'` documented as "Canonical payload, no provider normalizer" (`src/shared/agent-status-observation.ts:15`) |
| **OSC 0/2 (title)** | Only as the weakest fallback and for readiness waits | `src/shared/agent-title-status.ts`, origin `'title'` = "The weakest evidence ref acts on" (`agent-status-observation.ts:17`) |
| OSC 133 | No. Grep finds no prompt-mark handling for status | `[fact]` searched `src/` |
| OSC 633 | No | `[fact]` searched `src/` |
| OSC 9;4 | Appears **only** inside recorded PTY fixtures (progress bars painted by agent TUIs), never parsed for status | `[fact]` matches confined to `src/main/runtime/__fixtures__/*.txt`, `src/shared/__fixtures__/*` |
| BEL `\x07` | OSC terminator, and a separate `terminal-bell` notification source; not a status signal | `agent-status-osc.ts:38`, `src/main/ipc/notifications.ts:113-118` |

The OSC 9999 parser is worth copying on its mechanics alone `[fact]` `src/shared/agent-status-osc.ts:57-131`:

- It is a **stateful, resumable** parser: a frame split across PTY chunks is carried in `pending`, and `pendingSearched` records how far the terminator search already failed, so a frame spread over many chunks re-scans only new bytes instead of the whole accumulation.
- Fast path: if there is no pending carry and the chunk does not contain the prefix, it returns the input by identity — no clean-data string is rebuilt for ordinary output.
- A partial prefix at a chunk boundary is detected by a cheap last-character test against the four bytes that can begin the marker, before any substring work.
- `MAX_PENDING = 64 KiB`; an over-long unterminated frame is dropped, not accumulated.
- It returns `cleanData` with the frames stripped, so downstream title tracking and text scraping never see status payloads (`src/main/runtime/ref-runtime-on-pty-data.ts:199-226`).

`[inference]` **No in-repo code emits OSC 9999.** Every reference is a consumer (main runtime `onPtyData`, renderer `pty-output-processor.ts`, `background-agent-status-consumer.ts`, `automation-session-observer.ts`, `parked-terminal-byte-watcher.ts`) plus tests. Combined with `docs/reference/agent-status-store.md:237` listing "the OSC 9999 wire format" among things that must not change, I read this as a **published in-band integration contract**: an agent CLI, a wrapper, or a user script writes the frame to its own stdout and the app picks it up with zero configuration. That is the escape hatch for a CLI with no hook system at all and no willingness to run curl.

### 3.8 PTY transcripts: evidence for text rules, not a status source

`[fact]` `docs/reference/agent-pty-transcript-capture.md`

Transcripts are **not** parsed for agent status. They exist to pin the *text* rules — readiness and blocked-prompt detection — to recorded evidence. The doc opens with exactly that framing: "readiness and blocked-prompt rules are text rules over what an agent CLI paints on a terminal. They are only as good as the screens they were written against" (`:3-5`).

The recorder `config/scripts/capture-agent-pty-transcript.mjs` allocates a real PTY, mirrors it to your terminal so you can drive the CLI by hand, and appends **every byte** to `src/main/runtime/__fixtures__/<name>.txt` with no escape stripping, `\r` folding, or rewrapping (`:10-20`). Details worth stealing:

| Feature | Why it exists | Cite |
|---|---|---|
| Ctrl-`]` ends the capture, consumed and never forwarded | The only way to stop recording *while a modal still owns the screen*; quitting the agent would dismiss the thing you came to record | `:21-24` |
| `--cols/--rows` pinned, recorded in a sidecar | Wrapping is part of the evidence | `:25-26` |
| `--send "<ms>:<text>"` scripted keystrokes | A dialog capture must be driven, and CI/agents have no TTY to type into | `:27-32` |
| `<name>.meta.json` sidecar | CLI version and account type behind a screen are not recoverable from the bytes | `:35-38` |
| `--scan` / `--redact` with **same-length** placeholders | A shorter replacement reflows the screen and destroys the evidence | `:60-83` |
| `pty-transcript-secret-scan.test.mjs` re-scans every committed fixture | A transcript that skips scrubbing fails the suite | `:95-96` |
| Tests feed raw bytes **through the runtime**, not into a matcher | "a rule tested on pre-normalised text is tested on something no pane ever sees" | `:98-106` |

There is a candid known-gap section: three older fixtures contain no escape bytes and no carriage returns because they went through a renderer and a clipboard, so "they are good enough for the wording-based rules built on them and are not evidence for anything else" (`:122-129`). New captures assert escape-byte presence so a pasted screen cannot pass as a capture. `[fact]`

### 3.9 The text-rule layer that does exist, and its narrow job

Three places use terminal text. All three are scoped.

**(a) Command Code output scrape — one CLI, because it has no hooks.**
`[fact]` `src/shared/command-code-output-status.ts:1-7` is explicit: "that CLI lacks hooks, so working/done agent-status rows are seeded from its rendered status words and idle composer."

This is the closest thing in the reference implementation to a pane-regex classifier, and reading it is instructive about the failure modes:
- The CLI **randomizes** its in-flight status word from a package-local list, so the detector hardcodes all 75 of them (`:26-102`), with the comment "checking only a few examples misses real active turns."
- Three regexes: active LLM status (glyph + word + ellipsis), active execution (`Executing:` / `Running(`), idle composer prompt (`:105-111`).
- Arming requires a version-banner match first (`:115-121`), gated behind a cheap character prefilter so non-Command-Code panes do not pay for two ~4 KB string builds per chunk (`:138-158`, `:251-255`).
- Scan window is bounded: 300 chars of carry plus a 4 KB budget, split head/tail for megabyte paste echoes (`:21-22`, `:167-187`).
- Match acceptance requires the match to **overlap new bytes**, not just appear in the window, tested both with and without a synthetic chunk boundary (`:189-220`).

`[inference]` That is roughly 300 lines of careful, version-fragile machinery to cover **one** CLI, versus ~50 lines per CLI on the hook path. It is the cost curve of text classification made explicit in one file.

**(b) OSC title heuristics — readiness waits, stats, and a last-resort dashboard fallback.**
`[fact]` `src/shared/agent-title-status.ts`, `src/shared/agent-title-core.ts`

`createAgentStatusTracker` (`agent-title-status.ts:68-124`) watches title changes and fires `onBecameIdle` / `onBecameWorking` / `onAgentExited`. `detectAgentStatusFromTitle` returns `working | idle | permission | null`, keyed off per-CLI glyphs (braille spinner frames, quarter-circle spinner, `✻`, `◇`, `π - `) and strong keyword regexes, with per-CLI precedence rules. `normalizeTerminalTitle` canonicalizes high-churn titles (a braille frame animated every 80 ms becomes one frame so consecutive titles dedupe; a rotating tool phrase collapses to one label) `[fact]` `:150-181`.

Two important scoping facts:
- Title evidence has its own observation origin, documented as the weakest the app acts on (`agent-status-observation.ts:17`). `[fact]`
- The flow is **also run backwards**: hook events *drive synthetic titles* so the renderer's title tracker stays in sync, because "native OSC titles miss some idle/permission frames" (`src/main/startup/main-window-agent-status.ts:112-120`). `[fact]` So the title layer is partly a *derived* view of hook truth, not an independent competing sensor.

**(c) Readiness and blocked-startup-modal rules for `terminal.wait`.**
`[fact]` `src/main/runtime/terminal-wait-detection.ts:1-80`

This answers "is the CLI ready to accept typed input?" — needed before the app injects a prompt into a pane. It combines an explicit-idle title check with a ready-prompt-preview scan and a blocked-signal scan, and resolves conflicts positionally: a blocked signal that appears *before* a live prompt in the tail is no longer actionable, because a live prompt proves the startup modal was dismissed (`:50-80`). `[fact]` This is where the Antigravity / cursor-agent transcripts get consumed (`docs/reference/agent-pty-transcript-capture.md:104-106`).

### 3.10 The one inference fallback, and how tightly it is fenced

`[fact]` `src/main/agent-hooks/server/server-status-inference.ts:20-114`

`inferInterrupt` synthesizes a final `done` when the user hits Escape or Ctrl-C and the agent never emitted its cancellation hook. The guard list is the interesting part, because it is a catalogue of everything that goes wrong if you infer status loosely:

| Guard | Why | Line |
|---|---|---|
| valid pane key, recognized intent | — | `:21-26` |
| not `providerSessionOnly` | identity-only rows carry placeholder status fields | `:33-35` |
| not `restoredUnconfirmed` | "inference must not fabricate a `done` onto a row whose `working` was never confirmed this runtime" | `:36-39` |
| Droid + Ctrl-C → refuse | there Ctrl-C exits the CLI, handled by PTY lifecycle, and does not interrupt the turn | `:42-45` |
| OpenCode/Copilot + single Escape → refuse | first Escape is a TUI cancel that can leave the turn running; only a double Escape counts | `:46-53` |
| Claude + Escape on an `AskUserQuestion` wait → reroute | that is "question dismissed", a different transition | `:54-61` |
| **exact baseline match** on state, agent type, prompt, `receivedAt`, `stateStartedAt`, and age ≤ 30 min | "a strict baseline match keeps a delayed timer from clobbering any newer hook" | `:62-72` |
| any non-idle subagent → refuse | Ctrl-C does not stop background children; inferring `done` would retire live child rows | `:73-76` |
| Claude with a running background task or session cron → refuse | Escape at the idle prompt does not stop provider-owned shells or crons | `:77-84` |
| sync the provider's lead-turn record before writing | otherwise a later child event re-emits the stale `working` and resurrects the cancelled pane | `:85-91` |

`[inference]` The design principle visible here: inference is allowed only to **close** a turn the app already watched open, only on an exact snapshot match, and never to open one.

### 3.11 Pane identity and authority — the part that actually makes it reliable

This is the half of the problem that pane-text classification does not even have a vocabulary for: *which* agent session a given event belongs to.

`[fact]` The row key is `paneKey = "${tabId}:${leafId}"` where `leafId` is a stable UUID layout leaf (`agent-status-types.ts:105-106`). Around it:

| Mechanism | What it stops | Cite |
|---|---|---|
| **Launch token** — an ephemeral identity stamped into the PTY env per launch, stored only as a SHA-256 hash | A stale process reclaiming a pane's row after the pane was reused | `listener-event.ts:9`, `server-status-disposition.ts:49-71` |
| **Retired-pane fences** + closed-pane/closed-tab sets, LRU-capped at 1024 each | Events from a pane the user closed | `server-status-disposition.ts:15-26`, `:131-160`, `server-constants.ts:26-30` |
| **`accept \| restart \| suppress`** disposition, computed before any write | A retired-but-reused pane staying permanently rowless; a stale process winning a re-fence race | `server-status-disposition.ts:28-121` |
| **`isNewTurnEvent` used as the revive gate**, not raw event-name literals | The bug the comment names: "only 5 of 18 sources name their boundary `UserPromptSubmit`/`SessionStart`; the rest stayed retired forever" | `server-status-disposition.ts:76-95` |
| **Asymmetric fail-open for unknown providers** | A stranded pane is "invisible and permanent with no user recovery, while a spurious revive decays after 30 min" | `server-status-disposition.ts:87-90` |
| **Pane-key aliases** | A pane detached into another tab; legacy numeric keys | `server-authority-aliases.ts`, `server-status-disposition.ts:123-129` |
| **Per-connection ordering watermark**, forcing `now` monotonic | An SSH reconnect clear being outranked by an in-flight older event | `server-status-update.ts:36-48` |
| **`connectionId` stamped only by `ingestRemote`** | A client asserting a transport identity it does not own | `listener-event.ts:13-15`, `agent-hook-relay.ts:78-79` |
| **`observation.incarnation` rebind on `restart`** | Later observations being ordered against a retired session's revisions | `server-lifecycle.ts:109-113`, `agent-status-observation.ts:47-48` |
| **`restoredUnconfirmed`** stamped on every hydrated non-`done` row | A 7-day-old persisted `working` reading as live truth | `agent-status-types.ts:155-159`, `server-hydration.ts` |
| **Agent-type identity resolution** with inherited-status suppression | A new agent in a reused pane inheriting the previous agent's row | `server-status-update.ts:112-139` |

`[fact]` Persistence is deliberately lossy in the right places (`server-persistence.ts:17-70`): invalid pane keys, structured-session rows, the observation stamp ("the sequencer that issued it dies with the process"), replay provenance, `restoredUnconfirmed`, live-only interaction keys, and the raw launch token (hash only) are all excluded. Version mismatch or corrupt JSON hydrates **empty** rather than partially (`server-hydration.ts:50-60`); hydrate age cap is 7 days (`server-constants.ts:23`).

### 3.12 Subagents / child rows

`[fact]` `agent-status-types.ts:67-82`, `providers/claude-events.ts:36-205`, `providers/claude-roster-state.ts`, `codex-subagent-roster`

Each row can carry up to 32 `AgentSubagentSnapshot` children (`agent-status-types.ts:150-151`, cap at `:190-192` bounding "per-pane cache and IPC fanout against a runaway spawner"). Child states add `idle` and `unverifiable` to the parent vocabulary.

The Claude normalizer is by far the most complex provider (315 lines) and almost all of that complexity is child bookkeeping `[fact]`:
- Lead events carry no `agent_id`; child events do. "even a child missed by lifecycle tracking cannot own the lead turn" (`claude-events.ts:198-205`).
- Child tool activity keeps the row live but must not overwrite the lead's tool/prompt caches, "a live card would vanish" (`:151`).
- A child's permission wait **displaces** the lead state, so the lead state is stashed and restored on clear — and a second child wait carries the *original* stash, not the intermediate `waiting` (`:219-232`).
- Parallel sibling tool completions during an open question are matched by `tool_use_id` so they do not clear the wrong wait (`:139-149`, `:176-185`).
- `background_tasks` inventory is trusted only where unambiguous, because teammates report "running" even while idle (`:207-218`); a `TeammateIdle` hook exists specifically to park those (`src/main/claude/hook-settings.ts:58-73`).
- `turnCompletedAt` is minted when the lead turn ended but background inventory keeps the pane `working`, so the later all-clear `done` can be paired to that turn (`:263-271`).

`[inference]` If you plan to support agent teams / subagents, this is where the model gets expensive, and it is not expressible in pane text at all.

### 3.13 Dashboard surfaces

**Renderer store.** `agentStatusByPaneKey` is the live map, with sibling maps for retained rows, sleeping sessions, launch configs, acknowledgements. Contract types in `src/renderer/src/store/slices/agent-status-contract.ts:19-80`; live reducer in `agent-status-live-reducer.ts`. Capacity is capped at 500 live rows with eviction (`docs/reference/renderer-agent-status-performance.md:316-318`, `agent-status-capacity-eviction.ts`). `[fact]`

**Sidebar rollups.** Two layers:
- `getLiveAgentStatusByWorktreeId` (`src/renderer/src/lib/worktree-activity-state.ts:32-70`) reduces fresh non-`done` rows to `working | monitoring | permission` per worktree, with `permission` winning over `working` winning over `monitoring`. `[fact]`
- `getWorktreeStatus` (`src/renderer/src/lib/worktree-status.ts:43-72`) takes that hook-derived value **first** and only falls back to per-pane title heuristics, then to "has a live PTY / browser tab" → `active`, then `inactive`. `[fact]`

**Smart attention ordering.** `[fact]` `src/renderer/src/components/sidebar/smart-attention.ts:20-30,126-200`. Five ordinal classes, lower = more demanding:

| Class | Meaning | Attention timestamp |
|---|---|---|
| 1 | Needs you (`blocked` / `waiting`) | `stateStartedAt` |
| 2 | Done, not interrupted, completed within 30 min | completion time |
| 3 | Working | `stateStartedAt` of the most recent prior done/blocked/waiting, else current |
| 4 | **Unverifiable** — stale non-`done` row on a pane the app still holds a live PTY for | when evidence was last observed, so the least-silent pane ranks first |
| 5 | Idle — no live row, interrupted `done`, aged-out completion, or stale row with no PTY | 0 |

Across panes, class is the **min** (most demanding pane wins) and the timestamp the **max** within that class (`:122-125`). Class 1 carries a `cause` (`blocked` / `waiting` / `title-heuristic`) used for telemetry on how often the weak fallback is what surfaced a worktree.

Class 4 is the honest-observer idea and it is the single most transferable piece of reader-side design here `[fact]` `src/renderer/src/lib/agent-row-decay-state.ts:12-56`: silence is not evidence, so a stale row's destination splits on the liveness the app actually holds. Live PTY → `unverifiable`, labelled "No update in 34m"; no PTY → `idle`. Neither ever claims the agent finished. The label deliberately reports *what the app last heard* rather than what the agent is doing, "the elapsed time is what lets a user apply knowledge ref does not have (a 40-minute build, a long download)" (`:46-50`).

**Dashboard buckets.** `[fact]` `src/shared/dashboard-snapshot.ts:13-48`, `src/renderer/src/components/dashboard/dashboard-row-bucket.ts`

Four columns in a fixed shared order: `attention`, `working`, `done`, `idle`. Card dot state is `working | blocked | waiting | done | idle` — kept **distinct** from the bucket "so attention cards retain their precise dot state." `dashboardCardDisplayState` adds `monitoring`, and makes completed cards stay green until acknowledged, then settle into gray idle (`:41-48`). `unverifiable` is projected down to `idle` on the published contract because the pop-out renderer validates against a fixed allowlist and may predate a new member — "publishing today's `idle` keeps those surfaces at today's behavior instead of silently losing the card" (`dashboard-row-bucket.ts:12-20`). `[fact]` That is a nice compatibility instinct: never add an enum member that an older reader will drop wholesale.

Bucket counts are memoized at three levels (active workspaces, per-worktree tally, last totals by identity) so the counters do not rescan on unrelated writes (`build-dashboard-bucket-counts.ts:26-48`). There is also a pop-out "agent map" view with spatial layout, label decluttering, and status glow (`src/renderer/src/components/dashboard-popout/agent-map-*.ts`). `[fact]`

**Dock, tray, OS notification, sound, push.**

```
 store row transition
        │
        ▼
 agent-completion-coordinator  (per pane, module-scoped, pruned on pane death)
   sources: hook | title | process-exit
   · replay guard 1000 ms
   · hook-done quiet window 1500 ms  (cancellable by resumed work)
   · hook outranks title
        │
        ├──▶ dispatchAttention   (waiting | blocked)
        └──▶ dispatchCompletion  (done, not interrupted, not sessionBoundary)
                  │
                  ▼
        notifications:dispatch  (main)
          · tray attention dot set BEFORE the cooldown/focus/enabled gates
          · settings gates: enabled, agentTaskComplete, terminalBell
          · Electron Notification + sound
          · mobile push: needs-input | finished
 separately: unread count selector → app.dock.setBadge (macOS only, "99+" cap)
```

`[fact]` coordinator `src/renderer/src/components/terminal-pane/agent-completion-coordinator.ts:21-45`, notification controller `agent-completion-notification-controller.ts:14-16,90-91,262-272`, IPC `src/main/ipc/notifications.ts:108-140`, tray `src/main/tray/tray-attention-icon.ts:1-8` (amber `#f59e0b` dot composited into the raw BGRA bitmap because Electron's NativeImage has no compositing API), dock `src/main/dock/unread-badge.ts:1-22` + `src/renderer/src/hooks/useUnreadDockBadge.ts:19-30`, push mapping `src/main/runtime/push/push-dispatcher.ts:41-52`.

Two details worth noting. First, the tray dot is lit *before* the gates so cooldown/focus/enabled cannot hold it back, and it clears on window show/restore (`src/main/startup/main-window-controller.ts:191-192`). Second, desktop focus does **not** suppress the phone: "desktop focus only means this computer sees the worktree; the paired phone may still need the alert" (`notifications.ts:133-134`). `[fact]`

The dock-badge hook has a performance note that generalizes: it uses a purpose-built selector rather than the raw maps, because "this hook is mounted on the App root, so subscribing to `tabsByWorktree` re-rendered the entire shell on every title frame" (`useUnreadDockBadge.ts:19-24`). `[fact]`

**Main → renderer → mobile propagation.** `[fact]` `src/main/startup/main-window-agent-status.ts:29-141`
- `agentStatus:set` to the main window and the dashboard pop-out window; `agentStatus:clear` likewise; `agentStatus:getSnapshot` for startup.
- `providerSessionOnly` rows are forwarded **without** titles, telemetry, or status UI (`:57-74`).
- Structured-session rows are currently filtered out of the renderer fan-out because the renderer's own feed bridge still writes them, "forwarding them too would give one pane key two writers" (`:52-56`, and the same rule at `docs/reference/agent-status-store.md:182-185`).
- Mobile: a hook-status change invalidates that pane's `session.tabs` snapshot so a paired client is pushed the new projection, because "hook rows are the only carrier of live agent state on a headless host" (`src/main/startup/main-process-observers.ts:66-74`). The mobile row builder prefers the hook/OSC payload over title-derived state (`src/main/runtime/runtime-mobile-agent-status-builder.ts:96`), and its last-resort projection is `done` with a comment explaining why it must be dated by its *evidence* and not by the byte stream (`:127-133`).
- Power management: `subscribeStatusChanges` feeds a power-save blocker keyed on the working-agent count (`main-process-observers.ts:58-61`).

### 3.14 Automations driven by status

`[fact]` `src/main/automations/`

Automations do not subscribe to status transitions directly. They ask the runtime a *question* — `waitForTerminal(handle, { condition: 'tui-idle' })` — and the runtime answers from "a sticky agent status, an idle pane title, or a ready shell prompt, across two different code shapes" (`runtime-terminal-run-observer.ts:60-66`). The comment explains the choice: "Asking it the question it already answers keeps every one of those in scope." `[fact]`

The observer's bounds are explicit and honest:

| Bound | Value | Why | Cite |
|---|---|---|---|
| agent-start poll interval | 250 ms | — | `runtime-terminal-run-observer.ts:10` |
| agent-start probe timeout | 250 ms | kept under the runtime's 2 s tui-idle fallback poll so a probe waiter cannot start a foreground-process poll of its own | `:11-13` |
| agent-start deadline | 2 min | past that, "silence is not evidence of work" | `:14-18` |
| total observation budget | 6 h | wall-clock and generous; exists so a pane whose agent is never detected stops re-arming for the process lifetime with its run stuck at `dispatched` | `:19-25` |

And when it loses the terminal it says so rather than claiming completion: `describeStrandedAutomationRun` returns "ref lost the terminal for this run before it reported completion." (`run-completion-watcher.ts:26-31`). `[fact]`

`[fact]` I found **no** auto-commit-on-done and **no** auto-run-next-on-done. The status-transition-driven automations are: notification/sound/push dispatch, tray+dock badging, power-save blocker, first-work branch/folder/workspace rename (`src/main/agent-hooks/first-work-*.ts`, hooked at `main-window-agent-status.ts:75-77`), synthetic title driving, stats/transition recording, `session.tabs` invalidation for mobile, PR/review refresh on completion (deferred microtask, `renderer-agent-status-performance.md:146-147`), and auto-hibernate of a completed agent (`agent-status-contract.ts:27-31` shutdown reason `auto-hibernate-completed-agent`).

### 3.15 Usage and cost tracking

`[fact]` Two independent data sources, neither of which is the hook stream.

| Source | Mechanism | Cite |
|---|---|---|
| **Provider transcript files on disk** | Recursive walk of `~/.claude/projects` and `~/.claude/transcripts` for `*.jsonl`, incremental by `(path, mtimeMs, size)`, parsed into turns, attributed to worktrees, aggregated into sessions + daily rollups. Priced from a local model-pricing table. Batched 4 files at a time with `setImmediate` yields — a comment records that `setTimeout(0)` is clamped to ~1 ms and cost ~2 s parked on timers for a 7.5k-transcript scan | `src/main/claude-usage/transcript-file-discovery.ts:1-40`, `scanner.ts:23-50`, `claude-model-pricing.ts`, `worktree-attribution.ts` |
| Same for the other CLI | Sessions discovered across managed + system + per-account home dirs, with realpath canonicalization and a legacy copied-session bridge; token deltas, cost estimate, rollup projections | `src/main/codex-usage/codex-session-file-discovery.ts:1-30`, `codex-usage-token-delta.ts`, `codex-usage-cost-estimate.ts`, `codex-model-pricing.ts` |
| **Live rate-limit windows** | A managed `statusLine` script piggybacks on the CLI's per-turn statusline invocation and POSTs the `rate_limits` blob to `/statusline/claude` on the same hook server. Gives live 5-hour / 7-day utilization + reset time "without spending the OAuth usage endpoint's tight budget (the endpoint 429s under polling)" | `src/shared/claude-statusline-rate-limits.ts:1-10,12-26`, `src/main/claude/statusline-script.ts`, handled at `server-lifecycle.ts:75-83` |

The statusline script is a masterclass in per-event cost control, because it runs ~3× per second while streaming `[fact]` `src/main/claude/statusline-script.ts:20-62`:
1. exit immediately if the process is a backgrounded daemon worker (its env names a pane it does not run in);
2. exit if no pane key (static PTY env, so it can gate *before* stdin is consumed);
3. buffer stdin to a per-pane temp file;
4. an all-builtin seconds-of-day throttle at 15 s, "avoids spawning findstr+curl on every streaming tick";
5. `findstr` for the `rate_limits` key — absent for non-subscriber sessions, so skip;
6. re-source the endpoint file for a fresh port/token;
7. only then spawn curl, and only then stamp the throttle, "so skipped ticks never push the next allowed post out".

There is a third provider usage dir (`src/main/opencode-usage/`) and a shared `src/main/usage/usage-provider-contract.ts` with a `UsageProviderId` union, so the pattern is pluggable. `[fact]`

### 3.16 Robustness details I'd steal verbatim

`[fact]` unless marked.

1. **Hook scripts must be harmless when the app is absent.** `wrapPosixHookCommand` (`src/main/agent-hooks/posix-hook-command.ts:8-36`) guards on a required env var *plus* file-exists/readable/executable, and otherwise drains stdin (or prints a configured fallback JSON). The comment explains the env guard specifically: without it "the agent spawns a shell for it on every event and the script only then discovers ref is not listening" — the spawn has already happened by then, which is the whole cost a standalone session was paying.
2. **Fail-closed CLIs need a real answer, not silence.** One CLI's `PreToolUse` reads silence as a hard deny, so the managed hook must still answer (`posix-hook-command.ts:11`, and `src/main/antigravity/hook-events.ts:28-30` chooses `{"decision":"ask"}` — the only documented value that defers to the user's own permission config, because `allow` "would auto-approve every tool call ref observes").
3. **Windows stdin is a hazard.** A dedicated stdin contract (`src/main/agent-hooks/hook-stdin-contract.ts`) with a named drain label, because outside a managed pane the caller can abandon stdin and the drain never returns. The env guards are ordered to outrank a provider skip for exactly that reason (`src/main/claude/hook-service.ts:73-77`).
4. **One CLI importing another's config.** The Claude hook installer can skip itself when a sibling CLI imports `.claude` hooks by default, "so status posts stay attributed to" the right agent (`hook-service.ts:80-84`).
5. **Do not register a hook that fires before its own outcome is known.** `PreCompact` is deliberately *not* registered: it fires before the compact is validated and an aborted compact emits it alone, "so mapping it to 'working' would strand the pane exactly as this registration is meant to prevent" (`src/main/claude/hook-settings.ts:87-95`).
6. **Truncated-body telemetry.** An authenticated POST whose body dies short of its own `Content-Length` is counted as transport interference — "this is the one failure mode that silently stops status for every runtime at once" (`server-lifecycle.ts:121-127`).
7. **Field caps everywhere.** tool name 60, tool input 160, assistant message 8000, interactive prompt 16000 (not truncated like tool input, because it holds full question JSON and truncating "would corrupt it and drop options"), subagents 32, JSON structural tokens 4096 / nesting depth 16 (`agent-status-types.ts:236-256`).
8. **Explicit re-normalization at the trust boundary.** The relay normalizes and the host normalizes **again** on receive, "so relay skew or a buggy remote process cannot poison main-process state" (`agent-hook-relay.ts:11-20`, `server-ingest-remote.ts:22`).
9. **Oversized relay frames shed optional fields and record what they shed**, with a digest for subagent rosters, so the receiver can distinguish "shed in transit" from "the agent cleared it" (`agent-hook-relay.ts:122-138`).
10. **Duplicate-delivery vs same-text rerun.** `promptInteractionKey` is a per-turn key minted where the provider exposes enough context, used only for in-memory dedupe and never persisted or sent in telemetry (`listener-event.ts:20-21`, `agent-hook-relay.ts:83-85`).
11. **Assistant-message retry.** A `done` that arrives without its assistant text is retried 5× at 50 ms (`server-constants.ts:5-6`, `server-lifecycle.ts:116`), and `lastCompletedAssistantMessage` is kept across the next `working` because batched publication can fold a whole done→working turn into one notification (`agent-status-types.ts:124-127`).
12. **Stranded-claim reaping.** A PTY that dies while the app is down never runs teardown, so hydrate can rebuild a subagent roster nothing can ever retire — which would gate the pane `working` for the rest of its life and block hibernation. A second reap path checks real local PTY liveness, but only after both execution-host and relay binding prove local ownership, and skips panes that reported in this runtime (`server-reaping.ts:11-60`).

---

## 4. Implementation patterns

| Pattern | Description | Location |
|---|---|---|
| Push-not-poll status | Agent CLI posts normalized events to loopback HTTP; app never scrapes | `src/main/agent-hooks/server/server-lifecycle.ts:53-131` |
| Exhaustive per-provider switch | New CLI = compile error until every question is answered | `src/shared/agent-hook-listener/provider-dispatch.ts:51-151`, `provider-event-routing.ts:38-82,131-174` |
| Provider normalizer returns `null` | "this event says nothing" is a first-class answer | every `providers/*-events.ts` |
| Single writer | `applyNormalizedStatus` is the only mutation path; every ingress funnels through it | `server-status-update.ts:23-210` |
| Provenance at write time | `origin`/`authorityId`/`incarnation`/`revision`/`boundary`/`kind` stamped once; readers never re-adjudicate | `src/shared/agent-status-observation.ts:12-57`, `server-status-application.ts:151-174` |
| Execution host owns status | The host that runs the process is the only party that can observe it; clients mirror, never write back | `docs/reference/agent-status-store.md:43-60` |
| Three clocks | delivery / evidence / state-start, plus a replica receipt clock | `server-status-application.ts:19-80`, `agent-status-freshness.ts` |
| Hydration honesty | Restored non-`done` rows are `restoredUnconfirmed` and never fresh | `agent-status-types.ts:155-159`, `agent-status-freshness.ts:46-53` |
| Silence-is-not-evidence decay | Stale rows split into `unverifiable` (live PTY) vs `idle` (no PTY); neither claims completion | `src/renderer/src/lib/agent-row-decay-state.ts:12-31` |
| Session-boundary flag | Idle-landing `done` excluded from completion-reactive consumers | `agent-status-types.ts:180-187` |
| Fenced inference | Inference may only close a turn it watched open, on an exact snapshot match | `server-status-inference.ts:62-91` |
| Offline spool + pre-bind drain | No lost events across app restarts, fenced by launch-token hash | `agent-hook-spool.ts`, `server-lifecycle.ts:44-52` |
| Endpoint file | Long-lived PTYs re-discover a new port/token without restarting the agent | `endpoint-publication.ts:27-86` |
| Burst-fold transaction | 33 ms window → one ordered fold → one store publication | `renderer-agent-status-performance.md:110-131` |
| Additive wire evolution | New optional fields only; old clients ignore them; enum members never added where a validator would drop the whole payload | `agent-status-store.md:175-180`, `dashboard-row-bucket.ts:12-20` |
| Transcript fixtures as evidence | Byte-exact PTY captures, same-length redaction, fed through the real runtime in tests | `docs/reference/agent-pty-transcript-capture.md` |

---

## 5. Per-agent-CLI support matrix

`[fact]` Install targets from `src/main/agent-hooks/managed-agent-hook-registry.ts:39-113`; config paths from each `src/main/<agent>/hook-*.ts`; event maps from `src/shared/agent-hook-listener/providers/*-events.ts` and `src/shared/agent-hook-listener/provider-event-routing.ts:38-82`.

| CLI (wire source) | Install mechanism | Config artifact | Turn-start event | Working events | Needs-input events | Done events | Notes |
|---|---|---|---|---|---|---|---|
| `claude` | managed script + settings edit (+ statusline script) | `~/.claude/settings.json`; script under a managed hooks dir | `SessionStart`, `UserPromptSubmit` | `UserPromptSubmit`, `Pre/PostToolUse`, `PostToolUseFailure` | `PermissionRequest`, `PreToolUse`(AskUserQuestion) → `waiting` | `Stop`, `StopFailure`, `PostCompact`(manual) | Richest: 13 registered events incl. `SubagentStart/Stop`, `TeammateIdle`. `SessionStart` lands a session-boundary `done`, not `working`. `PreCompact` deliberately unregistered |
| `openclaude` | same code path, different config dir | `~/.openclaude/settings.json` | as claude | as claude | as claude | as claude | Claude-compatible; no Windows compat launcher |
| `kimi` | managed script + TOML block | `~/.kimi-code/config.toml`, `[[hooks]]` inside a marked managed block | `UserPromptSubmit` | `UserPromptSubmit`, `Post/PreToolUse`, `PostToolUseFailure` | `PermissionRequest`, `PreToolUse`(ask tool) | `Stop`, `StopFailure` | Claude-compatible payloads, own attribution/icon |
| `devin` | managed script + JSONC edit | `~/.config/devin/config.json` (or `%APPDATA%`) | `UserPromptSubmit` | `UserPromptSubmit`, `Pre/PostToolUse`, `PostCompaction` | `PermissionRequest` | `Stop`, `SessionEnd` | JSONC-preserving writer (edits text in place so comments/key order survive). `SessionStart` **dropped** — it fires on idle TUI open |
| `codex` | managed script + `config.toml` + app-server trust grant | `<codex home>/config.toml`, `hooks.json` | `SessionStart`, `UserPromptSubmit` | `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `PermissionRequest`, `PreToolUse`(request_user_input) | `Stop` | Only async installer: awaits a trust-grant session; precomputes a per-hook config hash to avoid manual approval |
| `gemini` | managed script + settings edit | `~/.gemini/settings.json` | `BeforeAgent` | `BeforeAgent`, `Before/AfterTool`, `Pre/PostToolUse` | — (no needs-input event) | `AfterAgent` | Thinnest normalizer (51 lines); no permission signal |
| `antigravity` | managed script + per-event Windows wrappers + hooks bundle | `~/.gemini/config/hooks.json` | `PreInvocation` | `Pre/PostInvocation`, `Pre/PostToolUse` | `PreToolUse`(ask_question / ask_permission) | `Stop` when fully idle | `Stop` can arrive with `fullyIdle:false` → stays `working`. Post-`Stop` bookkeeping `PostToolUse` suppressed by transcript path. Prompt read from transcript when hook omits it. Must answer `{"decision":"ask"}` or silence = deny |
| `cursor` | managed script + hooks.json | `~/.cursor/hooks.json` | `beforeSubmitPrompt`, `sessionStart` | `preToolUse`, `postToolUse`, `postToolUseFailure`, `beforeShellExecution`, `beforeMCPExecution` | — (gates treated as `working` to avoid notification spam) | `stop`, `sessionEnd` | `afterAgentResponse` after a `done` enriches the row instead of resurrecting it. `stop` with non-`completed` status ⇒ `interrupted`. Each event has a required response JSON |
| `droid` | managed script + settings edit | `~/.factory/settings.json` | `UserPromptSubmit` | `UserPromptSubmit`, `Pre/PostToolUse` | `PermissionRequest`, `PreToolUse`(ask/high-risk), `Notification`(permission text) | `Stop`, `Notification`(idle text) | `SessionStart` dropped. Emits no `Stop` on user interrupt — only an idle notification. Ctrl-C exits the CLI, so interrupt inference refuses |
| `copilot` | managed script (+PowerShell path) | `~/.copilot` (env-overridable) | `SessionStart`, `UserPromptSubmit` | `SessionStart`, `UserPromptSubmit`, `Post/PreToolUse`, `PostToolUseFailure`, `PermissionRequest` | `Notification`(permission_prompt / elicitation_dialog), ask-user tool → `blocked` | `Stop`, `SessionEnd`, `ErrorOccurred` when not recoverable | `PermissionRequest` fires before allow/ask/deny so it stays `working`. `ErrorOccurred` + `recoverable` → `working`. Single Escape does not infer interrupt |
| `amp` | provider-native plugin file | `~/.config/amp/plugins/<managed>.ts` | `agent.start` | `agent.start`, `tool.call`, `tool.result` | — | `agent.end` | Multiplexes many threads through one pane: caches by thread id internally while keeping the pane key stable; drops stale tool events arriving after the thread ended. `agent.end` + `cancelled` ⇒ `interrupted` |
| `hermes` | provider-native plugin dir (`plugin.yaml` + source) | `~/.hermes/plugins/<name>/` | `pre_llm_call`, `on_session_start` | `on_session_start`, `pre_llm_call`, `pre/post_tool_call`, `post_approval_response` | `pre_approval_request` | `post_llm_call`, `on_session_end`, `on_session_finalize`, `on_session_reset` | Own install lifecycle, deliberately absent from the shared script refresher |
| `grok` | managed script + hooks config file (symlink-aware) | agent home hooks config | `user_prompt_submit` | `user_prompt_submit`, `pre/post_tool_use`, `post_tool_use_failure` | `pre_tool_use`(ask_user_question), `Notification`(permission) | `stop`, `session_end`, `stop_failure`, `Notification`(idle text) | Event names normalized snake_case. Routine permission-prompt notifications return `null`. Idle detected from notification wording. Rotating working title collapsed to one label. Has an async remover and symlink-cleanup marker |
| `command-code` | managed script + settings edit **and** a PTY text scrape | `~/.commandcode/settings.json` | none (`isNewTurnEvent` = false) | `Pre/PostToolUse` | — | `Stop` | **The hookless case.** No prompt-start hook and no reliable `Stop` for no-tool turns, so working/done are seeded from TUI status words + idle composer. Prompt read from its transcript. Has a renderer-side done-settle window |
| `opencode` | managed JS plugin written into a per-session config overlay injected via env | overlay dir + `ref-opencode-status.js` | `SessionStart` | `SessionBusy`, `MessagePart` | `PermissionRequest`, `AskUserQuestion` | `SessionIdle`, `SessionStart`(as boundary `done`) | Mid-session boundary rides an explicit-prompt `MessagePart`, which the generic classifier cannot name, so the revive gate special-cases it. First Escape does not infer interrupt |
| `mimo-code` | own hook service + PTY env | (own home dir) | **none** | `SessionBusy`, `MessagePart` | `PermissionRequest`, `AskUserQuestion` | `SessionIdle` | No `SessionStart` at all; without the `MessagePart` special case its retired panes would never come back |
| `pi` / `omp` / `prime-agent` | generated agent-status handler + titlebar/prefill extensions injected into the agent's config dir | `.pi` / `.prime` etc. | `before_agent_start` | `agent_start`, `tool_call`, `tool_execution_start/end`, `message_end`, approval resolution | ask tool / `tool_approval_requested` → `blocked`; `ui_prompt_start` → `waiting` | `agent_end`, `ui_prompt_end` when `is_idle` | `session_start` clears per-turn cache and returns `null` unless a UI prompt is active. Emits a milestone `done` while still working, so the notification path routes it through the quiet window. Has its own explicit state marker in the OSC title that outranks glyph sniffing |
| — (any, no hook support) | **OSC 9999** frame written to stdout | none | n/a | canonical payload with `state` | canonical payload | canonical payload | Zero-config in-band fallback. No provider normalizer. No `providerSession` field, so an OSC event is never evidence a session ended |

Coverage counts `[fact]`: 18 wire sources; 14 entries in the managed installer registry, 12 in the script refresher (Amp and Hermes write provider-native plugins with their own lifecycles and are deliberately absent, enforced by a coverage test), 14 removers, 14 status readers, 1 async remover.

Installation hardening around this matrix `[fact]`: a per-install lock (`managed-hook-install-lock.ts`), owner-identity records so the app never clobbers a hook it does not own (`managed-hook-owner-identity.ts`, `managed-hook-lock-claims.ts`), detection commands to find pre-existing hooks (`managed-hook-detection-commands.ts`), a guard against writing scripts outside the managed dir (`hook-script-outside-ref.test.ts`), local-CLI presence probing with PATH hydration so install is not offered for a CLI that is not there (`local-agent-cli-presence.ts`), install telemetry (`install-telemetry.ts`), and SFTP-based remote installers for every service (`remote-managed-hook-installers.ts`, `installer-utils-remote.ts`).

---

## 6. Adopt / Adapt / Avoid

### Adopt

1. **Push over classify, with the provider switch made exhaustive.** The entire reliability story is that the agent tells you. `provider-dispatch.ts` + `provider-event-routing.ts` is the shape: three per-provider questions, one normalized payload, compile error on a new CLI. Two files, and it scales to 18 CLIs at ~50-100 lines each.
2. **`null` as a provider answer.** "This event says nothing about status" prevents the entire class of phantom rows. Most of the per-provider comments in this repo are about which events must return `null`.
3. **`sessionBoundary` on idle-landing `done`.** Without it, resume/clear/connect fires fake completion notifications, unread badges, and automation completions. Cheap flag, large blast radius.
4. **Three clocks: delivery, evidence, state-start.** Then one freshness function. Collapsing these is the bug that makes a reconnect renew a stale row's lease forever.
5. **`unverifiable` vs `idle` decay, and a "No update in 34m" label.** Never claim the agent finished from silence. Split on the liveness you actually hold, and report what you last heard so the user can apply knowledge you do not have.
6. **Launch token + retired-pane fence + `accept|restart|suppress`.** Pane reuse is the hardest identity problem in this space and is invisible in a text-classification design. The asymmetry argument for failing open on unknown providers is correct and worth quoting in your own code.
7. **Endpoint file + offline spool.** Together they make status survive an app restart in both directions: a live agent re-discovers the new port, and events emitted while you were down are drained before the listener binds.
8. **Hook scripts that are harmless when the app is absent**, with an env guard *before* any spawn, and a valid fail-closed response for CLIs that read silence as deny.
9. **Burst-fold into one store transaction**, plus purpose-built selectors for root-mounted consumers. The measured numbers in the perf doc justify both halves independently.
10. **Byte-exact PTY transcript fixtures with same-length redaction**, replayed through the real runtime. If you keep *any* text rules, this is how you stop them rotting.
11. **Additive-only wire evolution**, and never add an enum member a validating older reader would drop wholesale. Project the new value onto an old one instead.

### Adapt

1. **One store per execution host, clients mirror.** The rule is right; the specific "hook server is the store" implementation is an accident of history that this repo is mid-refactor on (`agent-status-store.md:1-16,84-235`). Build your daemon's status store as a store from day one rather than growing it inside an HTTP handler.
2. **OSC 9999 as the zero-config fallback.** Keep the idea and the resumable-parser mechanics; pick your own unused OSC number and publish the payload schema. Note that no in-repo code emits it, so treat it as an integration contract for third parties, not an internal channel.
3. **Subagent rosters.** Adopt only if you actually show child rows. The Claude normalizer's child bookkeeping is the single largest source of complexity in the provider layer.
4. **Statusline piggyback for live rate limits.** Clever and cheap, but the multi-stage cost gating exists because the hook fires ~3×/sec. Copy the gating order (cheap env gate → throttle → content prefilter → only then spawn) more than the specific script.
5. **Five-class smart attention ordering.** The class ladder is good. Whether you need per-class attention timestamps depends on whether you sort a large fleet.

### Avoid

1. **Pane-text classification as the primary signal.** The one place this repo does it — for a single CLI with no hooks — costs ~300 lines, hardcodes 75 randomized status words, and is pinned to one CLI version. That is the honest price of the approach, for one agent.
2. **Title heuristics as an authority.** Note that this repo runs the flow *backwards*: hook events drive synthetic titles to keep the title tracker correct. If you find yourself reconciling title-derived state against hook-derived state as peers, you have two sensors and no tiebreak.
3. **Unbounded inference.** Do not let a heuristic open a turn or fabricate a transition onto an unconfirmed row. Every guard in `server-status-inference.ts` is a bug someone shipped.
4. **One timestamp.** `updatedAt` alone cannot answer "did the pane report again?" (equal timestamps are accepted) nor "how old is this evidence?" (replays restamp it).
5. **Persisting the observation stamp or the raw launch token.** The sequencer dies with the process; the token is hashed for a reason.
6. **A per-event zustand write.** 2,000 events × 9,279 listeners is the measured 36% mean renderer CPU.
7. **Growing a second status store.** `agent-status-store.md:24-41` documents three copies of the same row inside one process, keyed differently, each with its own precedence and freshness rules, such that "the same pane can legitimately read differently on the desktop, on the phone, and in the CLI." That is the failure mode to design against from the start.

---

## 7. Decision inputs for us

### 7.1 The signal hierarchy to adopt

```
 ┌──────────────────────────────────────────────────────────────────────────┐
 │ TIER 0  hook / plugin push  (authenticated loopback HTTP, per-provider   │
 │         normalizer, provenance = 'hook')                                 │
 │         → the ONLY tier allowed to OPEN a turn and to declare needs-input│
 │         → survives app restart via endpoint file + offline spool         │
 ├──────────────────────────────────────────────────────────────────────────┤
 │ TIER 1  structured session feed  (we own the provider child process;     │
 │         its journal IS the truth; provenance = 'structured')             │
 │         → never persisted; host republishes on restore                   │
 ├──────────────────────────────────────────────────────────────────────────┤
 │ TIER 2  in-band OSC frame  (canonical JSON payload in PTY bytes,         │
 │         provenance = 'osc')                                              │
 │         → repaints the CURRENT state; kind = 'snapshot', never a         │
 │           transition, never proof a session ended                        │
 ├──────────────────────────────────────────────────────────────────────────┤
 │ TIER 3  process evidence  (PTY exit, foreground-process identity;        │
 │         provenance = 'process')                                          │
 │         → may CLOSE a turn; may never open one                           │
 ├──────────────────────────────────────────────────────────────────────────┤
 │ TIER 4  transcript parse  (provider-owned session files on disk)         │
 │         → we should use this for USAGE/COST and prompt recovery only,    │
 │           not for live status                                            │
 ├──────────────────────────────────────────────────────────────────────────┤
 │ TIER 5  pane text / OSC title  (provenance = 'title')                    │
 │         → readiness gate before typing, blocked-startup-modal reasons,   │
 │           and a LAST-RESORT dashboard row for a CLI with nothing else    │
 │         → must be tagged so the UI can say "heuristic"                   │
 ├──────────────────────────────────────────────────────────────────────────┤
 │ NOT A TIER  silence  → 'unverifiable' (we hold the PTY) or 'idle' (we    │
 │             do not). Never 'done'.                                       │
 └──────────────────────────────────────────────────────────────────────────┘
```

Recommendation: **hooks > structured > OSC > process > transcript > pane text**, with the two hard rules from the reference implementation: only Tier 0/1 may open a turn or assert needs-input, and silence never becomes `done`. `[inference]` from the origin enum ordering and comments in `src/shared/agent-status-observation.ts:12-27` plus the write-time precedence rule in `docs/reference/agent-status-store.md:52-57`.

Our current approach (regex over tmux pane text + optional Claude hooks) inverts this: it makes Tier 5 the default and Tier 0 the option. The reference implementation's Command Code file is the concrete cost estimate for staying there.

### 7.2 Which tiers are tmux-independent

| Tier | tmux-independent? | Why |
|---|---|---|
| 0 hook push | **Fully.** Needs only (a) env vars in the agent's process and (b) a reachable loopback port or a spool directory | `[fact]` `server-lifecycle.ts:53-131`, `spawn-env.ts`, `agent-hook-spool.ts`. Nothing in the ingest path references a terminal at all |
| 1 structured feed | **Fully.** No PTY exists for these rows | `[fact]` `agent-status-store.md:89-113` — "A structured session has no PTY, so no hook or OSC event carries its pane key" |
| 2 OSC frame | Needs a byte stream we read, not a tmux pane. Works over a PTY we own, a daemon-owned PTY, or a relayed stream | `[fact]` parsed in three different places: main `onPtyData`, renderer transport for remote runtimes, and a parked-pane byte watcher |
| 3 process evidence | Needs a process we can enumerate; independent of the multiplexer | `[fact]` `agent-completion-coordinator.ts:21` source `'process-exit'` |
| 4 transcript parse | Fully independent — files on disk | `[fact]` `claude-usage/transcript-file-discovery.ts` |
| 5 pane text / title | **tmux-coupled by construction.** Requires a rendered screen, a known pane size, and stable wrapping | `[fact]` `agent-pty-transcript-capture.md:25-26` treats wrapping as part of the evidence; `:48-58` requires capture on the execution host, inside the distro, at a pinned size |

So the migration away from tmux coupling is exactly the migration up this hierarchy. Tiers 0, 1, 3 and 4 have no opinion about tmux. Tier 2 needs a byte stream. Only Tier 5 needs a *pane*.

The one thing we genuinely still need pane text for, per the reference implementation, is the **readiness gate**: knowing when a freshly launched TUI is ready to accept typed input, before any hook has fired. `[fact]` `terminal-wait-detection.ts:36-80`, `runtime-terminal-run-observer.ts:60-66`. Budget for keeping that, fixture-pinned, and nothing else.

### 7.3 What the daemon must own so mobile / web / TUI / desktop see one truth

Straight from the reference implementation's own audit and rule `[fact]` `docs/reference/agent-status-store.md:43-83`:

> "The execution host owns agent status, in one store, and every reader subscribes to it." One store per execution host. A remote host keeps its own store and the client mirrors it down. **Mirroring is not merging: a client never writes its observations back to a host.** Precedence is decided once, at write time, with provenance recorded on the row. Readers never re-adjudicate.

Concretely, the daemon must own:

| # | Responsibility | Why it cannot live in a client | Cite |
|---|---|---|---|
| 1 | **The status map**, keyed by pane/session id, with the single-writer apply path | Three copies with different keys and different precedence = the same pane reading differently on desktop, phone, and CLI | `agent-status-store.md:17-41` |
| 2 | **The listener endpoint** (port + per-start token) and its publication (env + endpoint file) | The token must be minted by whoever validates it | `server-lifecycle.ts:36-52`, `endpoint-publication.ts` |
| 3 | **Pane authority**: launch tokens and hashed commitments, retired-pane fences, pane-key aliases, per-connection ordering watermarks, and the evidence-age map that must outlive a transport clear | Enumerated in the doc as the reason nothing else in the process can be the store | `agent-status-store.md:66-77` |
| 4 | **Durable persistence** with a bounded hydrate window and the `restoredUnconfirmed` stamp | A client cannot know whether a transition was missed while nobody was listening | `server-persistence.ts`, `server-hydration.ts`, `server-constants.ts:23` |
| 5 | **Write-time precedence and provenance stamping** | Readers must never re-adjudicate hook vs OSC vs structured | `server-status-update.ts:23-210`, `agent-status-observation.ts` |
| 6 | **Fan-out**: a push channel (`agentStatus:set` / `:clear`) *and* a pull snapshot (`getSnapshot`) | A client that attaches late needs the snapshot; one that is attached needs the deltas | `main-window-agent-status.ts:107-129`, `server-lifecycle.ts` |
| 7 | **Interrupt / question-answered inference**, because it needs the store's own baseline to fence against | `server-status-inference.ts:62-72` |
| 8 | **Liveness reconciliation / reaping** against real process liveness | Only the execution host can enumerate its processes | `server-reaping.ts:11-60` |
| 9 | **Offline spool drain**, before the listener binds | Ordering guarantee | `server-lifecycle.ts:44-52` |
| 10 | **The shared rollup + one decay clock** | The reference implementation currently derives its worktree card status in **three** places, with mobile hand-copying the 30-minute constant, and has an open PR to fix exactly that | `agent-status-store.md:227-234` |

Readers keep **only** presentation policy and user facts: the display decay window, acknowledgements, dismissals, unread counts `[fact]` `agent-status-store.md:58-60`. Put those behind one shared implementation too, or mobile will drift.

`[inference]` For us, the practical shape: the daemon exposes one subscribe-plus-snapshot status API; desktop, web, TUI and mobile are all mirrors; a mirrored row carries the replica's own receipt clock for decay so host clock skew does not leak into the UI (the `mirroredEvidenceReceivedAt` trick, `agent-status-freshness.ts:19-28`).

### 7.4 Per-CLI support matrix, condensed for our planning

`[fact]` Install-mechanism classes observed, in ascending integration cost:

| Class | CLIs | What we must build |
|---|---|---|
| **A. JSON/JSONC settings edit + managed script** | claude, openclaude, devin, droid, command-code, cursor, gemini, copilot | One managed script generator (POSIX + Windows cmd/PowerShell) plus a settings merger that preserves the user's file. JSONC needs an in-place text editor, not parse/stringify |
| **B. TOML settings edit** | kimi, codex | Marked managed block insertion/removal inside TOML |
| **C. Provider-native plugin file** | amp, hermes, opencode, mimo-code, pi family | Generate plugin source in the provider's language; it reads the endpoint file itself. Own install lifecycle, not the shared script refresher |
| **D. Config + trust grant** | codex | Async install; precompute a hook hash so the user is not dropped into a manual approve flow |
| **E. Hooks bundle + per-event wrappers** | antigravity | Per-event Windows wrapper files; must return a valid decision or silence reads as deny |
| **F. No hooks at all** | command-code (in addition to A) | Text scrape. ~300 lines, version-fragile, one CLI |
| **G. Zero-config in-band** | any | Publish the OSC frame schema and let the CLI or a wrapper emit it |

Event-shape families, which is what actually determines normalizer cost:

| Family | Members | Normalizer size |
|---|---|---|
| Claude-compatible (`UserPromptSubmit` / `Pre`-`PostToolUse` / `Stop` / `PermissionRequest`) | claude, openclaude, kimi, devin, codex, droid, copilot (after name normalization) | 50-180 lines; claude itself is 315 because of subagents |
| Lifecycle-pair (`*.start` / `*.end` + tool events) | amp, gemini, antigravity, hermes, pi family | 51-169 lines |
| Session-state (`SessionBusy` / `SessionIdle` / `MessagePart`) | opencode, mimo-code | 57 lines |
| camelCase gate-heavy | cursor | 67 lines |
| snake_case + notification-text sniffing | grok, droid (partially) | 104 lines + a tool-fields module |
| None | command-code | scrape instead |

`[inference]` Recommended order for us, by value per line: (1) the Claude-compatible family, because one normalizer covers six CLIs; (2) the OSC in-band contract, because it is one parser and covers everything else at zero per-CLI cost; (3) the session-state family; (4) per-CLI plugins only for CLIs we actually see in use. Defer text scraping indefinitely.

### 7.5 Two traps in our current design that this repo has already hit

`[fact]` Both are documented failures, not speculation.

1. **Matching raw event-name literals instead of a per-provider classifier.** The retired-pane revive gate originally matched two literals, and "only 5 of 18 sources name their boundary `UserPromptSubmit`/`SessionStart`; the rest stayed retired forever" (`server-status-disposition.ts:76-79`). Any per-provider question answered with a shared literal list will silently strand the providers that name things differently. Our regex-per-CLI approach is one big shared literal list.
2. **Letting more than one writer own a row key.** Three copies inside one process, keyed differently, each with its own precedence and freshness rules (`agent-status-store.md:24-41`). The refactor to collapse them is a four-PR sequence that is still only one PR in.

---

## 8. Analysis summary

- **Confidence: High** on the hook transport, the provider layer, the status vocabulary and state derivation, the store's single-writer/authority/persistence model, the OSC 9999 parser, the freshness and decay rules, the renderer batching, the dashboard buckets and attention classes, the notification/dock/tray/push path, and the usage sources. All read directly.
- **Confidence: Medium** on the completion coordinator's full precedence between its three sources (I read the coordinator head, the hook observer's option surface, and the quiet-window constants, not every branch of the title observer and process monitor), and on the exact per-CLI install file layout for the plugin-class agents (I read paths and install entry points, not every writer).
- **Confidence: Low / explicitly unresolved:** who emits OSC 9999. No producer exists in this repo; I inferred a published third-party contract from the consumer set plus the "must not change" list in the store doc.
- **Query coverage: ~95%.** All six requested areas are covered with citations. Two requested paths do not exist under the given names: there is no `src/main/notifications/native/` or `notification-status-macos` module (the macOS pieces are `src/main/notifications/desktop-away-state.ts`, `src/main/dock/unread-badge.ts`, `src/main/tray/*`), and there is no `src/main/claude/__fixtures__` transcript set for status parsing (the PTY transcript fixtures live in `src/main/runtime/__fixtures__/` and `src/shared/__fixtures__/`).

### Open questions a maintainer of that repo would have to answer

1. Who writes OSC 9999 frames? Is the schema published to CLI vendors, or is it only for user wrappers?
2. Is there any agent CLI in the matrix whose *only* signal is the title heuristic, or does every supported CLI have at least a hook, a plugin, or the OSC frame?
3. PR 1b, 2 and 3 of the store consolidation are unimplemented (`agent-status-store.md:187-234`). Until PR 3, three separate worktree-status rollups exist and mobile hand-copies the decay constant. Which is authoritative today?
4. The Command Code done-settle window is renderer-only policy with no main-process equivalent (`agent-status-store.md:222-225`). Does a headless host therefore mis-report that CLI's completions?
5. `dashboardCardDotState` flattens `unverifiable` to `idle` for the pop-out. Does the pop-out user lose the distinction between "quiet but alive" and "nothing there"?
6. Copilot maps a non-recoverable `ErrorOccurred` to `done`. Is an errored turn distinguishable from a completed one anywhere downstream, or does it fire a "finished" notification?
