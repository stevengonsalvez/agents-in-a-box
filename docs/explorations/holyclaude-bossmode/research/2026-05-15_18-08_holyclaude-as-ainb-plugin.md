# Research: Integrating HolyClaude as an ainb container plugin

**Date**: 2026-05-15 18:08
**Repository**: agents-in-a-box
**Branch**: feat/holyclaude
**Commit**: da76e948cf94999a409e7af03dc64eacfeac4558
**Research Type**: Comprehensive (web + codebase)

## Research Question

Look at CoderLuii/HolyClaude. What does it offer? What can be integrated into ainb as a plugin mechanism? See feat/plugin branch for plugin docs. End goal: run claude/codex/copilot as containers via ainb without the ainb team maintaining Docker images upstream — even if it means pulling HolyClaude as a dependency.

## Executive Summary

HolyClaude is a **Docker image + compose template kit** (not a CLI, not a library) that bakes Claude Code + 6 other agent CLIs into a single container with a web UI on `:3001`. Pulling it as an ainb dependency means shelling out to `docker compose` against `coderluii/holyclaude:1.2.2` — there is no library API to bind to. The ainb plugin mechanism on `feat/plugin` is wasm/JSON-RPC based with a strict capability model — and it has **one blocker**: `spawn_subprocess` is declared in the manifest schema but has **no `host/process/spawn` host function yet** (deferred to Phase 7+). Recommendation: ship a **host-side container provider today** (Track A) and migrate to a **proper plugin** once the subprocess host fns land (Track B).

## Key Findings

- **HolyClaude has no CLI/SDK/library** — only Docker images on Docker Hub (`coderluii/holyclaude:latest|slim|1.2.2`). Integration = `docker compose` + bind mounts + env vars.
- **One container holds all 7 agents** (Claude, Codex, Gemini, Cursor, TaskMaster, Junie, OpenCode). Single port `:3001` exposes CloudCLI web UI. Auth persists via `./data/claude` bind mount.
- **License gotcha**: HolyClaude itself is MIT, but vendored CloudCLI inside the image is **GPL-3.0**. Redistributing pre-built images = legal review.
- **Plugin mechanism shape**: TOML manifest, wasm32-wasip1 cdylib, JSON-RPC over stdio, capability via linker omission, 50ms fuel budget per call, 64 KiB buffers.
- **Critical gap**: `spawn_subprocess` capability flag exists in manifest schema but **no host function backs it**. Plugins literally cannot launch `docker compose` today.
- **Pragmatic path**: Host-side `ainb container` subcommand drives HolyClaude directly. No plugin work needed. Migrate to plugin in Phase 7.

## Detailed Findings

### HolyClaude — What Ships

| Artifact | Where | Use |
|---|---|---|
| Docker image | `coderluii/holyclaude:1.2.2` (Docker Hub) | `docker pull` |
| Compose template | `docker-compose.yaml` in repo | `docker compose up -d` |
| Entrypoint script | `scripts/entrypoint.sh` | UID/GID remap, auth sync loop |
| First-boot seed | `scripts/bootstrap.sh` | Seeds settings/memory on `~/.claude/.holyclaude-bootstrapped` sentinel |
| Notification hook | `scripts/notify.py` | Apprise integration on `stop`/`error` events |
| Vendored CloudCLI | `vendor/artifacts/siteboon-claude-code-ui-1.26.3.tgz` (patched) | Web UI on `:3001` (GPL-3.0) |

**Container model**: one long-running container per user, **not per-session, not per-project**. Sessions multiplex inside CloudCLI UI. `network_mode: bridge`, requires `SYS_ADMIN + SYS_PTRACE + seccomp=unconfined + shm_size: 2g` for Chromium.

**Auth wiring**: zero env-var injection at start. OAuth runs in browser through CloudCLI; tokens persist on bind-mounted volume; entrypoint syncs `~/.claude.json` every 60s.

**Update cadence**: 14 releases v1.0.0 → v1.2.2 over 3 weeks (Mar 22 – Apr 10 2026), then **5-week silent gap**. Pin versioned tags, never `:latest`.

### ainb Plugin Contract (feat/plugin)

**Manifest** (`plugin.toml`):
```toml
[plugin]
name = "burndown"
version = "2.0.0"
abi_version = 2

[capabilities]
read_sessions     = true
write_plugin_data = true
event_bus         = true
network           = []                # allowlist form: ["api.example.com:443"]
spawn_subprocess  = false             # ❌ declared but no host fn
read_claude_logs  = false

[provides]
screens        = ["analytics"]
commands       = ["/usage", "/burndown"]
cli_namespaces = ["usage"]

[lifecycle]
spawn          = "lazy"               # or "eager"
idle_reap_secs = 600
```

**Host function catalogue** (the locked surface):

| Method | Cap gate | Notes |
|---|---|---|
| `host/snapshot/get` | `event_bus` | Pull latest topic payload |
| `host/snapshot/publish` | `event_bus` | Push notification on topic |
| `host/snapshot/subscribe` | `event_bus` | Auto-receive via `plugin/handle_event` |
| `host/action/invoke` | `event_bus` | RPC w/ timeout |
| `host/log` | none | tracing line |
| `host/fs/read_dir` / `read_file` | `read_claude_logs` / `read_codex_logs` | path-canonicalized |
| `host/network/fetch` | `network` (allowlist) | HTTPS only |
| ❌ `host/process/spawn` | — | **Does not exist. Phase 7+** |

**Plugin entry points** (host → plugin JSON-RPC):
- `plugin/init`, `plugin/render`, `plugin/handle_event`, `plugin/handle_key`, `plugin/cli_dispatch`, `plugin/shutdown`

