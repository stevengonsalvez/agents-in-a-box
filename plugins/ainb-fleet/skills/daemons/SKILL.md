---
name: ainb-fleet:daemons
description: |
  Runtime-health view of the four fleet daemons — phone bridge, notifyd,
  ATC, and the fleet auto-continue daemon — in one table. Each row reports
  state (running / stopped / unknown), pid, uptime, last activity, error
  count, and a HEALTH reason that distinguishes a clean stop from a crash
  (stale heartbeat) or a pid-recycle. Use this when you need to answer "are
  my background daemons actually alive?" — not just installed, but serving.
  Default output: a fixed-width text table. Pass --format json for tooling.
version: "0.1.0"
user-invocable: true
triggers:
  - ainb-fleet:daemons
  - fleet daemons
  - daemon health
  - are my daemons running
  - is the bridge up
  - is notifyd alive
allowed-tools:
  - Bash
---

# ainb fleet:daemons — daemon health view

The runtime-health answer for the fleet's four background daemons. Reads the
on-disk liveness signals each daemon already maintains and renders one typed
row per daemon. The same aggregator (`fleet::daemons::collect`) backs the TUI
Daemons screen, so the CLI and the TUI can never drift.

This is the question `ainb fleet bridge status` could never answer: that verb
reports only **install** state. `daemons` reports whether the process is
actually **running and serving** right now — a crashed-but-installed bridge
shows `stopped` with a stale-heartbeat reason here.

## The four daemons

| daemon | id | how it's probed |
|---|---|---|
| **phone bridge** | `bridge` | heartbeat file (`~/.agents-in-a-box/daemons/bridge.json`) + pid-identity cross-check |
| **notifyd** | `notifyd` | PID-file liveness + bound Unix socket + sqlite DB file present |
| **ATC** | `atc` | most-recently-beating provisioned instance's `heartbeat-state.json` (timer-driven; no resident pid) |
| **fleet daemon** | `fleet-daemon` | heartbeat file + pid-identity cross-check |

Rows are always emitted in this exact order (bridge, notifyd, ATC, fleet
daemon), even when every daemon is stopped — so "everything stopped" is a
legible four-row table, never an empty one.

## Run

```bash
ainb fleet daemons                       # fixed-width text table (default)
ainb --format json fleet daemons         # typed rows for scripting
```

`--format` is a **global** flag — it goes before `fleet`. There are no other
flags or subcommands; `daemons` is a pure read.

## Text columns

```
DAEMON         STATE      PID      UPTIME     LAST ACTIVITY  ERRORS  HEALTH
```

- **STATE** — `● running` · `○ stopped` · `? unknown`.
- **PID** — the OS pid when known (heartbeat / PID file); `-` for ATC (timer-driven).
- **UPTIME** — compact since-start (`5s`, `12m`, `3h`, `2d`); `-` when unknown.
- **LAST ACTIVITY** — relative time of the last real activity (`5s ago`, `just now`).
- **ERRORS** — error count observed this run.
- **HEALTH** — for a running+connected daemon, the connection channel + reason
  (e.g. `Telegram (@bot) — running + connected`); otherwise the reason string,
  which is the load-bearing field for telling a clean stop from a crash.

## States and what they mean (JSON `state` + `reason`)

| state | reason examples | meaning |
|---|---|---|
| `running` | `running + connected`, `running (connecting…)`, `running but socket not bound yet` | process alive and heartbeating / serving |
| `stopped` | `no heartbeat — not running this session`, `stale heartbeat — pid 4242 not alive (crashed)`, `stale heartbeat — pid 4242 recycled (different process, crashed)`, `stale heartbeat — last beat 130s ago (wedged?)`, `no ATC instance provisioned`, `N instance(s) provisioned, all heartbeat-disabled` | not serving — the reason says clean-stop vs crash vs never-configured |
| `unknown` | (probe error) | the probe itself failed to read a signal — surfaced rather than guessed |

## JSON fields (per row)

| field | meaning |
|---|---|
| `kind` | `bridge` \| `notifyd` \| `atc` \| `fleet-daemon` |
| `state` | `running` \| `stopped` \| `unknown` |
| `pid` | OS pid when known (omitted otherwise) |
| `uptime_ms` | ms since start when known |
| `connected` | daemon-specific connection health (Telegram online / socket+db reachable / heartbeat alive) |
| `channel` | connection label, e.g. `"Telegram (@bot)"`, `"unix socket + sqlite"`, `"tower (every 15m)"` |
| `last_activity_at` | epoch ms of last real activity when known |
| `error_count` | errors observed this run |
| `last_error` | most recent error string when known |
| `reason` | short human explanation of the state (clean-stop vs crash vs never-configured) |

## The pid-identity recycle guard

A daemon's heartbeat records both its `pid` and its process `started_at`.
Bare liveness (`kill(pid, 0)`) is **not** trusted: when a daemon dies and the
OS hands its pid to a different, unrelated process, that pid is *alive* but is
no longer our daemon. The probe cross-checks process **identity** — the
heartbeat's `started_at` against the live process's real OS start time — so a
**recycled** pid classifies as `stopped` (reason: `pid … recycled (different
process, crashed)`), never a false `running`. A dead pid is caught immediately
regardless of how recent the heartbeat looks; a live-but-wedged pid whose beat
went quiet past the 90s staleness window reports `stopped` with a `wedged?`
reason.

For notifyd specifically, "connected" requires an **actually-bound** Unix
socket (a non-blocking `connect()` that a listener accepts) — not merely a
`notify.sock` file on disk, which a crashed daemon can leave behind. The DB is
reported as "file present", honestly *not* "reachable", since `exists()` does
not prove the sqlite file opens and accepts writes.

## Composition patterns

```bash
# Any daemon not running?
ainb --format json fleet daemons | jq '[.[] | select(.state != "running")]'

# Is the bridge connected to a chat channel right now?
ainb --format json fleet daemons | jq '.[] | select(.kind=="bridge") | {state, connected, channel, reason}'

# Surface any crash reasons in one line each
ainb --format json fleet daemons | jq -r '.[] | select(.reason | test("crash|stale|wedged")) | "\(.kind): \(.reason)"'
```

## Caveats

- **Read-only.** `daemons` never starts, stops, or restarts anything — it only
  reports. To manage a daemon, use its own verb (`ainb fleet bridge
  install/uninstall`, `ainb fleet atc setup/teardown`, `ainb fleet daemon`).
- **notifyd ignores `$AINB_HOME`.** It always uses `~/.agents-in-a-box`; the
  probe mirrors that so it reads the same files notifyd writes (a set
  `$AINB_HOME` is only honoured for an isolated test run).
- **ATC surfaces as one row.** v1 reports the most-recently-beating provisioned
  instance; the instance name is in the `channel` label. Heartbeat-disabled
  instances are never counted as running, even with a recent leftover beat.
