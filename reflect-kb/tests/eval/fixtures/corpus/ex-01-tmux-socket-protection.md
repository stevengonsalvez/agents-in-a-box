---
name: ex-01-tmux-socket-protection
title: "Never kill the tmux server — sessions die irreversibly"
category: tooling
tags:
  - tmux
  - process-management
  - safety
confidence: high
created: "2026-03-01"
key_insight: "Always kill tmux sessions by exact name; tmux kill-server destroys every session on the socket."
---
## Learning

`tmux kill-server` and `pkill tmux` destroy all sessions across all projects. Use `tmux kill-session -t <exact-name>` only.

**How to apply:** Always kill tmux sessions by exact name; tmux kill-server destroys every session on the socket.
