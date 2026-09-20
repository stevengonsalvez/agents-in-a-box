# Proof run 30: v2 `11d52b7d8`

Post-merge confirmation for v2 at `11d52b7d8`, the #1299 merge that added the `d3p-inbox` scenario. That scenario had no recorded run anywhere before this one.

## Result

```
proof: 22 of 23 nodes pass; failed: issue-983-redaction
```

`scripts/proof/run.sh --build`, all 23 nodes, 00:11:12 to 00:24:32 UTC (13 min 20 s including the build). Binary `ainb 1.28.2 (11d52b7d8, 2026-09-20, source)`. Nothing was skipped: this box has webkit, so the desktop legs ran.

Before starting, the box was checked for other work: no listener on the WebDriver port 4445, no `wdio` process, no other proof world.

## d3p-inbox: PASS, on its first run anywhere

Nine checks, no failures:

```
ok: the window subscribes to the inbox section from its first batch
ok: a separate process created an issue through the daemon
ok: the daemon aggregated the issue into the local human's inbox within 60 s
ok: the window's inbox section carries the row within 60 s, in its own telemetry
ok: the window counts it unread
ok: the TUI's inbox screen draws the issue's row within 30 s
ok: the daemon recorded the entry read
ok: the window's inbox section shows nothing unread within 60 s
ok: the TUI's inbox screen shows 0 unread within 30 s
```

It also recorded what it drove: issue `01M2Y360NW304W9RNST1VVGBSG`, the first applied batch carrying the `inbox` section, `1 rows, 1 unread` before the sweep, and the third process's sweep reply `{"marked":1,"unread":0,"mutation":{"outcome":"created","status":"accepted"}}`.

## The one red: issue-983-redaction

First failing check, with the value it read:

```
FAILED: the web cost panel is populated from the recorded turn
```

The node's own capture shows `.cost` is `null` in the web snapshot, while `ainb fleet cost` in the same world had already reported the recorded turn (`call_count: 1`, one session with a cwd). Every redaction check in the node passed; only this populated-ness check failed.

**It is the scenario, not the product.** What was measured:

| probe | result |
|---|---|
| the node on this binary, during the full 23-node run | FAIL |
| the node alone, twice, straight after the run | FAIL |
| the node alone on `cec002468` (the run 29 binary), three times | PASS, `cost_sessions: 1` each time |
| the node alone on this binary again, later, twice | **PASS**, `cost_sessions: 1` |
| `ainb fleet cost --format json`, both binaries, same fixture | identical output, 1530 bytes, `call_count: 1`, exit 0, 0.3 s |
| `ainb web` on each binary against the same fixture | both populate `.cost` within 12 s |

So both binaries do the same thing, and the node passes or fails on the same binary depending on when it runs. The check waits 60 s for the web's cost panel. The web fetches cost on its own task, abandons a fetch after 30 s and doubles the bound up to 240 s (`routes.rs`, `COST_FETCH_TIMEOUT`), and #1055 measured `ainb fleet cost` taking 120 s under contention. A busy box can therefore push the first landed cost past the node's 60 s wait, which is what a full run on a shared box provides.

The fix belongs in the scenario: wait on the cost panel for longer than the web's own backoff can take, or record the wait it lost rather than failing. The proof lane does not change product code, and this needs no product change.

Run 29 on `cec002468` passed the same check, which is why this shows as new; it is load, not a regression.

## Per node

Expected and observed, line by line, are in `summary.md` beside this file.

| node | result | assertions |
|---|---|---|
| `phase1-keymap` | PASS | 14 ok, 0 failed |
| `s-a-config-and-headroom` | PASS | 14 ok, 0 failed |
| `s-b-connections` | PASS | 5 ok, 0 failed |
| `s-c-answered` | PASS | 10 ok, 0 failed |
| `phase2-sections` | PASS | 5 ok, 0 failed |
| `phase3-uistate` | PASS | 8 ok, 0 failed |
| `s-d-surfaces` | PASS | 7 ok, 0 failed |
| `p1-app-extraction` | PASS | 6 ok, 0 failed |
| `p2-effects` | PASS | 8 ok, 0 failed |
| `p3-hangar-host` | PASS | 8 ok, 0 failed |
| `p4-review-screens` | PASS | 9 ok, 0 failed |
| `w0-wire` | PASS | 6 ok, 0 failed |
| `t0-daemon` | PASS | 5 ok, 0 failed |
| `issue-963-presence` | PASS | 5 ok, 0 failed |
| `issue-962-fleet-panel` | PASS | 6 ok, 0 failed |
| `t0-section` | PASS | 5 ok, 0 failed |
| `issue-983-redaction` | FAIL | 13 ok, 1 failed |
| `issue-1094-own-session` | PASS | 10 ok, 0 failed |
| `issue-1173-select-tab` | PASS | 4 ok, 0 failed |
| `d1-shell` | PASS | 7 ok, 0 failed |
| `d2-board` | PASS | 7 ok, 0 failed |
| `d3-review` | PASS | 3 ok, 0 failed |
| `d3p-inbox` | PASS | 9 ok, 0 failed |

## Leftovers

No process from run 30's worlds was left running and no world of this run remained. Captures are on claude-gcp in `/tmp/proof-out-run30`.
