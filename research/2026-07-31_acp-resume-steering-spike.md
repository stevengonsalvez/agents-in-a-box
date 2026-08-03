# Spike: ACP session resume and steering, per-adapter probe results

**Date**: 2026-07-31
**Branch**: f/buzz
**Answers open questions 1 and 2 of** `research/2026-07-31_14-56-19_buzz-acp-port.md:229-230`
**Status**: 2 of 3 adapters verified end to end on live wire; gemini-cli blocked at `session/new` by missing credentials (source-read only)

## Method

Each adapter was installed locally and driven by a purpose-built Node client speaking newline-delimited JSON-RPC 2.0 over piped stdio, with every byte in both directions timestamped into a `wire.log`; the client answered any agent-to-client request with `-32601` so the agent could never hang.
Scenarios per adapter: `initialize` capability read, `session/new`, real `session/prompt` turns, SIGKILL of the adapter process followed by a fresh process doing `session/load` on the same sessionId, `_session/steering` in-flight and idle, `session/list`, and a deliberately bogus method. Empirical results were cross-checked against the shipped (unminified) adapter source.

## Support matrix

| Adapter | Version | `loadSession` cap | `session/load` behaviour | `session/list` | Steering flags | Unknown-method behaviour | Resume verified e2e | Auth situation |
|---|---|---|---|---|---|---|---|---|
| `@agentclientprotocol/claude-agent-acp` | 0.64.0 | `true` | Works with a real sessionId, in-process and after a hard process kill. Replays history as `session/update` notifications (2 observed) before returning. Reply echoes the same sessionId. Malformed non-UUID id leaks the CLI error verbatim as `-32603` | Yes, advertised and working. Returns `{sessions:[{sessionId,cwd,title,updatedAt}]}`, filtered by `cwd`, empty for a session with no messages | Present: top-level `_meta.steering.supported = true` on the `initialize` result (NOT inside `agentCapabilities`). Also `agentCapabilities._meta.claudeCode.promptQueueing = true` | Proper `-32601`, offending method echoed in `data.method` | Yes. Codeword survived SIGKILL plus fresh-process load | Not blocked. `authMethods: []`, ambient `~/.claude/.credentials.json` picked up implicitly, real turns completed with `stopReason: end_turn` |
| `@agentclientprotocol/codex-acp` | 1.1.7 | `true` | Works only once a rollout is persisted (at least one turn). Three distinct failures, all `-32603` with free-text `data.details`: `no rollout found for thread id X` (fresh session AND fabricated id, indistinguishable except by the echoed id), and `session X is archived` after `session/delete`. Success result does NOT echo sessionId. Replays history during the call (7 notifications for a 1-turn session) | Yes, advertised and working. Returns `{sessions:[...],nextCursor}` with auto-derived `title`; optional `cwd` filter | Present: `initialize` result has `_meta.steering.supported = true`. Extension is `_session/steering`; bundle also defines `_codex/session/goal_control` | Proper `-32601` for `_test/bogus`, `totally/madeup`, `goose/customNotifications`, `_codex/bogus`. Unknown notifications silently ignored (spec-correct) | Yes. Magic word survived SIGKILL plus fresh-process load, at 18k cached input tokens for a 1-turn reload | Not blocked. Pre-existing `~/.codex/auth.json`. `initialize` is genuinely auth-free; every `session/*` method calls an internal `checkAuthorization()` per bundle source |
| `gemini-cli` | 0.53.0 | `true` | Implemented and dispatched, but auth-gated: fabricated id returns `-32000 "Authentication required"`, NOT `-32601`. Real path unreachable. Source shows a genuine disk-backed resume via `SessionSelector.resolveSession` then `geminiClient.resumeChat` then `streamHistory` (bundle `gemini-7M47OEXS.js:15092`). **Unverified on wire** | Absent. `-32601`. Present in the bundled ACP name table only; `SessionSelector.listSessions()` exists internally but is not exposed over ACP | Absent. No `_meta` at the top level of `initialize` and none in `agentCapabilities`. `_session/steering` returns `-32601`. An internal `experimental.modelSteering` prompt-injection path exists but is not wired to ACP | Proper `-32601`, offending method echoed in `data.method` | No, blocked by auth | Blocked. No `~/.gemini`, no `GEMINI_API_KEY`/`GOOGLE_API_KEY`, no ADC. `authenticate` with `oauth-personal` needs an interactive browser and was not attempted (credential acquisition) |

### Secondary capability read

