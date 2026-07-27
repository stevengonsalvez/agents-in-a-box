# Agent entity: Multica vs Hangar

## Multica Agent

### Schema (full field list, `agent` table)

Assembled from `server/migrations/001_init.up.sql:36-46` plus every subsequent `agent_*` migration.

| Field | Type | Meaning | Migration |
|---|---|---|---|
| `id` | UUID PK | — | 001 |
| `workspace_id` | UUID FK → workspace, cascade | tenant | 001 |
| `name` | TEXT | display name | 001 |
| `avatar_url` | TEXT nullable | avatar image | 001 |
| `runtime_mode` | TEXT CHECK `local|cloud` | legacy pre-runtime-table field, still present | 001 |
| `runtime_config` | JSONB default `{}` | provider-specific config blob (e.g. openclaw gateway `{gateway:{token}}`) | 001 |
| `visibility` | TEXT CHECK `workspace|private`, default `private` (030) | legacy/derived; superseded as auth source by `permission_mode` (130) but kept in sync for old clients | 001, 030 |
| `status` | TEXT CHECK `idle|working|blocked|error|offline`, default `offline` | legacy column; front-end no longer reads it for presence — see derive-presence below | 001 |
| `max_concurrent_tasks` | INT, default 1→6 (023) | dispatch cap | 001, 023 |
| `owner_id` | UUID FK → user, nullable | — | 001 |
| `runtime_id` | UUID NOT NULL FK → `agent_runtime`, RESTRICT | every agent binds to exactly one runtime row (004 backfilled this from the old `runtime_mode`/`runtime_config`) | 004 |
| `description` | TEXT NOT NULL default `''`, CHECK `char_length<=255` (060) | — | 002, 060 |
| `skills` | TEXT NOT NULL default `''` | legacy free-text skills field (superseded by `agent_skill` join) | 002 |
| `tools` | JSONB default `[]` | legacy tool list | 002 |
| `triggers` | JSONB default `[]` | legacy trigger config | 002 |
| `instructions` | TEXT NOT NULL default `''` | system prompt | 021 |
| `archived_at` | TIMESTAMPTZ nullable | soft-archive marker; NOT NULL = archived | 031 |
| `archived_by` | UUID FK → user, nullable | audit: who archived it | 031 |
| `custom_env` | JSONB default `{}` | user env vars injected at subprocess launch (router/Bedrock/Vertex creds); **never serialized** in API responses — only `has_custom_env`/`custom_env_key_count`; read/write via dedicated audited `GET/PUT /api/agents/{id}/env` | 040 |
| `custom_args` | JSONB default `[]` | extra CLI args appended at launch | 041 |
| `mcp_config` | JSONB nullable | agent's own MCP server config; redacted (`mcp_config_redacted`) for non-owners | 046 |
| (constraint) | UNIQUE(workspace_id, name) | 409 instead of silent dup/500 | 046 |
| `model` | TEXT nullable | explicit per-agent model override (first-class column so UI can render a dropdown; some providers like Codex app-server reject `-m` in custom_args) | 050 |
| `thinking_level` | TEXT nullable | runtime-native reasoning/effort token (Claude: `low|medium|high|xhigh|max`; Codex: `none|minimal|low|medium|high|xhigh`); NULL = provider default, deliberately NOT normalized cross-runtime | 095 |
| `kind` | TEXT CHECK `user|system`, default `user` | distinguishes ordinary agents from hidden system agents (e.g. the agent-builder carrier) | 163 |
| `system_key` | TEXT nullable | identity key for system agents; unique per `(workspace_id, owner_id, runtime_id, system_key)` where not null (172) | 163, 172 |
| `disabled_runtime_skills` | JSONB default `[]` | per-agent tool/skill disable list at the RUNTIME level (distinct from workspace `agent_skill` attach/detach) | 206 |
| `service_tier` | TEXT nullable | per-agent Codex service-tier override (runtime-native catalog id, e.g. `"priority"` shown as "Fast"); NULL = inherit local Codex config | 212 |
| `permission_mode` | TEXT CHECK `private|public_to`, default `private` | AUTHORITATIVE invocation-permission source (MUL-3963), replacing `visibility`; `private` = owner-only, admin does NOT bypass (fixes a Composio mailbox-leak privacy hole) | 130 |
| `composio_toolkit_allowlist` | TEXT[] | Composio toolkit slugs this agent may mount as MCP for any run that passes the invocation gate, using the OWNER's Composio connection; redacted for non-owners | (Composio-era, commented in 130) |

