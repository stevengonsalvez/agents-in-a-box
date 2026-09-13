#!/usr/bin/env bash
# Start Grafana Alloy in a dedicated tmux session, forwarding local OTLP
# (Claude Code + Codex) to Grafana Cloud. Idempotent: re-running restarts it.
#
# Written by `ainb otel setup`. Reads creds + endpoint from grafana-cloud.env
# (same directory) and the OTLP fan-in pipeline from config.alloy (same
# directory). Override the config path with ALLOY_CONFIG=... if you keep a
# custom one.
set -euo pipefail

OTEL_DIR="$HOME/.agents-in-a-box/otel"
ENV_FILE="$OTEL_DIR/grafana-cloud.env"
CONFIG="${ALLOY_CONFIG:-$OTEL_DIR/config.alloy}"
SESSION="otel-alloy"
LOG="$HOME/.agents-in-a-box/logs/alloy.log"

[ -f "$ENV_FILE" ] || { echo "missing $ENV_FILE — run 'ainb otel setup' first"; exit 1; }
[ -f "$CONFIG" ] || { echo "missing $CONFIG — run 'ainb otel setup' first"; exit 1; }
# shellcheck disable=SC1090
source "$ENV_FILE"
case "${GRAFANA_API_TOKEN:-}" in ""|__GRAFANA_*|REPLACE_*) echo "fill creds in $ENV_FILE first (run 'ainb otel setup')"; exit 1;; esac

command -v alloy >/dev/null || { echo "alloy not installed — brew install grafana/grafana/alloy"; exit 1; }

mkdir -p "$(dirname "$LOG")"
# Alloy's WAL/data dir. Pin it under the otel home with an absolute
# --storage.path so Alloy never creates a stray ./data-alloy in whatever
# directory ainb happened to launch from.
DATA_DIR="$OTEL_DIR/data-alloy"
mkdir -p "$DATA_DIR"

# Kill only our own named session, never the server.
tmux has-session -t "$SESSION" 2>/dev/null && tmux kill-session -t "$SESSION"

tmux new-session -d -s "$SESSION" -n alloy
tmux send-keys -t "$SESSION:alloy" \
  "export GRAFANA_OTLP_ENDPOINT='$GRAFANA_OTLP_ENDPOINT' GRAFANA_INSTANCE_ID='$GRAFANA_INSTANCE_ID' GRAFANA_API_TOKEN='$GRAFANA_API_TOKEN'; alloy run --stability.level=experimental --server.http.listen-addr=127.0.0.1:12345 --storage.path '$DATA_DIR' '$CONFIG' 2>&1 | tee '$LOG'" C-m

echo "Alloy starting in tmux session '$SESSION'"
echo "  config:  $CONFIG"
echo "  logs:    $LOG"
echo "  health:  http://127.0.0.1:12345  (Alloy UI)"
echo "  attach:  tmux attach -t $SESSION"