**Existing plugins**:
- `ainb-plugin-burndown` — TUI analytics + `/usage` CLI namespace
- `ainb-plugin-session-reader` — data plane, walks Claude/Codex/Gemini logs, publishes `sessions.usage_data`

**Spec docs**: `docs/plugin-spec/v1.md`, `docs/plugin-authoring.md`, `docs/plugins.md`, `ainb-tui/plans/plugin-phase-6-data-plane-spec.md`, `plans/plugin-phase-7-runtime-redesign.md` (stub).

### Integration Tracks

**Track A — Host-side container provider (ship now)**

```text
ainb container up [--profile slim]     → docker compose -f <bundled> up -d
ainb container down                     → docker compose down
ainb container exec -- <cmd>            → docker exec -it holyclaude <cmd>
ainb container logs --follow            → docker logs -f holyclaude
ainb container open                     → open http://localhost:3001
ainb container update                   → docker compose pull && up -d
```

- Bundle `docker-compose.yaml` template inside ainb release
- Pin `coderluii/holyclaude:1.2.2` (env-overridable `AINB_HOLYCLAUDE_TAG`)
- Bind mounts default to `~/.agents-in-a-box/holyclaude/{claude,workspace}/` (per-user, shared across worktrees — cwd-keyed designs are wrong on volatile worktrees per memory)
- Pre-flight checks: docker engine present, Chromium caps available, port 3001 free, host arch supported
- Status snapshot: emit `containers.status` event for future plugins to consume

Pros: lands this week, no plugin churn, full subprocess control.
Cons: container concerns leak into host. Couples ainb to Docker as a runtime dep. Inconsistent with "everything is a plugin" north star.

**Track B — `ainb-plugin-holyclaude` (Phase 7)**

Becomes possible once `host/process/spawn`, `host/process/wait`, `host/process/kill`, `host/process/stdio_stream` land.

```toml
# ainb-plugin-holyclaude/plugin.toml
[plugin]
name        = "holyclaude"
version     = "0.1.0"
abi_version = 7                # Phase 7 ABI

[capabilities]
spawn_subprocess = true
network          = ["registry-1.docker.io:443", "auth.docker.io:443"]
write_plugin_data = true
event_bus        = true

[provides]
cli_namespaces = ["container"]
commands       = ["/container"]
```

- Plugin owns full data plane: discovery, pull, lifecycle, status
- Publishes `containers.status` snapshot for dashboards
- Host has zero docker refs (per memory: "Plugin owns full data plane")
- Pull HolyClaude as a **versioned image dependency**, not a code dependency

Pros: clean separation, host stays small, hot-swappable image set.
Cons: blocked on subprocess host fns. Each `docker exec` round-trip pays JSON-RPC + msgpack overhead. Streaming logs needs careful budget accounting (50ms per call).

### Why Not Just Track B Now?

Per the plugin map: `spawn_subprocess = true` in the manifest **is rejected at install time today** because there is no linker import to gate. The Phase 6 spec explicitly defers subprocess execution. Workaround would be a host-side CLI handler that the plugin RPCs into — but that's the same as Track A with extra hops.

### Risks / Open Questions

1. **GPL-3.0 redistribution** — vendored CloudCLI inside HolyClaude image. Track A pulls the image from Docker Hub (no redistribution by ainb). Track B same. Confirm with legal that "users pull a third-party image from a third-party registry on first run" is not a redistribution event for ainb itself.
2. **5-week commit gap on HolyClaude** — pin to `1.2.2`, monitor; have a fallback plan if project goes dormant (own thin Dockerfile derived from HolyClaude's).
3. **Restricted CI environments** — `SYS_ADMIN + seccomp=unconfined + shm_size:2g` unavailable on GitHub Actions default runners, Fargate, Cloud Run. ainb should detect and refuse cleanly.
4. **Codex bwrap on Synology** — top open issue (#16). If ainb's user base overlaps Synology owners, escalate to HolyClaude or workaround.
5. **Per-session containers vs single container** — HolyClaude is single-container. If ainb wants ephemeral per-task containers, HolyClaude is the wrong tool and we'd need our own image.

## Recommendations

1. **Ship Track A as `ainb container` subcommand within 1-2 weeks.** Bundled compose template, pinned image tag, pre-flight checks, status snapshot.
2. **Open a feat/plugin issue for `host/process/*` host functions** scoped explicitly for Track B. Include this design as the canonical consumer.
3. **Add `AINB_HOLYCLAUDE_TAG` env override** and document that we pin versioned tags, not `:latest`.
4. **Document the GPL-3.0 boundary** in ainb's release notes — image is pulled, not redistributed.
5. **Add a `containers.status` event topic to the snapshot bus now**, even in Track A — so the eventual plugin migration is a swap, not a rewrite.

## Open Questions

- Should ainb ship its own thin Dockerfile derived from HolyClaude as insurance against project dormancy?
- Per-user single container, or per-worktree (per-project) container? Memory: "Stevie works exclusively off dynamic short-lived worktrees" → per-worktree is probably correct, but HolyClaude wasn't designed for that.
- Auth multiplexing: if a user runs ainb container in two worktrees, do they share `~/.claude` or get isolated trees?

## Code References

- HolyClaude `Dockerfile:152-168` — agent install list
- HolyClaude `docker-compose.yaml:264-268` — Linux cap requirements
- HolyClaude `scripts/entrypoint.sh:47-87` — auth sync loop
- ainb `docs/plugin-spec/v1.md` — plugin contract
- ainb `ainb-tui/crates/ainb-plugin-burndown/plugin.toml` — canonical manifest
- ainb `ainb-tui/plans/plugin-phase-6-data-plane-spec.md` — subprocess deferral note
- ainb `plans/plugin-phase-7-runtime-redesign.md` — where `host/process/spawn` will land