Companion table `agent_invocation_target` (130): `id`, `agent_id` (no FK — app-layer cleanup), `target_type` CHECK `workspace|member|team`, `target_id` (polymorphic), `created_by`, `created_at`; UNIQUE(agent_id, target_type, target_id). Targets stack — `canInvokeAgent` OR-matches across all rows for a `public_to` agent.

Separate `agent_runtime` table (004): `id`, `workspace_id`, `daemon_id`, `name`, `runtime_mode` (`local|cloud`), `provider`, `status` CHECK `online|offline`, `device_info`, `metadata` JSONB, `last_seen_at`. One physical daemon+provider = one runtime row; an agent binds to exactly one.

`agent_skill` join gained `enabled BOOLEAN NOT NULL DEFAULT TRUE` (161) — per-agent, per-skill on/off toggle independent of the skill's global existence.

### States / status model

Legacy `agent.status` column (idle/working/blocked/error/offline) still exists but the front-end does **not** read it for presence. Presence is entirely re-derived client-side (`packages/core/agents/derive-presence.ts:1-155`) from two orthogonal, independently-computed dimensions:

1. **`AgentAvailability`** (`types.ts:23-33`) = `online | unstable | offline | archived` — pure function of runtime reachability (`deriveRuntimeHealth`), folding `about_to_gc` into `offline`. `unstable` is a transient ~5-minute amber grace window after the runtime goes unreachable, decaying to `offline` if no fresh heartbeat lands (hence a 30s poll on consuming hooks). `archived` (agent.archived_at set) wins over everything — checked BEFORE runtime health so a leftover "online" runtime row can never make a retired agent look live.
2. **`Workload`** (`derive-presence.ts:38-46`) = `working | queued | idle` — pure function of *live* task counts only (`running>0 → working`; else `queued>0 → queued`; else `idle`). Deliberately excludes terminal states (completed/failed/cancelled) — history lives on the detail page / Inbox, never bleeds into the list-level dot.

`buildPresenceMap` batches this over a whole workspace in one O(N) pass (group tasks by agent_id, one runtimesById lookup) for list/card views.

### Create-flow inputs (`CreateAgentRequest`, `agent.go:927-965`)

`name`, `description`, `instructions`, `avatar_url`, `runtime_id` (required), `runtime_config`, `custom_env` (map), `custom_args`, `mcp_config`, `visibility` (legacy), `permission_mode` + `invocation_targets` (authoritative when present), `max_concurrent_tasks`, `model`, `thinking_level`, `service_tier`, `composio_toolkit_allowlist`, `template` (which quick-create template slug seeded it, for the `agent_created` analytics event), `skill_ids` (attached in the same transaction as the row, so create is never partially-configured).

### Agent Builder / quick-create (`agent_builder.go:1-120`)

A conversational, LLM-driven creation flow: `CreateAgentBuilderSession` spins up a hidden **system agent** (`kind='system'`, `system_key='agent_builder:<flowID>'`, name `.multica-agent-builder-<flowID>`) on an existing runtime, running a fixed system prompt (`agentBuilderInstructions`) that interviews the user and must end every turn with a strict `<agent_draft>{"name","description","instructions","model","skill_ids","permission_scope","member_ids"}</agent_draft>` JSON block. The builder agent itself never appears in normal lists / assignee pickers. Rules baked into the prompt: model must be empty or an exact id from the caller-supplied available-models list (never invent), skill_ids must be from the caller-supplied available-skills list, permission_scope defaults to `private`, never leaks secrets into the draft, and the LLM never claims the agent was actually created — the user must confirm in the UI. This is the actual "agent builder" UX: chat → structured draft → user reviews/edits → confirms → real `CreateAgent` call.

### Per-provider config (`server/pkg/agent/*.go`)

One Go file per provider (`claude.go`, `codex.go`, `cursor.go`, `copilot.go`, `grok.go`, `kimi.go`, `qwen.go`, `openclaw.go`, `deveco.go`, `hermes.go`, `qoder.go`, `pi.go`, `kiro.go`, `traecli.go`, `codebuddy.go`, `antigravity.go`, `opencode.go`...) — roughly 15 first-party provider integrations, each 400-3000 lines covering: invocation shape (argv/stdin/app-server RPC), model catalog + validation, thinking/reasoning-effort mapping (`thinking.go`, `thinking_test.go` — 785/1131 lines, a dedicated cross-provider effort-level module), MCP config translation (`mcp_config.go`, `browser_mcp_config.go`, `opencode_mcp.go`), version detection (`version.go`), stream-JSON result parsing (`stream_json_result.go`), and process lifecycle (`proc_other.go`/`proc_windows.go`). This is a full per-CLI adapter layer, not a single generic "provider" enum.

