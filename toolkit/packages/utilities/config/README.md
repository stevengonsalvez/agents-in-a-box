# Configuration Files

Dotfiles and configuration templates for Claude Code development environments.

## Files

### tmux.conf

Optimized tmux configuration for Claude Code:

- **Anti-flicker settings** - Reduces visual noise when Claude updates rapidly
- **Session isolation** - Prevents `CLAUDECODE` env var from causing nested session errors
- **Large history** - 1M lines of scrollback for reviewing Claude output
- **Mouse support** - Easy scrolling and pane selection

#### Installation

```bash
# Option 1: Replace your tmux.conf
cp tmux.conf ~/.tmux.conf

# Option 2: Source from existing config
echo "source-file $(pwd)/tmux.conf" >> ~/.tmux.conf

# Reload tmux config
tmux source-file ~/.tmux.conf
```

#### Key Fix: Nested Session Error

If you see this error when starting Claude in tmux:
```
Error: Claude Code cannot be launched inside another Claude Code session.
```

The fix is already in this config:
```bash
set-option -g update-environment "CLAUDECODE CLAUDE_CODE_ENTRYPOINT"
```

This tells tmux to refresh these variables from the current environment rather than preserving stale values.

**Manual fix if error persists:**
```bash
tmux set-environment -gu CLAUDECODE
tmux set-environment -gu CLAUDE_CODE_ENTRYPOINT
```