| Fact | claude-agent-acp | codex-acp | gemini-cli |
|---|---|---|---|
| Negotiated `protocolVersion` | 1 | 1 | 1 (verified clamping: client sent 99, agent replied 1) |
| Other `sessionCapabilities` advertised | `additionalDirectories`, `close`, `delete`, `fork`, `list`, `resume` | `additionalDirectories`, `close`, `delete`, `list`, `resume` | none |
| `promptCapabilities` | image, embeddedContext | image, embeddedContext | image, audio, embeddedContext |
| `mcpCapabilities` | http, sse | http (acp false, sse false) | http, sse |
| Default session mode observed | `bypassPermissions` (inherited from ambient state, not requested) | `agent` (read+edit+run) | not reachable |
| Model/mode state returned inline at `session/new` | yes (`modes` + several KB of `configOptions`) | yes (`models` with 38 entries, `modes`, `configOptions`) | not reachable |
| Agent-to-client requests observed across all runs | zero | zero | zero |

### Steering outcomes actually observed

| Adapter | In-flight turn | Idle session | Idle with opt-in | Error shapes |
|---|---|---|---|---|
| claude-agent-acp | `{"outcome":"injected"}`, pushed onto the same SDK streaming input at priority `now`, pre-empting the current generation; the steered reply streams via `session/update`, not via the steer response | `{"outcome":"startedNewTurn"}`, and the adapter fires a **detached** `prompt()` whose result the client never receives | request `_meta {"steering":{"idleBehavior":"promptRequired"}}` gives `{"outcome":"promptRequired","reason":"noRunningTurn"}` and sends nothing | bad `idleBehavior`: `-32602 "Invalid params: unsupported steering idleBehavior"`. Unknown well-formed UUID: `-32603` with `details: "Session not found"` |
| codex-acp | `{"outcome":"injected"}`, NON-cancelling: the original turn continued to `stopReason: end_turn`. Proof: prompt "count 1 to 10" plus steer "append PINEAPPLE" produced `...10PINEAPPLE` | `{"outcome":"startedNewTurn"}` | not offered | param is `prompt` (array of content blocks), not `content`; wrong shape gives `-32602` with a zod `_errors` tree |
| gemini-cli | no ACP steering surface at all | n/a | n/a | `-32601` |

Timeline caveat on the claude-agent-acp steer run: the 6 s mid-turn delay in the driver was too long, so the probe labelled `steer-midturn` actually exercised the idle path and the probe labelled `promptRequired` hit the in-flight path. Both outcomes are genuine and match the adapter source (`dist/acp-agent.js:915-975`); only the labels in `results-steer.json` are off by one state.

## Open questions answered

**Q1 (line 229): which adapters support `session/load` today, and what does chat-session resume look like across daemon restarts?**
Answered, and the answer overturns the doc's risk line. `research/2026-07-31_14-56-19_buzz-acp-port.md:139` says "ACP sessions are not resumable across daemon restarts". That is **false for claude-agent-acp and codex-acp at the versions probed**. Both persist to their own on-disk session stores (the Claude Code session store, and codex rollouts under `~/.codex`), not to adapter process memory, so a SIGKILL of the adapter and a fresh process reloading the same sessionId recovers full conversational context. Both were proven with a secret-word round trip across a real process kill. Gemini-cli's resume is implemented and disk-backed per source read but is **unverified on wire** (blocked by credentials).

**Q2 (line 230): verify claude-agent-acp/codex-acp steering behaviour before promising broadcast-steer semantics.**
Answered. Steering can be promised for claude-agent-acp and codex-acp: both advertise `_meta.steering.supported = true` at `initialize`, both implement `_session/steering`, both return an `outcome` discriminator, and injection was proven to reach the model in both. It **cannot** be promised for gemini-cli today. Broadcast-steer therefore has to be a per-adapter capability, not a fleet-wide guarantee.

## Implications for the ainb chat-bus plan

**Resume strategy the daemon must own**

