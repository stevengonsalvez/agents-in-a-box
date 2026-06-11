# mcporter alternative

If you'd rather run AgentMail through [mcporter](https://github.com/modelcontextprotocol/mcporter) (a thin shim that turns MCP tools into CLI invocations) instead of the bundled REST scripts, the recipes below mirror each script one-to-one. Use this when you already have mcporter wired into your environment and want one workflow across MCP servers.

mcporter is **optional**. The default skill flow uses plain curl + jq for predictability — no extra runtime, no MCP server lifecycle, no JSON-RPC framing to debug. mcporter is here for parity, not preference.

## Prereqs

```bash
# Install mcporter (one-time)
npm install -g @modelcontextprotocol/mcporter

# Point it at the AgentMail MCP server
mcporter add agentmail \
  --command npx --args -y @agentmail/mcp-server \
  --env AGENTMAIL_API_KEY=...      # if AgentMail starts gating the API later
```

Verify the four tools the MCP server exposes:

```bash
mcporter tools agentmail
# expected: create_inbox, list_messages, get_message, delete_inbox
```

## Recipes

### Create an inbox (`create.sh` equivalent)

```bash
ADDRESS=$(mcporter call agentmail create_inbox '{}' | jq -r '.address')
echo "$ADDRESS"
# Save your own state file alongside — mcporter won't track expiry for you.
```

### Poll for a message (`wait.sh` equivalent)

```bash
SLUG="mytest"
ADDR=$(jq -r '.address' .agents/agentmail/inboxes/${SLUG}.json)
DEADLINE=$(( $(date -u +%s) + 120 ))

while [ "$(date -u +%s)" -lt "$DEADLINE" ]; do
  COUNT=$(mcporter call agentmail list_messages "{\"address\":\"$ADDR\"}" | jq '.messages | length')
  if [ "$COUNT" -gt 0 ]; then
    break
  fi
  sleep 5
done
```

mcporter has no built-in backoff — wrap 429s manually if you hit the read limit (20/min).

### Read a message (`read.sh --extract verification` equivalent)

```bash
MSG=$(mcporter call agentmail list_messages "{\"address\":\"$ADDR\"}" | jq '.messages[0]')
MSG_ID=$(echo "$MSG" | jq -r '.id')
FULL=$(mcporter call agentmail get_message "{\"address\":\"$ADDR\",\"id\":\"$MSG_ID\"}")
echo "$FULL" | jq -r '.text' | grep -oE '\b[0-9]{6}\b' | head -1
```

### List local inboxes (no MCP equivalent)

`list.sh` reads from your local state dir — mcporter has nothing to do with this. Always use the bundled `list.sh` even if you went mcporter for everything else.

### Tear down (`expire.sh` equivalent)

```bash
# Optional — AgentMail auto-expires after 24h. delete_inbox is for early
# teardown when you want to free the slug name immediately.
mcporter call agentmail delete_inbox "{\"address\":\"$ADDR\"}"
rm -f .agents/agentmail/inboxes/${SLUG}.json
```

## Why default to REST, not mcporter

| Concern | REST scripts | mcporter |
|---|---|---|
| Runtime deps | curl + jq (already on dev machines) | node + mcporter + npx-run MCP server |
| Failure surface | one HTTP call per script | spawn MCP server → JSON-RPC handshake → tool call → parse response |
| State tracking | explicit `.agents/agentmail/` json | none — you bring your own |
| Rate-limit handling | bundled (`wait.sh` backs off on 429) | manual |
| Parallel inboxes | trivial (different slug files) | trivial (different address args) |
| MCP-free environments | works | requires node toolchain |

Default REST. Reach for mcporter only if you have a hard reason (e.g. your environment already standardises on MCP and you don't want a second integration shape).
