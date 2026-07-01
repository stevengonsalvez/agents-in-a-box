#!/usr/bin/env bash
# Visual demo of the shared MCP pool — drives REAL context7 (npx) and proves
# two independent session attachments share ONE backend process.
#
# Designed to be recorded by vhs (docs/assets/screenshots/mcp-pool.tape) but
# runs standalone too. Deterministic: isolated $HOME, no Claude auth/TUI, no
# network beyond context7's own npm fetch.
#
#   AINB_BIN=/path/to/ainb scripts/mcp-pool-demo.sh
set -uo pipefail

# ---- resolve the ainb binary -------------------------------------------------
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
AINB="${AINB_BIN:-}"
if [ -z "$AINB" ]; then
  AINB="$(find "$ROOT/ainb-tui/target/release" "$ROOT/ainb-tui/target/debug" \
            -maxdepth 1 -name ainb -type f -perm +111 2>/dev/null | head -1)"
fi
[ -x "$AINB" ] || { echo "ainb binary not found (set AINB_BIN or cargo build)"; exit 2; }
AINB="$(cd "$(dirname "$AINB")" && pwd)/$(basename "$AINB")"   # absolute — survives cd

cyan()  { printf '\033[1;36m%s\033[0m\n' "$*"; }
green() { printf '\033[1;32m%s\033[0m\n' "$*"; }
dim()   { printf '\033[2m%s\033[0m\n'   "$*"; }
run()   { printf '\033[1;33m$ %s\033[0m\n' "$*"; eval "$*"; }

# ---- isolated sandbox (own HOME → own sockets, never touches real pool) ------
SBX="$(mktemp -d /tmp/mcp-pool-demo-XXXX)"
export HOME="$SBX"
SOCK="$SBX/.agents-in-a-box/mcp/sockets/context7.sock"
cleanup() {
  exec 3>&- 4>&- 2>/dev/null || true
  "$AINB" mcp stop >/dev/null 2>&1 || true
  rm -rf "$SBX"
}
trap cleanup EXIT

mkdir -p "$SBX/proj"
cat > "$SBX/proj/.mcp.json" <<'JSON'
{ "mcpServers": {
    "context7": { "command": "npx", "args": ["-y", "@upstash/context7-mcp"] } } }
JSON
# Keep the backend alive across the demo (don't reap between attaches).
mkdir -p "$SBX/.agents-in-a-box/config"
printf '[mcp_pool]\nidle_grace_secs = 120\n' > "$SBX/.agents-in-a-box/config/config.toml"

clear
cyan "═══ ainb shared MCP pool — 2 sessions, ONE context7 process ═══"
echo
dim  "A project whose only MCP config is a plain .mcp.json (context7 via npx):"
run  "cat \"$SBX/proj/.mcp.json\""
echo

cyan "▸ ainb mcp import   —   .mcp.json → poolable server"
( cd "$SBX/proj" && "$AINB" mcp import --user ) | sed 's/^/  /'
echo

cyan "▸ start the pool daemon"
( cd "$SBX/proj" && "$AINB" mcp daemon >"$SBX/daemon.log" 2>&1 & )
sleep 2
green "  daemon up"
echo

# ---- attach two independent sessions via the stdio shim ----------------------
# Each shim is what a real Claude/Codex session spawns. We hold both open with
# FIFOs so the daemon sees two concurrent clients, and send each a real
# initialize + tools/list — the responses come from the ONE real context7.
attach() {  # $1 = index, $2 = fd, $3 = fifo, $4 = out
  local i="$1" fifo="$3" out="$4"
  mkfifo "$fifo"
  ( cd "$SBX/proj" && "$AINB" mcp proxy "$SOCK" <"$fifo" >"$out" 2>/dev/null & )
}

cyan "▸ session A attaches  →  initialize + tools/list  (real context7)"
attach 1 3 "$SBX/f1" "$SBX/o1"
exec 3>"$SBX/f1"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"A","version":"1"}}}' >&3
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}' >&3
printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' >&3
# Poll for the tools/list response (context7 npx cold-start varies).
for _ in $(seq 1 30); do
  grep -q '"tools"' "$SBX/o1" 2>/dev/null && break
  sleep 1
done
A_TOOLS=$(grep -o '"name":"[a-zA-Z_-]*"' "$SBX/o1" 2>/dev/null | sed 's/"name":"//;s/"//' | sort -u | tr '\n' ' ')
green "  A got tools: ${A_TOOLS:-<pending>}"
echo

cyan "▸ session B attaches  →  initialize + tools/list  (SAME process)"
attach 2 4 "$SBX/f2" "$SBX/o2"
exec 4>"$SBX/f2"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"B","version":"1"}}}' >&4
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}' >&4
printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' >&4
for _ in $(seq 1 15); do
  grep -q '"tools"' "$SBX/o2" 2>/dev/null && break
  sleep 1
done
B_TOOLS=$(grep -o '"name":"[a-zA-Z_-]*"' "$SBX/o2" 2>/dev/null | sed 's/"name":"//;s/"//' | sort -u | tr '\n' ' ')
green "  B got tools: ${B_TOOLS:-<pending>}"
echo

# ---- the proof (scoped to THIS daemon's child — orphan-proof) -----------------
STATUS=$( cd "$SBX/proj" && "$AINB" mcp status )
cyan "▸ pool status  —  two clients, one shared child process"
echo "$STATUS" | python3 -m json.tool 2>/dev/null \
  | grep -E '"name"|"clients"|"child_pid"|"state"' || true
echo

CHILD=$(echo "$STATUS" | python3 -c 'import sys,json; print(json.load(sys.stdin)["servers"][0]["child_pid"])' 2>/dev/null)
cyan "▸ the daemon's ONE context7 child is pid ${CHILD} — its whole process group:"
PG=$(ps -o pgid= -p "$CHILD" 2>/dev/null | tr -d ' ')
ps -Ao pid=,pgid=,command= | awk -v g="$PG" '$2==g' | grep -E 'context7|npm exec' | grep -v grep \
  | sed -E 's/  +/ /g; s/^/  /' | cut -c1-72
echo

PROCS_IN_SERVER=$(ps -Ao pgid=,command= | awk -v g="$PG" '$1==g' | grep -cE 'context7|npm exec')
green "═══ 2 sessions  ·  1 shared context7 server (pid ${CHILD}, ${PROCS_IN_SERVER} procs, 1 group) ═══"
dim   "    without the pool: 2 sessions → 2 servers → ~4 node processes"
sleep 4
