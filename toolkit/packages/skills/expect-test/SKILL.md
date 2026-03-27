---
name: expect-test
description: AI-powered browser testing using expect-cli. Auto-detects dev server, reads git diff, generates and executes browser tests via Playwright. Use when you need to verify UI changes in a real browser, test user flows, or validate fixes visually. Trigger on "test this in the browser", "verify the UI", "run expect", "browser test", or after completing UI fixes.
user-invocable: true
---

# Expect Test — AI Browser Testing

Run AI-generated browser tests against your dev server using [expect-cli](https://github.com/millionco/expect). No test files to write — the AI reads your git diff, generates a test plan, and executes it in a real Playwright-driven browser.

## When to Use

- After making UI changes — verify they work visually
- After fixing bugs — confirm the fix in a real browser
- To test user flows (login, navigation, forms)
- As part of `/validate` to add browser verification
- When you need visual confirmation, not just build passes

## Prerequisites

Requires Claude Code (already running if you're reading this).

## Setup & Installation

```bash
# Check if expect-cli is installed
if ! command -v expect-cli &>/dev/null; then
  echo "Installing expect-cli..."
  npm install -g expect-cli@latest
fi

# Verify
expect-cli --version
```

## Usage

### Auto-detect changes and test

```bash
# Test unstaged changes (default) — auto-detects dev server
expect-cli

# Test with a specific instruction
expect-cli -m "test the login flow works correctly"

# Test a whole branch diff
expect-cli -t branch

# Skip plan review (CI mode)
expect-cli -y

# Show browser window during test
expect-cli --headed

# Verbose logging
expect-cli --verbose
```

### Targeted testing

```bash
# Test specific feature
expect-cli -m "verify the Future/Past toggle on events calendar switches views correctly"

# Test a fix
expect-cli -m "confirm the modal has a solid dark background, not transparent"

# Test a flow
expect-cli -m "sign in as coach, navigate to perform, check events show only future events"
```

## Running in tmux (for agents)

**Always run in tmux** — expect-cli has an interactive TUI that blocks the terminal.

```bash
SESSION="expect-$(date +%s)"
tmux new-session -d -s "$SESSION"

# Run with -y to skip interactive review
tmux send-keys -t "$SESSION" "expect-cli -m 'test description here' --headed -y --verbose 2>&1 | tee expect-results.log" C-m

# Monitor progress
sleep 30
tmux capture-pane -t "$SESSION" -p | tail -20

# Check results
cat expect-results.log | grep -E "✔|✘|PASS|FAIL"
```

## CLI Reference

| Flag | Description |
|------|-------------|
| `-m, --message <text>` | Natural language test instruction |
| `-f, --flow <slug>` | Reuse a previously saved test flow |
| `-y, --yes` | Skip plan review, run immediately |
| `-a, --agent <provider>` | AI provider: `claude` or `codex` (default: claude) |
| `-t, --target <target>` | What to test: `unstaged`, `branch`, or `changes` |
| `--headed` | Show visible browser window |
| `--verbose` | Enable verbose logging |
| `--replay-host <url>` | Override replay viewer host |

| Subcommand | Description |
|------------|-------------|
| `expect-cli init` | First-time global install + agent skill setup |
| `expect-cli audit` | Run lint/type/format checks and suggest browser tests |

## How It Works

1. **Auto-detects dev server** — scans localhost ports via `lsof`, recognizes Vite/Next/React/Angular
2. **Reads git diff** — scopes test plan to what actually changed
3. **AI generates test plan** — step-by-step browser actions + assertions
4. **Playwright executes** — real browser, real clicks, real screenshots
5. **Reports pass/fail** — with session recording for replay

## Replay Recordings & Evidence

Expect-cli automatically saves session recordings to disk:

```bash
# Default location
ls /tmp/expect-replays/

# Override with env var
EXPECT_REPLAY_OUTPUT_PATH=/path/to/dir expect-cli -m "test X" -y
```

**Artifacts saved per session:**

| File | Format | Usage |
|------|--------|-------|
| `*.html` | HTML replay viewer | Open in browser to replay the full test session |
| `*.ndjson` | rrweb event recording | Raw event data for programmatic analysis |
| `*.webm` | Playwright video | Actual browser screen recording |
| `*-steps.json` | Step metadata | Pass/fail per step with timestamps |

**To collect evidence after a test run:**
```bash
# Find latest replay
LATEST=$(ls -t /tmp/expect-replays/*.html 2>/dev/null | head -1)
echo "Replay: $LATEST"

# Open replay in browser
open "$LATEST"

# Copy video for PR evidence
cp /tmp/expect-replays/*.webm ./test-evidence/
```

**Note:** Screenshots are NOT saved to disk (base64 in AI context only). Use the `.webm` video or HTML replay for visual evidence.

## Integration with /validate

When `/validate` is invoked, this skill adds browser verification as Step 2.5:

```
Step 2.5: Browser Verification (via expect-test)
- Auto-detect running dev server
- Generate browser tests from the implementation plan
- Execute tests and capture screenshots
- Report visual verification results alongside code validation
```

## Result Interpretation

```
✔ = Test passed
✘ = Test failed (with description of what went wrong)
```

Results include:
- Pass/fail status per test step
- Screenshots at each step
- Session recording URL for replay
- Specific failure descriptions (e.g., "element not found", "wrong color value")

## Limitations

- Early stage tool (v0.0.x) — may crash on complex scenarios
- macOS/Linux only (uses `lsof` for port detection)
- Requires a running dev server on localhost
- No custom Playwright config — browser is fully managed
- Interactive TUI — must use `-y` flag or run in tmux for automation

## Environment

| Variable | Description |
|----------|-------------|
| `NO_TELEMETRY=1` | Disable analytics/telemetry |
