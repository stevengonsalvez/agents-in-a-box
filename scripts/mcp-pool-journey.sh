#!/usr/bin/env bash
# A from-scratch USER GUIDE for the shared MCP pool, recorded as a VHS GIF.
# Walks the real commands a user runs — `ainb mcp import`, sessions attaching,
# `ainb mcp status` — against a REAL context7 server (npx), and proves two
# sessions share ONE backend process.
#
# Deterministic & self-contained: isolated $HOME (never touches your real pool
# sockets), no Claude auth/TUI. Sessions are shown via the exact stdio shim an
# ainb session runs (`ainb mcp proxy`).
#
#   AINB_BIN=ainb-tui/target/release/ainb scripts/mcp-pool-journey.sh
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
AINB="${AINB_BIN:-}"
if [ -z "$AINB" ]; then
  AINB="$(find "$ROOT/ainb-tui/target/release" "$ROOT/ainb-tui/target/debug" \
            -maxdepth 1 -name ainb -type f -perm +111 2>/dev/null | head -1)"
fi
[ -x "$AINB" ] || { echo "ainb binary not found (set AINB_BIN or cargo build)"; exit 2; }
AINB="$(cd "$(dirname "$AINB")" && pwd)/$(basename "$AINB")"

# Expose the binary as a plain `ainb` on PATH so the guide shows clean
# commands (`ainb mcp import`) rather than an absolute target/ path.
BINDIR="$(mktemp -d /tmp/mcp-journey-bin-XXXX)"
ln -sf "$AINB" "$BINDIR/ainb"
export PATH="$BINDIR:$PATH"

# ── palette ───────────────────────────────────────────────────────────────
B='\033[1m'; DIM='\033[2m'; R='\033[0m'
CY='\033[1;36m'; GR='\033[1;32m'; YE='\033[1;33m'; GY='\033[38;5;245m'
step()  { printf "\n${CY}── %s ──${R}\n" "$*"; }
note()  { printf "${GY}%s${R}\n" "$*"; }
cmd()   { printf "${YE}\$ %s${R}\n" "$*"; eval "$*"; }
ok()    { printf "${GR}%s${R}\n" "$*"; }
pause() { sleep "${1:-1.2}"; }

SBX="$(mktemp -d /tmp/mcp-pool-journey-XXXX)"
export HOME="$SBX"
SOCK="$SBX/.agents-in-a-box/mcp/sockets/context7.sock"
cleanup() { exec 3>&- 4>&- 2>/dev/null || true; ainb mcp stop >/dev/null 2>&1 || true; rm -rf "$SBX" "$BINDIR"; }
trap cleanup EXIT

mkdir -p "$SBX/my-project"
cd "$SBX/my-project"
cat > .mcp.json <<'JSON'
{ "mcpServers": {
    "context7": { "command": "npx", "args": ["-y", "@upstash/context7-mcp"] } } }
JSON
mkdir -p "$SBX/.agents-in-a-box/config"
printf '[mcp_pool]\nidle_grace_secs = 120\n' > "$SBX/.agents-in-a-box/config/config.toml"

clear
printf "${B}  Shared MCP pool — a from-scratch walkthrough${R}\n"
note   "  Goal: run ONE MCP server process for many ainb sessions."
pause 1.5

step "1 · You have a project with an MCP server"
note "Just a normal project-scoped .mcp.json — context7 over npx:"
cmd  "cat .mcp.json"
pause 2

step "2 · Make it poolable — one command"
note "Imports the stdio server into ainb config so the pool can manage it."
cmd  "ainb mcp import --user"
note "(New ainb sessions also auto-import .mcp.json — this is the explicit path.)"
pause 2

step "3 · The pool daemon (auto-starts on first session; shown here explicitly)"
( cd "$SBX/my-project" && ainb mcp daemon >"$SBX/daemon.log" 2>&1 & )
sleep 2
ok "  ✓ ainb mcp daemon running"
pause 1.5

step "4 · Two sessions attach"
note "Each ainb session runs the stdio shim:  ainb mcp proxy <socket>"
note "We attach two and ask each for context7's tool list (REAL context7):"
attach() { mkfifo "$2"; ( cd "$SBX/my-project" && ainb mcp proxy "$SOCK" <"$2" >"$3" 2>/dev/null & ); }

printf "${GY}  session-1 →${R} "; attach 1 "$SBX/f1" "$SBX/o1"; exec 3>"$SBX/f1"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"session-1","version":"1"}}}' >&3
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}' >&3
printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' >&3
for _ in $(seq 1 40); do grep -q '"tools"' "$SBX/o1" 2>/dev/null && break; sleep 1; done
T1=$(grep -o '"name":"[a-zA-Z_-]*"' "$SBX/o1" 2>/dev/null | sed 's/"name":"//;s/"//' | sort -u | tr '\n' ' ')
ok "tools: ${T1:-?}"

printf "${GY}  session-2 →${R} "; attach 2 "$SBX/f2" "$SBX/o2"; exec 4>"$SBX/f2"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"session-2","version":"1"}}}' >&4
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}' >&4
printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' >&4
for _ in $(seq 1 15); do grep -q '"tools"' "$SBX/o2" 2>/dev/null && break; sleep 1; done
T2=$(grep -o '"name":"[a-zA-Z_-]*"' "$SBX/o2" 2>/dev/null | sed 's/"name":"//;s/"//' | sort -u | tr '\n' ' ')
ok "tools: ${T2:-?}"
pause 1.5

step "5 · Inspect the pool"
STATUS=$( cd "$SBX/my-project" && ainb mcp status )
printf "${YE}\$ ainb mcp status${R}\n"
echo "$STATUS" | python3 -m json.tool 2>/dev/null | grep -E '"name"|"clients"|"child_pid"|"state"' | sed 's/^/  /'
pause 2

step "6 · How many context7 processes are actually running?"
CHILD=$(echo "$STATUS" | python3 -c 'import sys,json;print(json.load(sys.stdin)["servers"][0]["child_pid"])' 2>/dev/null)
PG=$(ps -o pgid= -p "$CHILD" 2>/dev/null | tr -d ' ')
cmd  "ps -Ao pid=,pgid=,command= | awk '\$2==$PG' | grep -E 'context7|npm exec' | grep -v grep"
echo
NPROC=$(ps -Ao pgid=,command= | awk -v g="$PG" '$1==g' | grep -cE 'context7|npm exec')

printf "\n${GR}${B}  ✓ 2 sessions  ·  1 shared context7 server${R}  ${GY}(pid ${CHILD}, ${NPROC} procs, 1 group)${R}\n"
note   "    Without the pool: 2 sessions → 2 servers → ~4 node processes."
note   "    Inspect any time with  ainb mcp status  ·  stop with  ainb mcp stop"
sleep 5
