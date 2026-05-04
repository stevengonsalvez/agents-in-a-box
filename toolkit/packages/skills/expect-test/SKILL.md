---
name: expect-test
description: AI-powered browser testing using expect-cli. Auto-detects dev server, reads git diff, generates and executes browser tests via Playwright. Use when you need to verify UI changes in a real browser, test user flows, or validate fixes visually. Trigger on "test this in the browser", "verify the UI", "run expect", "browser test", or after completing UI fixes.
argument-hint: "[-m message] [--headed] [-y]"
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

**Always run in tmux** — expect-cli `tui` has an interactive TUI that blocks the terminal.

### CLI syntax changed — use the `tui` subcommand

Recent expect-cli versions (≥0.1.2) moved the test runner under the `tui` subcommand.
`expect-cli -m "..."` without `tui` will print `error: unknown option '-m'`.

```bash
# ✅ CORRECT — tui subcommand
expect-cli tui -m "short description" --browser-mode headed -y

# ❌ WRONG on v0.1.2+
expect-cli -m "short description" --headed -y
```

### TUI is deprecated — prefer the MCP server going forward

On first run, expect-cli prints a banner:
> ⚠ The TUI is deprecated. Use /expect from your coding agent instead. Run `npx expect-cli@latest init` to set up.

For new projects, install the MCP server (`expect-cli init`) so Claude Code can call
the atomic tools (`expect open`, `expect screenshot`, `expect network_requests`, etc.)
directly. The `tui` command still works but will be removed in a future release.

### Safe pattern for long missions (the ONLY way that's robust)

Put the mission in a file via heredoc. Inline `-m "..."` strings with apostrophes,
embedded double-quotes, or newlines break the shell chain: `tmux send-keys` →
`bash -c` → expect-cli argv parser. Symptom: `error: too many arguments for 'tui'. Expected 0 arguments but got 4`.

```bash
STAMP=$(date +%s)
SESSION="expect-${STAMP}"
LOG="/tmp/expect-${STAMP}.log"
MISSION_FILE="/tmp/mission-${STAMP}.txt"
URL="http://localhost:5173"   # or a deployed preview

# 1) Write the mission to a file — heredoc preserves apostrophes/quotes/newlines.
cat > "$MISSION_FILE" <<'EOF'
Navigate to the page. Wait for network idle.
Verify the SHOTs tab is selected and 5+ article cards are visible.
Apostrophes ("don't"), quotes, and newlines all survive when the mission is a file.
EOF

# 2) Launch tmux + expect-cli tui — reference the file with $(cat …), NOT inline.
tmux new-session -d -s "$SESSION" \
  "EXPECT_REPLAY_OUTPUT_PATH=/tmp/expect-replays \
   expect-cli tui -u '$URL' -m \"\$(cat $MISSION_FILE)\" --browser-mode headed -y --verbose \
   2>&1 | tee $LOG"

# 3) First-run prompt: expect-cli asks "Install the expect skill for your coding agents? (Y/n)".
#    The `-y` flag does NOT cover this prompt. Decline it:
sleep 5
tmux send-keys -t "$SESSION" "n" Enter

# 4) Monitor. DO NOT attach — it blocks the agent.
tmux capture-pane -t "$SESSION" -p | tail -30
```

### Why the naive inline approach fails

```bash
# ❌ BROKEN — the double-quote nesting inside bash -c collapses
tmux send-keys -t "$S" "bash -c 'expect-cli tui -m \"$LONG_MISSION\" ...'"
# With apostrophes and newlines in $LONG_MISSION, bash sees 4+ positional args.
```

### Quick-check pattern (no AI, just raw browser)

For smoke checks where you don't need the AI agent:

```bash
expect-cli open "$URL" --headed --wait-until networkidle
expect-cli screenshot             # saves to /tmp/expect-artifacts/
expect-cli console_logs
expect-cli network_requests --url supabase
expect-cli close
```

These atomic commands don't need tmux (they return immediately), don't require a mission
string, and don't block on the install prompt.

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

**Note:** Screenshots are NOT saved to disk (base64 in AI context only). Use the `.webm` video for evidence extraction (see below).

## Publishing Interactive Replay (Recommended)

The HTML replay is the best evidence format — reviewers can scrub through the entire session interactively. Publish via `/here-now`:

```bash
SESSION_ID=$(ls -t /tmp/expect-replays/*.html | head -1 | xargs basename .html)
mkdir -p /tmp/replay
cp /tmp/expect-replays/${SESSION_ID}.html /tmp/replay/
cp /tmp/expect-replays/${SESSION_ID}.ndjson.js /tmp/replay/
cp /tmp/expect-replays/${SESSION_ID}.ndjson /tmp/replay/

# Publish (3 files: HTML shell + ndjson.js recording + ndjson raw data)
/.claude/skills/here-now/scripts/publish.sh /tmp/replay/ --client claude

# The replay URL is: https://{slug}.here.now/{SESSION_ID}.html
# Post to PR comment with the direct link
```