### Permissions / effective access (`packages/core/agents/effective-access.ts`)

Front-end derives a 3-state `AccessScope` (`workspace | specific-people | owner-only`) from the authoritative `permission_mode` + `invocation_targets`, because the legacy 2-state `visibility` is lossy (a `public_to` agent scoped to specific people collapses to `visibility:"private"`, indistinguishable from truly owner-only). Mirrors the server's `canInvokeAgent` gate.

---

## Hangar Agent (ainb)

### Schema / struct fields (`crates/ainb-hangar-store/src/repo/agent.rs:39-82`, migrations 0002/0006/0015/0041/0042)

| Field | Type | Meaning | Migration |
|---|---|---|---|
| `id` | TEXT PK (ULID) | — | 0002 |
| `workspace_id` | TEXT FK → workspace | — | 0002 |
| `name` | TEXT NOT NULL | — | 0002 |
| `runtime_id` | TEXT NOT NULL FK → agent_runtime | required, same "always bound" invariant as multica | 0002 |
| `instructions` | TEXT nullable | system prompt | 0002 |
| `visibility` | TEXT CHECK `workspace|private` | only auth model that exists — no `permission_mode`/allow-list equivalent | 0002 |
| `owner_id` | TEXT FK → user | — | 0002 |
| `max_concurrent_tasks` | INTEGER NOT NULL default 1 | dispatch cap; column exists but is **not exposed on the `Agent` struct** (dispatch reads it via raw SQL separately) | 0006 |
| `archived` | INTEGER (bool) CHECK 0/1, default 0 | boolean only — no timestamp, no "archived_by" audit trail | 0015 |
| `model` | TEXT nullable | provider model override | 0015 |
| `cli_args` | TEXT (JSON array) default `[]` | extra CLI args | 0015 |
| `mcp_config` | TEXT (JSON object) default `{}` | raw MCP config; no ownership/redaction model | 0015 |
| `thinking` | TEXT nullable | reasoning/effort level | 0015 |
| `agent_env` | TEXT (JSON object) default `{}` | per-agent env vars; stored/returned in plain — no `has_custom_env`/key-count redaction contract | 0015 |
| `provider` | TEXT nullable | which CLI backend (`claude`/`codex`/`copilot`); NULL = fall back to runtime's advertised provider | 0041 |
| `token_budget` | INTEGER nullable | optional rtk/headroom cap; stored + surfaced only — no dispatch-time enforcement yet | 0042 |

`agent_runtime` (0002): `id`, `workspace_id`, `daemon_id`, `provider`, `runtime_mode` (`local|cloud`), `last_seen_at`, `status` default `'offline'` — no explicit CHECK constraint value list shown, effectively binary online/offline, no grace-window/`unstable` state.