- Do not build a client-side transcript replay for resume. For claude-agent-acp and codex-acp the daemon only needs to persist the `(provider, sessionId, cwd)` tuple; the adapter's own store holds the history and replays it as `session/update` notifications during the `session/load` call. The daemon's job is durable sessionId storage plus a rehydration path, not a message-log shadow copy.
- **The `session/update` replay arrives before the `session/load` reply.** Any `AcpProviderAdapter` must have its notification handler live and routing to the right session before it issues `session/load`, otherwise the entire replayed history is dropped on the floor. This is the single most likely implementation bug in the port.
- **`cwd` is part of the session key.** Gemini scopes session files by a project identifier derived from the target dir (source-confirmed), and both working adapters accept a `cwd` filter on `session/list`. Store `cwd` alongside the sessionId and pass the same value on load.
- **Session config does NOT survive `session/load` on codex-acp** (measured: `currentModelId` reverted to the default and mode reverted to `agent`). The daemon must re-apply model, mode and reasoning effort after every load. Treat post-load config re-application as a mandatory step of the rehydrate routine, not an optimisation.
- **Pin the permission mode explicitly.** claude-agent-acp's `session/new` came back with `currentModeId: bypassPermissions` inherited from ambient state, and as a direct consequence zero agent-to-client requests fired across every run (no `session/request_permission`, no `fs/*`, no `terminal/*`); stderr confirmed `canUseTool will not be invoked`. If ainb wants the actionable-permission rows the research doc plans for, it must pin the mode at `session/new` or immediately via `session/set_mode`. Trusting the default silently disables the whole permission UX.
- **"Unknown session" is not typeable on codex-acp.** A fabricated id and a real-but-turnless session both return `-32603` with a free-text `data.details`, distinguishable only by string-matching `no rollout found` vs `is archived`. Wrap that string matching in exactly one place in the adapter and treat it as a known-brittle seam.
- **`session/delete` on codex-acp is a soft archive**, and there is no ACP method to unarchive (only the `codex unarchive` CLI). From the ACP surface, treat delete as irreversible. `session/close` returns `{}` and does not invalidate the session (a later `session/load` in the same process succeeded); it only releases in-process resources.
- Session **fork** is advertised by claude-agent-acp (`sessionCapabilities.fork`) and is directly interesting for broadcast/branching, but is **untested by this spike**.

**Whether steering can be promised, per adapter**

- claude-agent-acp: yes, with a required guard. The idle path fires a **detached** `prompt()` whose result the client never receives, which is a ghost turn from the harness's point of view. ainb must always send the request `_meta {"steering":{"idleBehavior":"promptRequired"}}` opt-in so an idle steer degrades into an explicit `promptRequired` outcome instead of an untracked turn.
- codex-acp: yes. Injection is non-cancelling and the original turn completes normally. Param name is `prompt` (array of content blocks). No idle opt-in is offered, so an idle steer will start a turn; branch on the returned `outcome` rather than trying to track in-flight state client-side. The `outcome` discriminator means one method covers both steer-and-prompt.
- gemini-cli: no. Fall back to queueing or to a plain `session/prompt`. Broadcast-steer must be advertised in ainb's capability bitset per adapter, and the UI must not offer steer for adapters that lack it.

**The codex-acp `{}` false-success hazard and the capability-gating rule it forces**

- The research doc's claim at `research/2026-07-31_14-56-19_buzz-acp-port.md:57`, that codex-acp answers unknown methods with `{}` success (buzz's stated reason for capability-gating Steer), is **not true at codex-acp 1.1.7**. Verbatim reply to a bogus method: `{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"\"Method not found\": _test/bogus","data":{"method":"_test/bogus"}}}`. Same `-32601` for the underscore-extension namespace, so `_`-prefixed methods are not a silent-success path. All three adapters return a well-formed `-32601` with the offending method echoed in `data.method`.
- Caveat, stated honestly: this spike cannot speak to older codex-acp versions where buzz may genuinely have observed `{}`. **The rule that follows is to assert a version floor, not to assume good behaviour.**
- Concrete gating rule for `AcpProviderAdapter`: gate on the declared flag first (`initialize` result `_meta.steering.supported`), fall back to method probing only when the flag is absent, and record the adapter name plus version from `agentInfo` so a floor can be enforced. Never infer a capability from a bare success reply.
- Error-code taxonomy to encode once in the adapter, valid across all three probed adapters: `-32601` means the method is genuinely absent; `-32602` means the method exists but params are wrong (this is how you positively confirm an extension exists without valid args); `-32603` plus free-text `data.details` means a business-logic failure; `-32000` on gemini-cli specifically means the method exists and the auth gate fired first. That last distinction makes "implemented but unauthenticated" separable from "not implemented" during capability discovery.
- A real trap that `-32601` probing does NOT catch: `session/set_config_option` takes `configId`, not `optionId`. The wrong name returns `-32602` and the run silently continues on the default model. Param-shape errors are only visible if the adapter checks every reply, so the ACP client must not fire-and-forget any request.

**Interop hazard specific to gemini-cli**

- `authenticate` with `methodId: oauth-personal` writes raw ANSI escape sequences and human prose to **stdout**, i.e. onto the JSON-RPC channel. First polluted line, `cat -v`: `^[[?1049h^[[2J^[[H^[[?1006l^[[?1002l^[[<u^[[?7hPlease visit the following URL to authorize the application:` followed by a bare accounts.google.com URL. The clean (non-auth) run produced zero non-JSON stdout lines and zero stderr bytes, so the pollution is specific to the interactive auth flow. Recommendation: ainb must never call `authenticate` for gemini-cli, and should require pre-provisioned credentials instead; the line reader should still tolerate and skip non-JSON lines defensively.

**Known gaps in this spike**

