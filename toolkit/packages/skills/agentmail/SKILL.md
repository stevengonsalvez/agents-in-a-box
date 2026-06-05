---
name: agentmail
description: Spin up disposable email inboxes via myagentinbox.com for tests, signup flows, OTP/magic-link captures, and any task that needs to receive mail from a service without using a real mailbox. Provides a REST-first bash workflow (no MCP required), 24h-lifetime inboxes, polling helpers with timeout, and verification-code/magic-link extractors. Use when the user mentions "throwaway email", "disposable inbox", "test signup", "OTP capture", "magic link", "verify email flow", "/expect-test signup", or any flow where a service emails a code and the agent needs to read it back. Stores inbox metadata in a tool-neutral state file under .agents/agentmail/ so multiple inboxes survive across sessions. MCP is intentionally NOT recommended — direct REST is simpler and avoids the npx mcp-remote shim. mcporter is documented as a fallback for projects already standardised on MCP tooling.
---

# agentmail — disposable inboxes for agent workflows

## What this skill gives you

The `myagentinbox.com` service hands out 24-hour disposable inboxes via plain REST. Five things to know:

| capability | how |
|---|---|
| Create inbox | `POST /api/inboxes` — returns `{address, created_at, expires_in: "24h"}`. No auth. |
| Check inbox exists | `GET /api/inboxes/{address}` |
| List messages | `GET /api/inboxes/{address}/messages` — returns `{data: [...]}` |
| Read message | `GET /api/inboxes/{address}/messages/{id}` |
| Download attachment | `GET /api/inboxes/{address}/messages/{id}/attachments/{filename}` |

Limits (per IP):

| limit | value |
|---|---|
| Inbox creation | 3 per minute |
| API reads | 20 per minute |
| Inbox lifetime | 24 hours then auto-deleted |
| Max email size | 10 MB |

**No accounts. No keys. No tracking.** Inbox + everything in it vanishes after 24h. Perfect for ephemeral test flows; useless for anything that needs to be retained.

## When to use

- Driving `/expect-test`, `/shot-testing`, or any browser test through a signup flow that requires email verification
- Capturing a one-time password / magic-link sent by a service the agent is registering with
- Receiving a webhook-style email as a stage in an automated pipeline
- Pulling an attachment (PDF receipt, exported report) that a service emails out

## When NOT to use

- Anything you need to keep — inboxes are gone after 24h
- Long-lived account recovery email — pick a real mailbox
- Sending email — myagentinbox only receives
- Replacing a production account email — only for tests/agent workflows

## Quick start — 4-line flow

```bash
# 1. Create an inbox (writes state to .agents/agentmail/inboxes/default.json)
ADDR=$({{HOME_TOOL_DIR}}/skills/agentmail/scripts/create.sh default)
echo "Inbox: $ADDR"

# 2. Drive your signup flow with $ADDR as the email — through expect-cli,
#    curl, browser-harness, whatever.

# 3. Wait for the verification email (default timeout 120s, poll every 5s)
{{HOME_TOOL_DIR}}/skills/agentmail/scripts/wait.sh default

# 4. Pull the verification code or magic-link out of the latest message
{{HOME_TOOL_DIR}}/skills/agentmail/scripts/read.sh default --extract verification
```

## Bundled scripts

All scripts live in `{{HOME_TOOL_DIR}}/skills/agentmail/scripts/` and are executable. They read/write a state file at `${AGENTMAIL_STATE_DIR:-./.agents/agentmail}/inboxes/<slug>.json` so the inbox address persists across commands (and across agent sessions, until the 24h lifetime expires).

| script | purpose |
|---|---|
| `create.sh <slug>` | Create new inbox, save state. Errors out if a non-expired inbox already exists under that slug (prevents accidental wipe). Pass `--force` to override. |
| `wait.sh <slug> [--timeout 120] [--interval 5]` | Poll inbox until message arrives or timeout hits. Exits 0 on first message, 124 on timeout. |
| `read.sh <slug> [--extract verification\|magic-link\|raw] [--index 0]` | Fetch a message and optionally extract a 6-digit code or a magic-link URL. Index 0 = latest. |
| `list.sh` | Show all known inboxes from state with their age/expiry. |
| `address.sh <slug>` | Print just the email address for the named slug. Useful for piping into form-fillers. |
| `expire.sh <slug>` | Mark the inbox expired and remove state file. Server-side deletion is automatic at 24h; this is local-state cleanup only. |

### State file schema