**Important**: The HTML file loads the `.ndjson.js` by deriving the filename from its own name. All 3 files must be published together, and the HTML must be accessed by its original filename (not as `index.html`).

**Expiry**: Anonymous publishes expire in 24h. Include the claim URL in the PR comment so the user can make it permanent.

## Extracting Screenshots from Recordings

Since expect-cli doesn't save per-step screenshots to disk, extract key frames from the `.webm` video using ffmpeg. This produces shareable PNG evidence for PR comments.

```bash
WEBM=$(ls -t /tmp/expect-replays/*.webm | head -1)
mkdir -p /tmp/screenshots

# Extract frames at key timestamps (adjust based on test step timing from logs)
# Timing guide: step 1 finish ≈ step1_duration, step 2 ≈ step1 + step2, etc.
ffmpeg -i "$WEBM" -ss 25 -frames:v 1 /tmp/screenshots/01-login.png -y 2>/dev/null
ffmpeg -i "$WEBM" -ss 50 -frames:v 1 /tmp/screenshots/02-home.png -y 2>/dev/null
ffmpeg -i "$WEBM" -ss 80 -frames:v 1 /tmp/screenshots/03-feature.png -y 2>/dev/null

# Validate screenshots show real content (not splash/blank)
for f in /tmp/screenshots/*.png; do
  dims=$(magick identify -format "%wx%h" "$f" 2>/dev/null)
  echo "$(basename $f): $dims ($(wc -c < "$f") bytes)"
done
```

**Timestamp estimation**: Sum step durations from the test log. If step 1 took 25s and step 2 took 17s, the step 2 completion frame is at ~42s.

## Uploading Evidence for PR Comments

### Option 1: imgbb (preferred for small batches)

```bash
IMGBB_API_KEY="${IMGBB_API_KEY:-006dfde8d5037a1e366db2bb24e915d3}"
for file in /tmp/screenshots/*.png; do
  sleep 3  # Rate limit
  url=$(curl -s -X POST "https://api.imgbb.com/1/upload?key=${IMGBB_API_KEY}" \
    --form "image=@${file}" | jq -r '.data.url // empty')
  echo "$(basename $file) → $url"
done
```

### Option 2: GitHub Draft Release (when imgbb rate-limits)

```bash
gh release create pr${PR}-evidence --repo OWNER/REPO \
  --title "PR #${PR} Evidence" --notes "Browser test screenshots" \
  --draft /tmp/screenshots/*.png

# Get download URLs
gh release view pr${PR}-evidence --repo OWNER/REPO --json assets \
  --jq '.assets[] | "\(.name) → \(.url)"'
```

### Posting to PR

```bash
BASE="https://github.com/OWNER/REPO/releases/download/TAG"
gh pr comment $PR --repo OWNER/REPO --body "$(cat <<EOF
## ✅ Browser Test Evidence

![login](${BASE}/01-login.png)
![feature](${BASE}/02-feature.png)

<details><summary>All screenshots</summary>

![step3](${BASE}/03-step.png)

</details>
EOF
)"
```

## Full Evidence Workflow (end-to-end)

```bash
# 1. Run test
SESSION="expect-$(date +%s)"
tmux new-session -d -s "$SESSION"
tmux send-keys -t "$SESSION" "EXPECT_REPLAY_OUTPUT_PATH=/tmp/expect-replays expect-cli -m 'test description' --headed -y --verbose 2>&1 | tee /tmp/expect.log" C-m

# 2. Wait for completion
sleep 120 && tmux capture-pane -t "$SESSION" -p | tail -20

# 3. Extract frames
WEBM=$(ls -t /tmp/expect-replays/*.webm | head -1)
ffmpeg -i "$WEBM" -ss 30 -frames:v 1 /tmp/screenshots/step1.png -y 2>/dev/null
ffmpeg -i "$WEBM" -ss 60 -frames:v 1 /tmp/screenshots/step2.png -y 2>/dev/null

# 4. Validate (read with Claude to verify content)
# Use Read tool on each PNG to verify it shows the expected UI state

# 5a. Publish interactive replay (PREFERRED)
SESSION_ID=$(ls -t /tmp/expect-replays/*.html | head -1 | xargs basename .html)
mkdir -p /tmp/replay
cp /tmp/expect-replays/${SESSION_ID}.{html,ndjson.js,ndjson} /tmp/replay/
/.claude/skills/here-now/scripts/publish.sh /tmp/replay/ --client claude
# Post the replay URL to PR

# 5b. Upload static screenshots (FALLBACK if replay not suitable)
gh release create prN-evidence --draft /tmp/screenshots/*.png
gh pr comment N --body "## Evidence\n![step1](url)\n![step2](url)"
```

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