- `session/load` with a well-formed but nonexistent UUID on claude-agent-acp: **untested**. Only a malformed non-UUID string was tried. Steering with a nonexistent well-formed UUID gave `-32603 "Session not found"`, so load probably behaves similarly, but that is inference, not measurement.
- Permission-request handling is **unverified on every adapter**. Zero agent-to-client requests were issued across all runs because no prompt exercised a gated tool and claude-agent-acp ran in `bypassPermissions`. Needs a dedicated tool-using prompt.
- Advertised but untested: `session/fork`, `session/resume`, `providers/*`, `mcp/message`, `nes/*`, `authentication/*` on codex-acp; `session/fork`, `session/resume`, `session/close`, `session/delete` on claude-agent-acp; gemini-cli's entire post-auth surface including `session/prompt`, `session/update` streaming and `session/request_permission`.
- Gemini's `session/fork`, `session/resume` and `session/close` appear as schema constants in the bundle but were not probed; only `session/list` was empirically confirmed absent. Assume all four are absent.

## Raw evidence

All paths are absolute. Every `wire.log` contains ISO-timestamped raw bytes in both directions.

**claude-agent-acp**

| Path |
|---|
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/claude-agent-acp/wire.log` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/claude-agent-acp/probe.mjs` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/claude-agent-acp/results-probe.json` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/claude-agent-acp/results-prompt.json` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/claude-agent-acp/results-resume.json` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/claude-agent-acp/results-steer.json` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/claude-agent-acp/results-steer2.json` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/claude-agent-acp/state.json` |

Source corroboration (readable JS with doc comments): `node_modules/@agentclientprotocol/claude-agent-acp/dist/acp-agent.js:637` (`loadSession: true`), `:660` (the `_meta.steering.supported` advertisement), `:56` (`const STEER_METHOD = "_session/steering"`), `:915-975` (the three-branch `steer()` implementation matching the three observed outcomes), `:6055-6057` (`session.load` / `session.list` wiring).

**codex-acp**

| Path |
|---|
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/codex-acp/wire.log` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/codex-acp/probe.mjs` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/codex-acp/run1-capabilities.mjs` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/codex-acp/run2-prompt-and-steer.mjs` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/codex-acp/run3-respawn-load.mjs` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/codex-acp/run4-edge-cases.mjs` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/codex-acp/session-id.txt` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/codex-acp/node_modules/@agentclientprotocol/codex-acp/dist/index.js` |

The bundle is 1.1 MB unminified and greppable (`AGENT_METHODS` table, `agentRequestSpecs`, `AcpExtensions.ts` constants); it was used to cross-check every empirical result. codex-acp bundles `@openai/codex` and drives a `codex app-server` child, so it is a shim over codex's own app-server protocol rather than a reimplementation.

**gemini-cli**

| Path |
|---|
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/gemini-cli/wire.log` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/gemini-cli/wire-protocolversion.log` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/gemini-cli/probe.mjs` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/gemini-cli/pv.mjs` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/gemini-cli/help.log` |
| `/tmp/claude-1000/-home-claude--agents-in-a-box-worktrees-by-name-agents-in-a-box--f-buzz--19533eb1/1bf5746e-45a7-4119-8b64-a3c5f953ccfb/scratchpad/acp-spike/gemini-cli/npx-install.log` |

Source references: `bundle/gemini-7M47OEXS.js:15092` (`loadSession` implementation), `bundle/chunk-F3VE7C53.js:253130,253264` (`Storage.getProjectTempDir`, `listProjectChatFiles`, i.e. the cwd-scoped session store at `~/.gemini/tmp/<projectIdentifier>/chats/*.json`), `bundle/chunk-W5BZ47UJ.js:10115` (`SessionSelector.listSessions`, internal only), `bundle/chunk-F3VE7C53.js` (`USER_STEERING_INSTRUCTION`, in-app only). To close the gemini gap, re-run `probe.mjs` with `GEMINI_API_KEY` set in the child env.

## Install notes

| Adapter | Command | Result |
|---|---|---|
| claude-agent-acp | `npm i @agentclientprotocol/claude-agent-acp` | 0.64.0, 104 packages, 10 s. Binary `node_modules/.bin/claude-agent-acp`, plain stdio ndjson, no flags. Use the `@agentclientprotocol` org package; `@zed-industries/claude-code-acp` is at 0.16.2 and far behind |
| codex-acp | `npm i @agentclientprotocol/codex-acp` | 1.1.7, 19 packages, 9 s. Binary `node_modules/.bin/codex-acp` |
| gemini-cli | `npx -y @google/gemini-cli` | 0.53.0, roughly 2 min. Flag is `--acp`; `--experimental-acp` still accepted but marked deprecated |

Whole codex-acp spike ran about 4 minutes wall clock and roughly 37k tokens of codex usage across 3 prompts. No credential values were read, printed or logged for any adapter.
