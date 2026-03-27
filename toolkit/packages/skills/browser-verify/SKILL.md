---
name: browser-verify
description: Combined AI browser testing + visual inspection. Runs expect-cli for automated test generation/execution, then debug-bridge for screenshots and DOM inspection. Use when you need to verify UI changes with both automated tests AND visual evidence. Trigger on "verify in browser", "browser verify", "visual test", "test and screenshot", or after completing UI fixes that need proof.
user-invocable: true
---

# Browser Verify — Test + Screenshot Combo

Combines **expect-cli** (AI-generated browser tests) with **debug-bridge** (screenshots + DOM inspection) for full verification with visual evidence.

## When to Use

- After fixing UI bugs — need both test pass AND screenshot proof
- Before raising a PR — capture evidence for PR comments
- When `/validate` needs visual verification
- Anytime you hear "prove it works"

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    /browser-verify                           │
│                                                             │
│  Phase 1: expect-cli (Playwright browser)                   │
│  ├─ Auto-detect dev server                                  │
│  ├─ AI generates test plan from git diff                    │
│  ├─ Execute tests → pass/fail results                       │
│  └─ Session recording (ephemeral replay)                    │
│                                                             │
│  Phase 2: debug-bridge (App's own browser via WebSocket)    │
│  ├─ Open app with ?session=X&port=Y                         │
│  ├─ Navigate to affected pages                              │
│  ├─ Take before/after screenshots → saved as PNG            │
│  ├─ Inspect DOM state, console errors, network              │
│  └─ Upload screenshots via /imgbb-upload for PR comments    │
└─────────────────────────────────────────────────────────────┘
```

## Quick Start

```bash
/browser-verify                          # Full: expect-test + debug-bridge screenshots
/browser-verify --test-only              # Just expect-cli tests
/browser-verify --screenshot-only        # Just debug-bridge screenshots
/browser-verify -m "test the login flow" # Specific test instruction
```

## Phase 1: Automated Testing (expect-cli)

### Prerequisites

```bash
# Auto-install if missing
if ! command -v expect-cli &>/dev/null; then
  npm install -g expect-cli@latest
fi
```

### Run Tests

```bash
SESSION_NAME="verify-$(date +%s)"
tmux new-session -d -s "$SESSION_NAME"

# Run expect-cli with auto-detect
tmux send-keys -t "$SESSION_NAME" "expect-cli -t changes -y --headed --verbose 2>&1 | tee verify-expect.log" C-m

# Monitor (check every 30s)
sleep 30
tmux capture-pane -t "$SESSION_NAME" -p | tail -20

# Collect results
RESULTS=$(cat verify-expect.log 2>/dev/null | grep -E '✔|✘' | head -20)
```

### Result Format

```
✔ = Test passed
✘ = Test failed (description of failure)
```

## Phase 2: Visual Evidence (debug-bridge)

### Start Debug Bridge Server

```bash
DB_PORT=$(shuf -i 4000-4999 -n 1)
DB_SESSION="verify-$(date +%s)"
tmux new-session -d -s "$DB_SESSION"
tmux send-keys -t "$DB_SESSION" "npx debug-bridge-cli connect --session $DB_SESSION --port $DB_PORT 2>&1 | tee debug-bridge-$DB_PORT.log" C-m
sleep 3
```

### Open App with Debug Bridge

```bash
# Get dev server port
DEV_PORT=$(lsof -iTCP -sTCP:LISTEN -nP 2>/dev/null | grep -E 'node|vite' | awk '{print $9}' | grep -oE '[0-9]+$' | head -1)

# Open in browser with debug params
open "http://localhost:${DEV_PORT}/login?session=${DB_SESSION}&port=${DB_PORT}"
sleep 5
```

### Take Screenshots

```bash
# Navigate and screenshot
tmux send-keys -t "$DB_SESSION" "go /perform" C-m
sleep 3
tmux send-keys -t "$DB_SESSION" "screenshot" C-m
sleep 2

# Read screenshot path from output
SCREENSHOT=$(tmux capture-pane -t "$DB_SESSION" -p | grep "screenshot.*saved" | tail -1 | grep -oE 'screenshot-[0-9]+\.png')
```

### Inspect DOM

```bash
# List interactive elements
tmux send-keys -t "$DB_SESSION" "ui" C-m
sleep 2
tmux capture-pane -t "$DB_SESSION" -p | tail -20

# Run JavaScript in browser
tmux send-keys -t "$DB_SESSION" "eval console.log('CHECK:', document.querySelector('.some-element')?.textContent)" C-m
sleep 1
tmux capture-pane -t "$DB_SESSION" -p | grep "CHECK:"

# Check for console errors
tmux send-keys -t "$DB_SESSION" "eval console.log('ERRORS:', window.__errors?.length || 0)" C-m
```

### Capture Network Activity

Debug-bridge auto-captures fetch/XHR when enabled:
```
🌐 [POST] /auth/v1/token → ✓ 200 OK (45ms)
🌐 [GET] /rest/v1/events → ✓ 200 OK (120ms)
🌐 [GET] /rest/v1/profiles → ✗ 401 Unauthorized (12ms)
```

## Combined Workflow Example

```bash
# === PHASE 1: Automated Tests ===
SESSION="verify-$(date +%s)"
tmux new-session -d -s "expect-$SESSION"
tmux send-keys -t "expect-$SESSION" "expect-cli -m 'verify the events calendar Future/Past toggle and event preview sheet' -y --headed --verbose 2>&1 | tee verify-expect.log" C-m

# Wait for expect-cli to complete
sleep 180
EXPECT_RESULTS=$(cat verify-expect.log | grep -E '✔|✘')

# === PHASE 2: Screenshots ===
DB_PORT=$(shuf -i 4000-4999 -n 1)
tmux new-session -d -s "debug-$SESSION"
tmux send-keys -t "debug-$SESSION" "npx debug-bridge-cli connect --session $SESSION --port $DB_PORT" C-m
sleep 3

DEV_PORT=$(cat .tmux-dev-session.json 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin)['dev_port'])" 2>/dev/null || echo "7083")
open "http://localhost:${DEV_PORT}/perform?session=${SESSION}&port=${DB_PORT}"
sleep 5

