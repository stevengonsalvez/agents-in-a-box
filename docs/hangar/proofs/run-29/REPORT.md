# Proof run 29: v2 `cec002468`

Integration evidence for v2 at `cec002468`, the tip after the four list-identity merges (#1274, #1278, #1281, #1283, #1284) and #1286 (`fix/banner-grace-750`).

## Result

| what ran | result |
|---|---|
| `scripts/proof/run.sh`, all 22 nodes, webkit present | **22 of 22 pass**, none skipped, none failed, exit 0 |
| desktop e2e, `xvfb-run -a npm test`, 3 specs | **2 passed, 1 failed** (`review.e2e.js`), exit 1 |

The harness is green, desktop legs included: `d1-shell`, `d2-board` and the new `d3-review` all pass. The one red is the review journey, and it is not what was expected of it (see below).

## Build

Detached checkout of `origin/v2` at `cec002468` on claude-gcp.

1. `cargo build -p ainb -p ainb-hangar-daemon`, `scripts/build-plugins.sh`. Binary: `ainb 1.28.2 (cec002468, 2026-09-19, source)`.
2. `npm run build` in `crates/ainb-desktop/ui`, `cargo xtask stage-desktop-sidecar`, then `cargo build --features bundled`: the shell the harness drives.
3. Then `cargo build --features wdio` and `npm ci` in `e2e`, for the journey specs. The two desktop builds write the same binary, so they run one after the other, never at once.

Disk: 18G free before the desktop build, under the 30G bar, so `cargo clean` ran in `status-t0/ainb-tui` (PR #1271 merged, tree clean, no build running there, target untouched since 2026-09-18). That freed 33G, leaving 50G.

## Per node

Expected and observed, line by line, are in `summary.md` beside this file, as the harness wrote it.

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
| `issue-983-redaction` | PASS | 14 ok, 0 failed |
| `issue-1094-own-session` | PASS | 10 ok, 0 failed |
| `issue-1173-select-tab` | PASS | 4 ok, 0 failed |
| `d1-shell` | PASS | 7 ok, 0 failed |
| `d2-board` | PASS | 7 ok, 0 failed |
| `d3-review` | PASS | 3 ok, 0 failed |

Run time: 22:31:59 to 22:41:13 UTC, 9 min 14 s.

## The three e2e specs

```
journey.e2e.js  ✓ opens a seeded session, runs a palette command and sees a new one arrive
journey.e2e.js  ✓ records what a 50 MiB read costs the window          2 passing (6m 20.7s)
answer.e2e.js   ✓ answers a daemon question from the banner            1 passing (4m 33.7s)
review.e2e.js   ✖ draws the reducer's diff, takes a selection          1 failing (8m 26.2s)

Spec Files:  2 passed, 1 failed, 3 total (100% completed) in 00:19:26
```

- **`journey.e2e.js` passes**, palette step included.
- **`answer.e2e.js` passes.** On the previous tip (`fb72ca122` plus the four list branches) it failed with `the banner never showed the question`; #1286 fixed that.
- **`review.e2e.js` fails, but not on the windowing assertion.** The first failure is:

```
ReferenceError in "reviewing from the window.draws the reducer's diff, takes a selection, and records what it costs"
ReferenceError: selectedNode is not defined
    at Context.<anonymous> (.../e2e/specs/review.e2e.js:185:22)
```

`selectedNode()` is called at `review.e2e.js:185` and `:189` and is defined nowhere in the file. The throw is in the settings leg, which runs BEFORE the row-window assertions #1221 asks for, so the run never reaches them: this says nothing yet about whether the review tab windows rows in a real window. The spec has to define that helper (or drop the two calls) before the windowing question can be answered here.

The review windowing gap lane F is working on is therefore **not measured by this run**. The harness node that touches the review surface, `d3-review`, passes, but it only asserts that a separate process's diff reaches the window through the `git_view` section and that the window keeps applying batches; it draws no conclusion about rows.

## Leftovers

No process from run 29's worlds was left running and no world of this run remained. One `/tmp/ainb-proof-run.*` directory is present and live: it belongs to lane `p6d-fix`'s own concurrent proof run, not to this one.

Captures are on claude-gcp in `/tmp/proof-out-run29`, and the spec log is `e2e-specs.txt` beside this file, with stack frames dropped and the home directory and host name masked.
