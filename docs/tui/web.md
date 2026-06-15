---
title: "ainb web — fleet dashboard"
---

`ainb web` serves a browser-based dashboard *and remote-control surface* for
your agent fleet: the live session list, fleet `needs` (ASK / ERR / IDLE /
WAIT), and cost rollups — updated live over Server-Sent Events — plus a **live
in-browser terminal** for any running session and **web-push notifications**
when a session needs attention. It is an installable PWA.

It is intended as a glanceable view of *what's running, what needs attention,
and what it's costing* — and a way to actually *drive* a session — on
localhost, or over a tailnet with a bearer token.

> **The terminal is the one write surface.** Everything else (sessions, needs,
> cost, SSE) is read-only. The terminal attaches to a session's tmux pane, so
> it is gated behind auth **and** refused in `--read-only` mode. Run
> `ainb web --read-only` for a pure viewer.

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
| `--token <SECRET>` | _(none)_ | Bearer token required on every `/api/*` route + the WS terminal. Enables a non-loopback bind. |
| `--insecure-bind` | `false` | Allow a non-loopback bind with **no** token. Not recommended. |
| `--read-only` | `false` | Viewer-only: disable the live terminal (the `/ws/session/{id}` upgrade is refused with `403`). |

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

- Every `/api/*` route (and `/healthz`, and the WS terminal) returns **401**
  without `Authorization: Bearer <token>`, **200**/upgrade with the correct
  token.
- The token is compared in **constant time** (no timing oracle).
- The **two streaming surfaces** that cannot send an `Authorization` header —
  the SSE `EventSource` (`/api/events`) and the WebSocket terminal
  (`/ws/session/{id}`) — additionally accept the token via `?token=…` query
  string. The fallback is scoped to exactly those two paths; every JSON route
  still requires the header, so tokens never leak via logs/history/`Referer` on
  ordinary requests. The frontend strips the token from the URL after
  connecting.

The static frontend shell (`/`, `/static/*`) and the PWA surface
(`/manifest.webmanifest`, `/sw.js`) are served **without** auth — they carry no
secrets and must load before the page can prompt for a token. Only data and the
terminal are gated.

### The terminal write surface

The live terminal at `/ws/session/{id}` is the only route that can *change*
fleet state (it forwards keystrokes into a session's tmux pane). It is gated
twice:

1. **Auth** — the bearer middleware runs before the WebSocket upgrade. An
   unauthenticated client never attaches.
2. **Posture** — in `--read-only` mode the upgrade is refused outright with
   `403 READ_ONLY`. There is no "connect then reject input" half-state; the
   write surface simply does not exist.

The pane is attached via a real `tmux attach-session` in a pseudo-terminal, so
it is fully interactive (TUIs, colour, cursor, resize). Raw PTY bytes stream to
the browser as binary WebSocket frames and are rendered by an embedded
[xterm.js](https://xtermjs.org/); the browser sends JSON control frames for
input and resize.

---

## Routes

All API responses are JSON. Errors use `{ "error": { "code", "message" } }`.

| Method | Path | Response |
|--------|------|----------|
| GET | `/` , `/static/*` | Embedded vanilla-JS frontend (no auth). |
| GET | `/manifest.webmanifest` | PWA manifest (no auth). |
| GET | `/sw.js` | Service worker, `Service-Worker-Allowed: /` (no auth). |
| GET | `/healthz` | `{ ok, readOnly, tokenRequired, version }` |
| GET | `/api/snapshot` | `{ sessions, needs, cost }` — the full dashboard payload. |
| GET | `/api/sessions` | Live session list (proxies `ainb --format json list`). |
| GET | `/api/needs` | Fleet needs (proxies `ainb --format json fleet needs`). |
| GET | `/api/cost` | Cost rollups (proxies `ainb --format json fleet cost`); `null` when absent. |
| GET | `/api/events` | **SSE** stream of `snapshot` events (`event: snapshot`). |
| GET | `/ws/session/{id}` | **WebSocket** live terminal (refused `403` in `--read-only`). |
| GET | `/api/push/config` | `{ enabled, vapidPublicKey, subscriptionCount }`; `503` if push unconfigured. |
| POST | `/api/push/subscribe` | Register a browser `PushSubscription`. |
| POST | `/api/push/unsubscribe` | Drop a subscription by `endpoint`. |
| POST | `/api/push/presence` | Report tab focus (`{ endpoint, focused }`) to suppress pushes you don't need. |

`401 UNAUTHORIZED` — missing/invalid bearer (only when a token is configured).
`403 READ_ONLY` — the WS terminal in `--read-only` mode.
`502 UPSTREAM_FAILED` — an underlying `ainb` command failed.
`503 PUSH_NOT_CONFIGURED` — a `/api/push/*` route when push isn't enabled.

### Terminal wire protocol

After the upgrade:

| Direction | Frame | Meaning |
|-----------|-------|---------|
| S → C | binary | raw PTY bytes → `xterm.write()` |
| S → C | text | `{"type":"status","event":"attached","session":"…"}` / `{"type":"error","code":"…"}` |
| C → S | text | `{"type":"input","data":"ls\n"}` |
| C → S | text | `{"type":"resize","cols":120,"rows":35}` |

Resize is clamped to a minimum 10×3 to keep tmux out of a degenerate pane.

### Web-push delivery

A background task reads the same cached `needs` snapshot the dashboard renders
and sends a push the moment a session **transitions into** an attention state
(`ASK` / `ERR` / `WAIT` / `NEEDS_PERMISSION` — the same `AlertKind`
classification notifyd uses). The first tick is a baseline, so there's no flood
on startup. Pushes are suppressed for any subscription whose tab reports itself
focused (via `/api/push/presence`), and dead endpoints (404/410) are pruned
automatically.

VAPID keys are generated once per install and stored, `0600`, under
`$AINB_HOME/.agents-in-a-box/web/vapid.json`; subscriptions live next to them in
`push_subscriptions.json` (atomic write). The private key never leaves the
server.

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

- Core crate `ainb-web` (axum HTTP + SSE + WebSocket), wired in as the
  `ainb web` subcommand via `cli/registry.rs`.
- The frontend is a single embedded vanilla-JS bundle (`rust-embed`) — **no
  Node build step, no framework, no runtime filesystem dependency**. xterm.js
  is vendored and embedded the same way (no runtime CDN).
- The terminal uses `portable-pty` to run `tmux attach-session` and streams raw
  bytes to xterm.js — the browser is the terminal emulator, so no server-side
  vt100 re-parse.
- Web-push uses the `web-push` crate (`hyper-client`, no libcurl); the VAPID
  keypair is generated with `p256`.
- A long-lived HTTP server fits a core crate better than the sandboxed v2
  plugin runtime, hence a crate (not a plugin).

### Design notes & honest limitations

- **Shared pane.** The terminal attaches to the live tmux session, so the
  browser and any native `tmux`/TUI client share one pane. Resize is advisory
  (SIGWINCH to the attach client); tmux arbitrates window size across clients.
- **One sender, native-tls.** The `web-push` crate has no rustls path today, so
  the `hyper-client` feature uses the platform TLS (Security.framework on
  macOS, OpenSSL on Linux). This is isolated to the push send path.
- **Push targets attention transitions, not every hook.** Delivery is driven
  off the polled `needs` surface (same cadence as the dashboard), not a direct
  notifyd socket subscription — simple, and it reuses the existing cache. A
  future event-driven upgrade could hook notifyd directly for lower latency.