# Take evidence screenshots
tmux send-keys -t "debug-$SESSION" "screenshot" C-m
sleep 2

# === PHASE 3: Report ===
echo "## Browser Verification Report"
echo ""
echo "### Automated Tests (expect-cli)"
echo "$EXPECT_RESULTS"
echo ""
echo "### Screenshots"
echo "Saved: $(ls screenshot-*.png 2>/dev/null | tail -3)"

# Cleanup
tmux kill-session -t "expect-$SESSION" 2>/dev/null
tmux kill-session -t "debug-$SESSION" 2>/dev/null
```

## Cleanup

```bash
# Kill specific sessions
tmux kill-session -t "expect-$SESSION" 2>/dev/null
tmux kill-session -t "debug-$SESSION" 2>/dev/null

# Clean screenshot files
rm -f screenshot-*.png verify-expect.log debug-bridge-*.log
```

## Integration with /validate

When `/validate` invokes browser verification, it can use `/browser-verify` as the implementation:

```
/validate
  └── Step 2.5: Browser Verification
       └── /browser-verify
            ├── expect-cli (automated tests)
            └── debug-bridge (screenshots + evidence)
```

## Limitations

- **Two separate browser instances** — expect-cli and debug-bridge don't share state
- **Auth must be done separately** in each — expect-cli uses its own Playwright session, debug-bridge uses the opened browser tab
- **expect-cli is v0.0.x** — may crash on complex scenarios
- **debug-bridge needs SDK in the app** — already installed in SHOT
- **Sequential, not parallel** — run expect-cli first, then debug-bridge for evidence