`./.agents/agentmail/inboxes/<slug>.json`:

```jsonc
{
  "slug": "default",
  "address": "h4fphuv43v@myagentinbox.com",
  "created_at": "2026-05-11T12:27:56Z",
  "expires_at": "2026-05-12T12:27:56Z",
  "purpose": "PR #2548 signup verification",
  "last_polled_at": "2026-05-11T12:31:02Z",
  "message_count": 0
}
```

Schema is intentionally narrow: address + lifecycle. Don't store message bodies here — fetch fresh from the API.

### Where state lives

| location | when used |
|---|---|
| `${AGENTMAIL_STATE_DIR}/inboxes/<slug>.json` | if env var set (CI / explicit override) |
| `<git-root>/.agents/agentmail/inboxes/<slug>.json` | inside a git repo — tool-neutral, sits next to `.agents/MEMORY.md`, `.agents/standup/`. Same convention. |
| `~/.cache/agentmail/inboxes/<slug>.json` | outside a git repo (ad-hoc agent runs) |

Scripts auto-detect. Override with `AGENTMAIL_STATE_DIR=/some/path`.

## Worked example — full signup verification flow

```bash
# Setup: slug + state file
SLUG="staging-signup-$(date +%s)"
ADDR=$({{HOME_TOOL_DIR}}/skills/agentmail/scripts/create.sh "$SLUG" --purpose "M1 staging signup verification")

# Drive signup form (could be expect-cli, browser-harness, curl, etc.)
curl -s -X POST "https://cofbadigmvblbzikmmnf.supabase.co/auth/v1/signup" \
  -H "apikey: $STAGING_ANON_KEY" \
  -H "Content-Type: application/json" \
  -d "{\"email\":\"$ADDR\",\"password\":\"Th3SHOT123!\"}"

# Wait up to 90s for the verification email
if {{HOME_TOOL_DIR}}/skills/agentmail/scripts/wait.sh "$SLUG" --timeout 90; then
    # Extract the verification link from the message
    LINK=$({{HOME_TOOL_DIR}}/skills/agentmail/scripts/read.sh "$SLUG" --extract magic-link)
    echo "Verification link: $LINK"

    # Hit the link to complete verification
    curl -sL "$LINK" -o /dev/null -w "STATUS=%{http_code}\n"
else
    echo "Timed out waiting for verification email"
    exit 1
fi
```

## Patterns for AI-driven tests

### Pattern 1 — expect-cli mission with inline inbox

Pre-generate the inbox before launching expect-cli, then pass the address into the mission text:

```bash
ADDR=$({{HOME_TOOL_DIR}}/skills/agentmail/scripts/create.sh expect-signup)
MISSION_FILE=/tmp/mission-$(date +%s).txt
cat > "$MISSION_FILE" <<EOF
Navigate to https://staging-shot.vercel.app/signup
Fill the email field with: $ADDR
Fill the password field with: Th3SHOT123!
Fill DOB with 1990-01-01
Click the Register button
Wait for the confirmation page to load.
EOF

tmux new-session -d -s expect-test \
  "expect-cli tui -m \"\$(cat $MISSION_FILE)\" --browser-mode headed -y --verbose | tee /tmp/expect.log"

# After expect-cli completes the form, poll for the email
{{HOME_TOOL_DIR}}/skills/agentmail/scripts/wait.sh expect-signup --timeout 120
LINK=$({{HOME_TOOL_DIR}}/skills/agentmail/scripts/read.sh expect-signup --extract magic-link)
```

### Pattern 2 — OTP/6-digit code capture

```bash
ADDR=$({{HOME_TOOL_DIR}}/skills/agentmail/scripts/create.sh otp-flow)
# ... trigger the OTP send ...
{{HOME_TOOL_DIR}}/skills/agentmail/scripts/wait.sh otp-flow --timeout 60
CODE=$({{HOME_TOOL_DIR}}/skills/agentmail/scripts/read.sh otp-flow --extract verification)
echo "OTP: $CODE"
```

### Pattern 3 — multiple parallel inboxes

```bash
for ROLE in coach parent athlete; do
  ADDR=$({{HOME_TOOL_DIR}}/skills/agentmail/scripts/create.sh "test-$ROLE" --purpose "lane2 $ROLE")
  echo "$ROLE: $ADDR"
done
{{HOME_TOOL_DIR}}/skills/agentmail/scripts/list.sh
```

## Architecture