`agent_skill` join (0002): `agent_id`, `skill_id` composite PK, plus `enabled INTEGER NOT NULL DEFAULT 1` (0051, gap #24) — the per-agent skill on/off toggle, orthogonal to attach/detach. `agent.disabled_runtime_skills` (0051) is the by-NAME runtime suppression list (multica 206). Both are honoured at dispatch-time materialisation, not at a live tool registry (hangar has none).

No `description`, `avatar_url`, `kind`/`system_key`, `service_tier`, `permission_mode`/invocation-target table, `composio_toolkit_allowlist`, or name-uniqueness constraint exist on the hangar `agent` table.

### What create actually collects today

- **CLI** (`crates/ainb-core/src/cli/hangar/mod.rs:416-430`, `AgentCreateArgs`): `--name` (required), `--provider` (optional, default claude), `--instructions` (optional), `--workspace` (optional). That's it — no model, no thinking level, no mcp_config, no env, no max_concurrent, no skills at create time (all of those exist only via the separate `hangar agent edit` command, which does cover model/instructions/args/mcp/thinking/env/token-budget with explicit `--clear-*` flags for the four nullable fields).
- **TUI Agents screen** (`crates/ainb-plugin-hangar/src/screen/agents.rs:59-292`): even narrower — `n` opens a single inline "New agent name:" text buffer (`create_input: Option<String>`); Enter with a non-blank name submits, Esc cancels. No provider picker, no instructions, no model/thinking — the screen literally only collects a name; everything else must be set afterward via CLI `agent edit`.
- No agent-builder / conversational quick-create flow exists anywhere in ainb.

---

## GAPS

| Multica has | Hangar has | Gap | Effort |
|---|---|---|---|
| Two-dimensional derived presence: `AgentAvailability` (online/unstable/offline/archived, with a 5-min "unstable" grace window before decaying to offline) × `Workload` (working/queued/idle from LIVE task counts only, terminal states excluded) | `agent.archived` boolean + `agent_runtime.status` binary online/offline; no grace window, no separate workload signal, no pure derivation module | Users can't tell "runtime just blipped" from "truly dead", and there's no "queued but stuck" signal at all | M — port `derive-presence.ts`'s two pure functions + a task-count query; biggest single UX gap since it drives every list/card dot |
| `permission_mode` (private/public_to) + `agent_invocation_target` allow-list (workspace/member/team, OR-matched); admin does NOT bypass private | `visibility` column only (workspace/private), no allow-list, no explicit invoke-gate function | Can't share one agent with a subset of people; no `canInvokeAgent`-equivalent gate exists to audit | M — new join table + gate function; can start as a straight port of the 130 migration shape |
| Conversational **Agent Builder**: chat with a hidden system agent that proposes name/description/instructions/model/skills/permission as a structured draft the user reviews before real creation | Only a bare name-input field (TUI) or flag-driven CLI create; no assisted authoring at all | New/non-technical users must hand-write instructions and guess model ids with zero help | L — needs `kind='system'`/`system_key` support first, then the draft-loop prompt + parser |
| Full per-provider adapter layer (~15 providers, each with model catalog validation, thinking/effort mapping, MCP config translation, stream-JSON parsing, version detection) | 3 providers (`claude`/`codex`/`copilot`) recorded as a bare string with no per-provider model/thinking validation surfaced | Free-text `model`/`thinking` fields can silently hold invalid values per provider; no catalog to drive a UI dropdown | L — long tail, but even a validation-only pass for the 3 existing providers is worthwhile |
| `description` (255-char capped), `avatar_url`, `kind`(user/system), `service_tier` (Codex), `composio_toolkit_allowlist` w/ owner-based redaction, UNIQUE(workspace,name) | None of these columns exist | Agents have no blurb/avatar in lists; duplicate names silently allowed; no service-tier control for Codex agents | S–M — mostly additive columns + one unique index; cheapest wins here |
| ~~`agent_skill.enabled` per-agent toggle + `disabled_runtime_skills`~~ **CLOSED (#24, migration 0051)** | Both columns exist; `SkillRepo::skills_for_agent` filters `enabled = 1` and the materialiser drops names in `disabled_runtime_skills` | (closed) A disabled link stays attached and still counts as `used`; only its materialisation is suppressed | Landed — deviations D1 (materialisation, not a tool registry), D2 (attach never re-enables), D3 (`used` stays attachment-based) |
| `archived_at` + `archived_by` (who/when audit) | `archived` boolean only | No audit trail for who archived an agent or when | S — swap boolean for nullable timestamp + owner FK (mirrors 031 exactly) |
| `custom_env` never serialized in API responses (has_custom_env/key_count only), read/write gated behind a dedicated audited endpoint, `canViewAgentSecrets` gate | `agent_env` stored and presumably returned as plain JSON with no redaction contract | Lower stakes for a local single-user TUI, but still a good hygiene gap if hangar ever grows multi-user/remote access | S — mostly a serialization-layer change since hangar is local-first |

### Top gaps, ranked by user-visible impact

1. **Presence/status derivation** — biggest UX gap; hangar's binary online/offline + boolean archived can't express "flaky" or "stuck/queued", which multica's whole list UI is built around.
2. **Invocation permission (private/public_to + allow-list)** — hangar has no way to share an agent with specific people; only global workspace vs fully private.
3. **Agent Builder conversational create** — multica's create flow is guided; hangar's is a bare name field, with model/instructions/provider only settable after the fact via CLI edit.
4. **Per-provider adapter depth** — multica validates/maps model+thinking per provider; hangar stores free-text with no validation, so bad values fail silently at dispatch instead of at create/edit time.
5. **Missing metadata columns** (`description`, `avatar_url`, `kind`/`system_key`, `service_tier`) — cheap, additive, but currently absent entirely.
6. ~~**Per-agent skill enable/disable + disabled_runtime_skills**~~ **DONE (#24)** — migration 0051; `hangar skills toggle <skill> --agent <agent> --enabled <bool>`, `t` on the skill-manager screen, and a disabled link never reaches the agent's task tree.
7. **Archive audit trail** (`archived_at`/`archived_by` vs plain boolean) — small but real accountability gap.
8. **Name uniqueness constraint** — multica prevents duplicate agent names per workspace at the DB level; hangar does not (unclear if enforced at all).
