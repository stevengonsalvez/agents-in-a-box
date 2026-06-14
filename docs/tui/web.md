---
title: "ainb web — fleet dashboard"
---

`ainb web` serves a **read-only**, browser-based dashboard for your agent fleet:
the live session list, fleet `needs` (ASK / ERR / IDLE / WAIT), and cost
rollups — updated live over Server-Sent Events without a manual refresh.

It is intended as a glanceable view of *what's running, what needs attention,
and what it's costing* — on localhost, or over a tailnet with a bearer token.

> **Read-only by design.** This cut has no mutate endpoints, no terminal
> bridge, and no web-push. Those are deliberate, clean extension points.

---

## Quick start

```bash
# Loopback, no token needed (default bind 127.0.0.1:8420)
ainb web

# Pick a port
ainb web --listen 127.0.0.1:9000

# Expose over a tailnet / LAN — a token is REQUIRED for a non-loopback bind
ainb web --listen 0.0.0.0:8420 --token "$(openssl rand -hex 24)"
```

Then open the printed URL, e.g. <http://127.0.0.1:8420>.

When a token is set, append it once to the URL —
`http://host:8420/?token=YOUR_TOKEN` — the page stores it for the session and
strips it from the address bar.

---

## Flags

| Flag | Default | Meaning |
|------|---------|---------|
| `--listen <ADDR>` | `127.0.0.1:8420` | Address to bind. Non-loopback requires `--token`. |
| `--token <SECRET>` | _(none)_ | Bearer token required on every `/api/*` route. Enables a non-loopback bind. |
| `--insecure-bind` | `false` | Allow a non-loopback bind with **no** token. Not recommended. |

---

## Security model

The bind policy mirrors agent-deck's `CheckBindSecurity` and is enforced
**before any socket is opened** — an unsafe bind is refused and never listens.

First matching rule wins:

1. A bearer `--token` is configured → **allow** (the surface is authenticated).
2. `--insecure-bind` was passed → **allow** (explicit operator override).
3. The host is loopback (`127.0.0.0/8`, `::1`) → **allow**.
4. Otherwise → **refuse** with a clear message.

```text
$ ainb web --listen 0.0.0.0:8420
Error: refusing to bind to non-loopback address 0.0.0.0:8420 without authentication.
This would expose the dashboard to your network unauthenticated.
Pass --token <secret> to require a bearer token, or --insecure-bind to override.
```

When a token is configured:

- Every `/api/*` route (and `/healthz`) returns **401** without
  `Authorization: Bearer <token>`, **200** with the correct token.
- The token is compared in **constant time** (no timing oracle).
- The SSE endpoint additionally accepts the token via `?token=…` query string,
  because browsers cannot set headers on an `EventSource`. The frontend strips
  it from the URL after connecting.

The static frontend shell (`/`, `/static/*`) is served **without** auth — only
data routes are gated.

---

## Routes

All API responses are JSON. Errors use `{ "error": { "code", "message" } }`.

| Method | Path | Response |
|--------|------|----------|
| GET | `/` , `/static/*` | Embedded vanilla-JS frontend (no auth). |
| GET | `/healthz` | `{ ok, readOnly, tokenRequired, version }` |
| GET | `/api/snapshot` | `{ sessions, needs, cost }` — the full dashboard payload. |
| GET | `/api/sessions` | Live session list (proxies `ainb --format json list`). |
| GET | `/api/needs` | Fleet needs (proxies `ainb --format json fleet needs`). |
| GET | `/api/cost` | Cost rollups (proxies `ainb --format json fleet cost`); `null` when absent. |
| GET | `/api/events` | **SSE** stream of `snapshot` events (`event: snapshot`). |

`401 UNAUTHORIZED` — missing/invalid bearer (only when a token is configured).
`502 UPSTREAM_FAILED` — an underlying `ainb` command failed.

---

## How data flows

The dashboard never re-implements data access. It drives the **same**
`ainb --format json …` commands the CLI and TUI expose, so the browser view can
never drift from the terminal view:

- `ainb --format json list` → session rows
- `ainb --format json fleet needs` → attention cards
- `ainb --format json fleet cost` → cost rollups (best-effort)

A background poller refreshes the snapshot every 2s and pushes an SSE event to
connected clients **only when the content fingerprint changes**, so idle fleets
cost nothing and updates land within ~2s of a state change. The poller is
skipped entirely when there are no SSE subscribers.

### Cost graceful degradation

`ainb fleet cost` ships with the **fleet cost-surface** feature. If it isn't
present in your build, the dashboard does not fail — the Cost panel shows
"Cost surface not available in this build" and everything else works.

---

## Environment variables

| Variable | Effect |
|----------|--------|
| `AINB_BIN` | Path to the `ainb` binary the dashboard shells out to. Defaults to the running executable, else `ainb` on `PATH`. |
| `AINB_HOME` | Base dir for `~/.agents-in-a-box` (so the proxied `ainb` commands read the right `sessions.json`). |

---

## Architecture notes

- New core crate `ainb-web` (axum HTTP + SSE), wired in as the `ainb web`
  subcommand via `cli/registry.rs`.
- The frontend is a single embedded vanilla-JS bundle (`rust-embed`) — **no
  Node build step, no framework, no runtime filesystem dependency**.
- A long-lived HTTP server fits a core crate better than the sandboxed v2
  plugin runtime, hence a crate (not a plugin).

### Reserved extension points (non-goals in this cut)

- Write/mutate endpoints (the `read_only` flag is already surfaced).
- A WebSocket terminal bridge.
- Web-push / PWA.