```
   your script ──▶ create.sh ──POST /api/inboxes──▶  myagentinbox.com
                       │                                 │
                       │                                 │  stores inbox 24h
                       ▼                                 │
              .agents/agentmail/                         │
              inboxes/<slug>.json   ◀─────polls──────────│
                       │                                 │
                       ▼                                 │
   your script ──▶ wait.sh ──GET messages every 5s──────▶│
                       │                                 │
                       │ message arrives                 │
                       ▼                                 │
   your script ──▶ read.sh ──GET message/{id}───────────▶│
                       │
                       ▼
              extracted code / magic-link / raw body
```

Three layers:
- **Persistence**: state file on disk. Address survives across agent invocations.
- **Transport**: plain `curl` to a public REST API. No npx, no MCP server, no Node runtime needed.
- **Extraction**: regex helpers (verification code, magic link) baked into `read.sh`.

## Why no MCP

The myagentinbox guide pushes MCP as the primary integration. Three reasons this skill defaults to plain REST:

| concern | MCP approach | REST approach |
|---|---|---|
| Setup | `claude mcp add ... npx mcp-remote ...` + verify load | `chmod +x` the script |
| Dependencies | Node.js + `npx mcp-remote` shim | `curl` + `jq` |
| Cross-tool use | Tied to one MCP client (Claude Code / Desktop) | Works from any shell, any agent, any CI |
| Cross-session persistence | Address lives only in Claude's tool state | State file on disk |
| Failure mode | npx shim breakage → opaque MCP errors | curl exit code, visible HTTP status |
| Token cost in agent context | MCP tool defs are pulled into every turn | Zero — runs as bash, not as a tool |

The REST flow is strictly simpler. Use MCP only if you're already standardised on it for other reasons.

## mcporter alternative (when you must)

If a project already uses [mcporter](https://github.com/dgellow/mcporter) to talk to MCP servers from the CLI, you can call myagentinbox's MCP tools directly without the npx shim or SDK. See `references/mcporter-alternative.md` for the drop-in recipe covering all four tools (`create_inbox` / `check_inbox` / `read_message` / `download_attachment`) with mcporter calls equivalent to each `scripts/*.sh`.

When to prefer mcporter: the fleet already routes ALL external calls through MCP for audit/rate-limit reasons. Otherwise stick with the REST scripts in this skill.

## Failure modes & how the scripts handle them

| failure | symptom | script behaviour |
|---|---|---|
| Inbox creation rate-limit (3/min) | HTTP 429 | `create.sh` exits 1 with "rate limited, retry in {N}s" read from `Retry-After` header. |
| Read rate-limit (20/min) | HTTP 429 | `wait.sh` auto-backs off — doubles polling interval up to 30s. |
| Inbox expired (>24h) | HTTP 404 on GET | `wait.sh`/`read.sh` exit 1, suggest recreating via `create.sh --force`. |
| Network error | curl exit code | scripts retry once with 2s delay; on second failure exit with status. |
| Missing `jq` | `jq: not found` | scripts check upfront and exit with install hint. |
| Multiple agents sharing one inbox | last-poll race | benign — each agent gets its own copy of the message list. |

## Hard rules

- **Never store message contents in the state file** — fetch fresh from the API. Bodies may contain OTPs, PII, signed links. Disk = no.
- **Don't use this for production user emails.** The 24h purge will lose data. Real mailbox needed.
- **Treat returned magic links as one-shot.** Don't log them to chat/Slack/PR comments — anyone who sees them can use them.
- **Don't share an inbox across long-running multi-day flows.** The 24h ceiling will hit.
- **Don't rely on `myagentinbox.com` for anything load-bearing.** It's free, public, and could go away. For high-reliability E2E suites, run a self-hosted Mailpit/Mailhog and only fall back to this for ad-hoc agent runs.

## Caveman default

Output text obeys caveman mode: drop articles, filler, hedging. Keep technical terms exact. Address user as **Stevie**.

## When to use this skill

- User mentions "throwaway email", "disposable inbox", "test signup", "OTP", "magic link", "verify email flow"
- User asks `/expect-test` (or `/shot-testing`, etc) to drive a signup that needs email confirmation
- User says "spin up a disposable email and register"
- User wants to receive an automated email (e.g. PDF receipt) in a pipeline run

## When NOT to use this skill

- Real user account flows (use a real mailbox)
- Local-only test loops where email confirmation is disabled (use Mailpit / Mailhog instead — already in SHOT's local stack)
- Anything that needs >24h persistence
