# Proof run 28: v2 `1785167a9`

Integration evidence for v2 at `1785167a9`. CI no longer gates merges, so this run is the main check that the merged pieces work together.

Since run 27 (`9342a492a`), v2 gained #1215, #1217 (D3a), #1226, #1222 (P6e-1, dark), #1239, #1229, #1238, #1241, #1228, #1225 and #1247.

## Result

| run | what ran | result |
|---|---|---|
| 28a | `scripts/proof/run.sh`, all 21 nodes, box without webkit | **19 of 21 pass, 2 skipped** (`d1-shell`, `d2-board`), none failed, exit 0 |
| 28b | the same 21 nodes after installing webkit and building the desktop shell | **21 of 21 pass**, exit 0 |
| journey 1 | `xvfb-run -a npm test` in `crates/ainb-desktop/e2e` (`journey.e2e.js`, `answer.e2e.js`) | **2 of 2 spec files pass** (3 tests), 9 min 15 s |

No scenario failed, so there is no first failing assertion to report. `d1-shell` and `d2-board` pass in 28b, which is also their second run: both passed first in a `--only d1-shell --only d2-board` run at 17:37 UTC.

## Build

All from a detached checkout of `origin/v2` at `1785167a9` on claude-gcp (Ubuntu 24.04, 4 cores, 23 GB).

1. `cargo build -p ainb -p ainb-hangar-daemon`, then `scripts/build-plugins.sh`. Binary: `ainb 1.28.2 (1785167a9, 2026-09-19, source)`.
2. For 28b: installed the Tauri packages the `desktop.yml` CI job installs (`libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev patchelf`, webkit2gtk 2.52.6). Disk had 72G free before the install and 65G after the desktop build, so no other lane's target was cleaned.
3. `npm ci && npm run build` in `crates/ainb-desktop/ui`, `cargo xtask stage-desktop-sidecar`, then `cargo build --features bundled` in `crates/ainb-desktop`. This is the shell the harness drives.
4. For the journey: `cargo build --features wdio` in `crates/ainb-desktop` (the same binary path, rebuilt after 28b finished), then `npm ci` in `e2e` with the CI's driver-download skips.

## Per node

Expected and observed, line by line, are in `summary-28a-no-webkit.md` and `summary-28b-webkit.md` beside this file, as the harness wrote them. Counts below are from 28b.

| node | 28a | 28b | 28b assertions |
|---|---|---|---|
| `phase1-keymap` | PASS | PASS | 14 ok, 0 failed |
| `s-a-config-and-headroom` | PASS | PASS | 14 ok, 0 failed |
| `s-b-connections` | PASS | PASS | 5 ok, 0 failed |
| `s-c-answered` | PASS | PASS | 10 ok, 0 failed |
| `phase2-sections` | PASS | PASS | 5 ok, 0 failed |
| `phase3-uistate` | PASS | PASS | 8 ok, 0 failed |
| `s-d-surfaces` | PASS | PASS | 7 ok, 0 failed |
| `p1-app-extraction` | PASS | PASS | 6 ok, 0 failed |
| `p2-effects` | PASS | PASS | 8 ok, 0 failed |
| `p3-hangar-host` | PASS | PASS | 8 ok, 0 failed |
| `p4-review-screens` | PASS | PASS | 9 ok, 0 failed |
| `w0-wire` | PASS | PASS | 6 ok, 0 failed |
| `t0-daemon` | PASS | PASS | 5 ok, 0 failed |
| `issue-963-presence` | PASS | PASS | 5 ok, 0 failed |
| `issue-962-fleet-panel` | PASS | PASS | 6 ok, 0 failed |
| `t0-section` | PASS | PASS | 5 ok, 0 failed |
| `issue-983-redaction` | PASS | PASS | 10 ok, 0 failed |
| `issue-1094-own-session` | PASS | PASS | 10 ok, 0 failed |
| `issue-1173-select-tab` | PASS | PASS | 4 ok, 0 failed |
| `d1-shell` | SKIP | PASS | 6 ok, 0 failed |
| `d2-board` | SKIP | PASS | 7 ok, 0 failed |

## The desktop answer journey

In CI, `answer.e2e.js` timed out after #1226, and the log showed `Tauri core.invoke not available`. Locally it **passes**:

```
» specs/journey.e2e.js
the desktop shell
   ✓ opens a seeded session, runs a palette command and sees a new one arrive
   ✓ records what a 50 MiB read costs the window
2 passing (5m 39.1s)

» specs/answer.e2e.js
answering from the window
   ✓ answers a daemon question from the banner and the daemon records the desktop
1 passing (3m 28.2s)

Spec Files:  2 passed, 2 total (100% completed) in 00:09:15
```

The `core.invoke` message **does reproduce locally**: 106 `WARN tauri-service:window: Failed to get window states: Error: Tauri core.invoke not available after 5s timeout` lines in journey 1, starting with the first spec. It is a warning, not the failure:

- The message comes from `@wdio/tauri-service` 1.4.0. Before `getTitle`, `findElement`, `findElements`, `$`, `$$` and `elementClick`, `ensureActiveWindowFocus` runs a script that waits up to 5 s for `window.__wdio_original_core__.invoke`, then throws. The throw is caught and logged as a WARN, and the command goes ahead (`e2e/node_modules/@wdio/tauri-service/dist/esm/index.js`, around lines 3091 and 3196).
- So each of those commands can cost up to 5 s more, but none fails because of it.
- #1226 itself only changed how `answer.e2e.js` reads the answered row (the daemon's RPC instead of sqlite3). The warning already appears in `journey.e2e.js`, which #1226 did not touch.

Hypothesis, not confirmed: the CI timeout is those 5 s delays added to a slower runner, pushing a wait past its limit. The limits are `mochaOpts.timeout` 600 s per test and the 60 s and 30 s `waitUntil`s in the spec. The CI job's own log would confirm it (which wait timed out, and after how long) or rule it out, as would a local failure with the same message. Wiring `window.__wdio_original_core__` so the focus check stops waiting would remove the delay in both places.

The journey log, with stack frames removed and the home directory and host name masked, is `journey-run-1.txt`.

## Leftovers

No process from the proof worlds was left running, and no world directory remained after 28a or 28b. The full captures are on claude-gcp in `/tmp/proof-out-run28a` and `/tmp/proof-out-run28b`.
