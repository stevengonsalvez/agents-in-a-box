# shellcheck shell=bash
# Shared host registry + persisted-selection helpers for the cc-paste Raycast commands.
# Sourced by clipboard-image-to-ssh-path.sh (sender) and clipboard-image-set-host.sh (switcher).
# Not a Raycast command itself (no @raycast headers) so Raycast ignores it.
# bash 3.2 compatible (macOS /bin/bash) — no associative arrays.

CC_DEFAULT_HOST="mac"
CC_STATE_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/cc-paste-host"

# Host registry. key -> "ssh-target|tailscale-peer-hostname"
# ssh-target may include user@ ; peer-hostname is matched (case-insensitive) for the online check.
# Add a host = add a case line here (and a dropdown entry in clipboard-image-set-host.sh).
cc_resolve() {
  case "$1" in
    mac)     echo "stevens-macbook-pro-5|Stevens-MacBook-Pro-5" ;;
    hetzner) echo "claude@claude-hetzner|claude-hetzner" ;;
    *)       echo "" ;;
  esac
}

# Current host key: persisted choice if valid, else default.
cc_current_key() {
  local k=""
  [ -f "$CC_STATE_FILE" ] && k="$(tr -d '[:space:]' < "$CC_STATE_FILE")"
  if [ -n "$k" ] && [ -n "$(cc_resolve "$k")" ]; then
    printf '%s' "$k"
  else
    printf '%s' "$CC_DEFAULT_HOST"
  fi
}
