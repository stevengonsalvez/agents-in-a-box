#!/usr/bin/env bash
# E2E validation for the shared MCP pool (feat/mcp-socket).
#
# Proves, against REAL Claude Code sessions spawned by ainb:
#   1. N sessions with context7 → exactly ONE context7 backend process group
#   2. N `ainb mcp proxy` shim attachments on the pool socket
#   3. a context7 tool call succeeds from EVERY session
#   4. killing one session leaves the others' MCP working
#   5. after the last session detaches, the child is reaped post-grace
#
# Usage: scripts/validate-mcp-pool.sh [num_sessions]
# Env:   AINB_BIN=/path/to/ainb   (default: ainb-tui/target/release/ainb)
set -uo pipefail

N=${1:-3}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
AINB_BIN=${AINB_BIN:-"$ROOT/ainb-tui/target/release/ainb"}
GRACE=15
RUN_ID="mcpval-$$"
WORKDIR="$(mktemp -d "/tmp/${RUN_ID}-XXXX")"
PASS=0; FAIL=0

say()  { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }
ok()   { printf '\033[1;32mPASS\033[0m %s\n' "$*"; PASS=$((PASS+1)); }
bad()  { printf '\033[1;31mFAIL\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); }

cleanup() {
  say "cleanup"
  for i in $(seq 1 "$N"); do
    tmux kill-session -t "tmux_${RUN_ID}-s${i}" 2>/dev/null || true
  done
  (cd "$WORKDIR" && "$AINB_BIN" mcp stop >/dev/null 2>&1) || true
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

[ -x "$AINB_BIN" ] || { echo "ainb binary not found at $AINB_BIN (cargo build --release first)"; exit 2; }
command -v claude >/dev/null || { echo "claude CLI required"; exit 2; }
command -v jq >/dev/null || { echo "jq required"; exit 2; }

# ---------------------------------------------------------------- setup ----
say "setup: test repo + project config (context7 via npx, grace=${GRACE}s)"
REPO="$WORKDIR/repo"
mkdir -p "$REPO/.ainb"
git -C "$REPO" init -q -b main
echo "# mcp pool validation" > "$REPO/README.md"
git -C "$REPO" -c user.email=v@l.id -c user.name=validator add -A
git -C "$REPO" -c user.email=v@l.id -c user.name=validator commit -qm init

cat > "$REPO/.ainb/config.toml" <<EOF
[mcp_pool]
enabled = true
idle_grace_secs = $GRACE

[mcp_servers.context7]
name = "context7"
description = "context7 docs server"
enabled_by_default = true
shared = true
installation = { type = "PreInstalled" }
definition = { type = "Command", command = "npx", args = ["-y", "@upstash/context7-mcp"] }
EOF

# Fresh daemon for this run (project config is read from the daemon's cwd).
(cd "$REPO" && "$AINB_BIN" mcp stop >/dev/null 2>&1) || true
sleep 1

# Pre-trust each session dir for project .mcp.json servers so headless
# sessions don't hang on the interactive trust dialog. Targeted per-path
# entries in ~/.claude.json; removed on cleanup is not needed — paths are
# /tmp throwaways.
pretrust() {
  # Trust both the literal path and the canonical one (/tmp -> /private/tmp
  # on macOS — claude records the resolved path).
  local dir="$1" cfg="$HOME/.claude.json" resolved
  resolved="$(cd "$dir" && pwd -P)"
  [ -f "$cfg" ] || echo '{}' > "$cfg"
  for p in "$dir" "$resolved"; do
    jq --arg p "$p" '.projects[$p] = ((.projects[$p] // {}) + {"enableAllProjectMcpServers": true, "hasTrustDialogAccepted": true})' \
      "$cfg" > "$cfg.tmp.$$" && mv "$cfg.tmp.$$" "$cfg"
  done
}

# Belt + braces: if a session still shows the folder-trust dialog, accept it.
accept_trust_prompts() {
  for _ in $(seq 1 12); do
    local pending=0
    for i in $(seq 1 "$N"); do
      local s="tmux_${RUN_ID}-s${i}"
      if tmux capture-pane -t "$s" -p 2>/dev/null | grep -q "Is this a project you created or one you trust"; then
        tmux send-keys -t "$s" Enter
        pending=1
      fi
    done
    [ "$pending" -eq 0 ] && return 0
    sleep 3
  done
}

# --------------------------------------------------------------- spawn ----
say "spawn $N ainb sessions (claude + context7)"
for i in $(seq 1 "$N"); do
  SDIR="$WORKDIR/s$i"
  cp -R "$REPO" "$SDIR"
  pretrust "$SDIR"
  (cd "$SDIR" && "$AINB_BIN" run --repo "$SDIR" --name "${RUN_ID}-s${i}" \
      --dangerously-skip-permissions >/dev/null) \
    || { bad "ainb run session $i"; exit 1; }
done

sleep 5
accept_trust_prompts

# .mcp.json shim entries written?
for i in $(seq 1 "$N"); do
  if jq -e '.mcpServers.context7.args | index("proxy")' "$WORKDIR/s$i/.mcp.json" >/dev/null 2>&1; then
    ok "session $i .mcp.json points context7 at the pool shim"
  else
    bad "session $i .mcp.json missing shim entry"
  fi
done

# ------------------------------------------------- wait for attachments ----
say "wait: $N shims attached to pool (daemon status)"
deadline=$((SECONDS + 180))
clients=0
while [ $SECONDS -lt $deadline ]; do
  clients=$( (cd "$REPO" && "$AINB_BIN" mcp status 2>/dev/null) \
    | jq -r '.servers[]? | select(.name=="context7") | .clients' 2>/dev/null || echo 0)
  [ "${clients:-0}" -ge "$N" ] && break
  sleep 3
done
if [ "${clients:-0}" -ge "$N" ]; then
  ok "daemon reports $clients context7 clients"
else
  bad "expected $N clients, daemon reports '${clients:-none}'"
  (cd "$REPO" && "$AINB_BIN" mcp status) || true
fi

# ------------------------------------------------------ process counts ----
say "assert: exactly one context7 backend process group"
backend_pids=$(pgrep -f "context7-mcp" | tr '\n' ' ' || true)
pgroups=$(ps -o pgid= -p $(pgrep -f "context7-mcp" 2>/dev/null) 2>/dev/null | sort -u | grep -c . || echo 0)
if [ "$pgroups" -eq 1 ]; then
  ok "ONE context7 process group (pids: $backend_pids)"
else
  bad "expected 1 context7 process group, found $pgroups (pids: $backend_pids)"
fi

shims=$(pgrep -f "ainb mcp proxy" | grep -c . || echo 0)
if [ "$shims" -eq "$N" ]; then
  ok "$shims shim processes (one per session)"
else
  bad "expected $N shims, found $shims"
fi

# -------------------------------------------------- tool call per session --
say "tool call from every session (tmux drive, generous timeouts)"
for i in $(seq 1 "$N"); do
  T="tmux_${RUN_ID}-s${i}"  # plain session target — active window (base-index may be 1)
  MARK="POOLTEST_OK_${i}"
  tmux send-keys -t "$T" "Use the context7 MCP tool resolve-library-id with libraryName 'react'. If you get any result at all, reply with exactly ${MARK} and nothing else." C-m
  found=0
  for _ in $(seq 1 60); do
    sleep 5
    if tmux capture-pane -t "$T" -p -S -200 2>/dev/null | grep -q "$MARK"; then found=1; break; fi
  done
  if [ "$found" -eq 1 ]; then ok "session $i context7 tool call returned ($MARK)"; else
    bad "session $i tool call timed out"
    tmux capture-pane -t "$T" -p -S -30 2>/dev/null | tail -15
  fi
done

# ---------------------------------------------------------- resilience ----
say "resilience: kill session 1, others keep their MCP"
child_pid_before=$( (cd "$REPO" && "$AINB_BIN" mcp status) | jq -r '.servers[] | select(.name=="context7") | .child_pid')
tmux kill-session -t "tmux_${RUN_ID}-s1"
sleep 5
status_json=$( (cd "$REPO" && "$AINB_BIN" mcp status) )
clients_after=$(echo "$status_json" | jq -r '.servers[] | select(.name=="context7") | .clients')
child_pid_after=$(echo "$status_json" | jq -r '.servers[] | select(.name=="context7") | .child_pid')
if [ "$clients_after" -eq $((N-1)) ] && [ "$child_pid_after" = "$child_pid_before" ]; then
  ok "session 1 gone → $clients_after clients remain, same child pid $child_pid_after"
else
  bad "after kill: clients=$clients_after (want $((N-1))), child $child_pid_before→$child_pid_after"
fi

# -------------------------------------------------------------- grace ----
say "grace reap: kill remaining sessions, child should die within ${GRACE}s + slack"
for i in $(seq 2 "$N"); do tmux kill-session -t "tmux_${RUN_ID}-s${i}" 2>/dev/null || true; done
reaped=0
for _ in $(seq 1 $((GRACE + 30))); do
  sleep 1
  if ! pgrep -f "context7-mcp" >/dev/null 2>&1; then reaped=1; break; fi
done
if [ "$reaped" -eq 1 ]; then ok "backend reaped after last detach (grace ${GRACE}s)"; else
  bad "backend still alive $((GRACE + 30))s after last detach"
fi

# -------------------------------------------------------------- report ----
say "RESULT: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
