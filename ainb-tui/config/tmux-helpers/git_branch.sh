#!/bin/sh
# git_branch.sh — print the git branch of the focused pane's cwd, or empty.
# Used by ~/.tmux.conf status-right via #(~/.tmux/scripts/git_branch.sh).
#
# Why a helper instead of inline `#( git -C #{pane_current_path} ... )`:
# tmux only sometimes interpolates format vars inside #(...). Asking tmux for
# the path via display-message inside the shell command always works and
# avoids platform-specific escape gymnastics in the conf.

path=$(tmux display-message -p '#{pane_current_path}' 2>/dev/null)
[ -d "$path" ] || exit 0
branch=$(git -C "$path" symbolic-ref --short HEAD 2>/dev/null) || exit 0
printf ' %s ' "$branch"
