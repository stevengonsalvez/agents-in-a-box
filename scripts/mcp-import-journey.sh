#!/usr/bin/env bash
# Journey harness for the MCP pool overlay's IMPORT action, recorded by vhs.
# Sets up an isolated $HOME + a project dir holding a `.mcp.json` (a real,
# host-resolvable stdio server) with NO pool daemon running, then execs the
# ainb TUI from inside that project so vhs can drive it:
#   `p` opens the overlay (empty — "Pool daemon not running")
#   `i` imports the launch dir's .mcp.json (+ Claude user scope) into the global
#       user config AND starts the pool daemon (which loads every configured
#       server on boot), so the imported server appears in the table. The
#       overlay is a global view, so import targets the user config (the one
#       read from anywhere), not a per-worktree project config.
#
# Self-contained & deterministic: own $HOME (never touches your real env),
# onboarding pre-completed so no wizard, a fake stdio MCP (no network).
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
AINB="${AINB_BIN:-}"
[ -z "$AINB" ] && AINB="$(find "$ROOT/ainb-tui/target/release" "$ROOT/ainb-tui/target/debug" -maxdepth 1 -name ainb -type f -perm +111 2>/dev/null | head -1)"
AINB="$(cd "$(dirname "$AINB")" && pwd)/$(basename "$AINB")"

T="$(mktemp -d /tmp/mcp-import-journey-XXXX)"
export HOME="$T"
mkdir -p "$HOME/.agents-in-a-box/config"

# Skip the first-run wizard.
cat > "$HOME/.agents-in-a-box/config/onboarding.toml" <<EOF
completed = true
version = "1.5.0"
EOF

# A trivial stdio MCP that just echoes a result — enough to register/spawn,
# no network. Its command (`node`) must resolve on host for import to accept.
cat > "$T/fake.js" <<'EOF'
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',l=>{let m;try{m=JSON.parse(l)}catch{return}
 if(m.id!==undefined)process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:m.id,result:{pid:process.pid}})+'\n')});
EOF

# Pool enabled, but NO [mcp_servers.*] — the overlay's `i` is what brings the
# server in, straight from the project .mcp.json below.
cat > "$HOME/.agents-in-a-box/config/config.toml" <<EOF
[mcp_pool]
idle_grace_secs = 300
monitor_refresh_secs = 2
EOF

# The project the user is "in": a .mcp.json declaring one stdio server.
PROJECT="$T/proj"
mkdir -p "$PROJECT"
cat > "$PROJECT/.mcp.json" <<EOF
{ "mcpServers": { "context7": { "command": "node", "args": ["$T/fake.js"] } } }
EOF

# No daemon — the overlay opens on "Pool daemon not running"; pressing `i`
# imports AND auto-starts the pool, which is exactly what we're proving.

# Clean up the sandbox when the TUI exits.
trap '"$AINB" mcp stop >/dev/null 2>&1; pkill -f "$T/fake.js" 2>/dev/null; rm -rf "$T"' EXIT

# Hand the terminal to the TUI, with cwd = the project so the overlay's import
# reads THIS .mcp.json as a source (the target is the global user config).
cd "$PROJECT"
exec "$AINB"
