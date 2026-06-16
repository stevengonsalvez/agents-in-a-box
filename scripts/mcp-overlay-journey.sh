#!/usr/bin/env bash
# Journey harness for the MCP pool overlay, recorded by vhs. Sets up an
# isolated $HOME with a daemon + a fake server + two named sessions attached,
# then execs the ainb TUI so vhs can drive it (`p` opens the overlay).
#
# Self-contained & deterministic: own $HOME (never touches your real env),
# onboarding pre-completed so no wizard, a fake stdio MCP (no network).
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
AINB="${AINB_BIN:-}"
[ -z "$AINB" ] && AINB="$(find "$ROOT/ainb-tui/target/release" "$ROOT/ainb-tui/target/debug" -maxdepth 1 -name ainb -type f -perm +111 2>/dev/null | head -1)"
AINB="$(cd "$(dirname "$AINB")" && pwd)/$(basename "$AINB")"

T="$(mktemp -d /tmp/mcp-overlay-journey-XXXX)"
export HOME="$T"
mkdir -p "$HOME/.agents-in-a-box/config"

# Skip the first-run wizard.
cat > "$HOME/.agents-in-a-box/config/onboarding.toml" <<EOF
completed = true
version = "1.5.0"
EOF

cat > "$T/fake.js" <<'EOF'
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',l=>{let m;try{m=JSON.parse(l)}catch{return}
 if(m.id!==undefined)process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:m.id,result:{pid:process.pid}})+'\n')});
EOF

cat > "$HOME/.agents-in-a-box/config/config.toml" <<EOF
[mcp_pool]
idle_grace_secs = 300
monitor_refresh_secs = 2
[mcp_servers.context7]
name = "context7"
description = "docs server"
enabled_by_default = true
shared = true
installation = { type = "PreInstalled" }
definition = { type = "Command", command = "node", args = ["$T/fake.js"] }
EOF

SOCK="$HOME/.agents-in-a-box/mcp/sockets/context7.sock"

# Daemon + two named sessions attached, held open for the recording.
( "$AINB" mcp daemon >"$T/d.log" 2>&1 & )
sleep 2
hold() { ( printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}\n'; sleep 120 ) | "$AINB" mcp proxy "$SOCK" --session "$1" >/dev/null 2>&1 & }
hold web-session
hold api-session
sleep 3

# Clean up the sandbox when the TUI exits.
trap '"$AINB" mcp stop >/dev/null 2>&1; pkill -f "$T/fake.js" 2>/dev/null; rm -rf "$T"' EXIT

# Hand the terminal to the TUI so vhs keystrokes reach it.
exec "$AINB"
