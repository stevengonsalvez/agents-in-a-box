---
name: ainb-fleet:daemon
description: |
  Long-running watcher that scans every claude session every 5s and
  auto-sends `continue` to any session whose recent tmux pane buffer
  matches a known API-error regex (rate_limited, overloaded_error,
  internal_server_error, request_timeout, socket_hang_up, fetch_failed,
  ECONNRESET). Use this when you want unattended recovery from
  transient API failures across the fleet.
version: "0.1.0"
user-invocable: true
triggers:
  - ainb-fleet:daemon
  - fleet daemon
  - auto-continue
  - fleet watcher
  - recover stuck sessions
allowed-tools:
  - Bash
---

# ainb fleet:daemon

> **Superseded by [`/ainb-fleet:atc`](../atc/SKILL.md) for managed fleets.** ATC
> absorbs this daemon's job — its ERR playbook does the same auto-`continue`, but
> **with a per-session retry cap + escalate-on-exhaustion** that this daemon
> lacks (see the sharp edge below). Prefer `ainb fleet atc setup <name>` for
> unattended supervision. The daemon stays for unmanaged one-off recovery; do
> **not** run it against sessions an ATC instance already manages — they race.

Background watcher. Reads each session's tmux pane (`capture-pane`) and
auto-continues sessions hitting API errors via `tmux send-keys`. It does
**not** register as a peer — it is purely tmux-driven (broker health is
checked only to print an informational line).

## Run

```bash
ainb fleet daemon                    # quiet
ainb fleet daemon --verbose          # log every detection + send
```

For real background use:

```bash
nohup ainb fleet daemon --verbose > ~/.ainb-fleet.log 2>&1 &
```

## What it does each tick (every 5s)

1. Re-discover all sessions (ainb + peers + jobs, merged + deduped).
2. For each tmux-bearing session: `tmux capture-pane -p -S -80` (last 80 lines).
3. Run the API-error regex set over the buffer.
4. If a match fires AND the (session_id, pattern, snippet-tail) dedupe
   key hasn't been seen yet → send `continue` to that session via the
   standard route (tmux-first by default — see `AINB_FLEET_TRANSPORT`).

## Detected error patterns

| name | regex |
|---|---|
| `rate_limited` | `\brate[_ ]limited\b` |
| `overloaded` | `\boverloaded_error\b \| \bModel is overloaded\b` |
| `internal_server_error` | `\binternal_server_error\b` |
| `request_timeout` | `\brequest timed out\b` |
| `socket_hang_up` | `\bsocket hang up\b` |
| `fetch_failed` | `\bAPI Error\b \| \bfetch failed\b` |
| `connection_reset` | `\bECONNRESET\b \| \bconnection reset\b` |

Case-insensitive. Word-boundaried to avoid false positives.

## Dedupe key

```
key = (session_id, pattern_name, last_40_chars_of_match_context)
```

Within a single daemon run, the same `key` only fires `continue` once.
This prevents spam when the error text scrolls up but is still in the
buffer.

## ⚠️ Sharp edge — no retry cap in v0.1

If a session is permanently broken (wrong credentials, model deprecated,
loop bug), the daemon will keep firing `continue` forever as new errors
match new dedupe keys. Watch the daemon log; kill it (`Ctrl-C` or
`kill <pid>`) when you see runaway repetition.

Roadmap: per-session retry cap with exponential backoff.

## Stop the daemon

```bash
# foreground: Ctrl-C
# background:
pkill -INT -f "ainb fleet daemon"

# or kill by exact PID if you know it
kill <pid>
```

The daemon holds no broker registration, so there is no stale peer entry to
clean up on exit — `Ctrl-C` / `kill` is enough.

## Observe what the daemon is doing

On start the daemon prints whether the broker is reachable (informational
only — writes go via tmux regardless):

```
[fleet/daemon] broker healthy at 127.0.0.1:7899
   # or, when the broker is down:
[fleet/daemon] broker not reachable — operating in tmux-only mode
```

With `--verbose`, every detection also prints:

```
[fleet/daemon] auto-continue -> <tmux_session> (<pattern>)
```

Tail the nohup log to watch in real time:

```bash
tail -F ~/.ainb-fleet.log
```

## When NOT to run the daemon

- During hand-debugging — the daemon will send `continue` while you're
  reading the error, racing your investigation.
- On a fleet doing batch work that explicitly throws errors as control
  flow — the daemon will misinterpret intentional errors.
- On a session with no live tmux pane (bg job, dead tmux) — there is
  nothing to send-keys to; under the default `tmux-first` it can only
  reach such a session if it still has a healthy broker peer.
