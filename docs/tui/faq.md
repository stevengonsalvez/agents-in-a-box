---
title: "FAQ & Tips"
---

Common questions and tips for using agents-in-a-box effectively.

---

## Tmux

### How do I delete word-by-word inside tmux?

Outside tmux, `Option+Delete` deletes word-by-word, but inside tmux it only deletes character-by-character. This is because tmux doesn't pass through the Option key modifier by default.

**Quick Solution: Use `Ctrl+W`**

`Ctrl+W` deletes the word before the cursor - this works universally in tmux without any configuration.

**To Get Option+Delete Working:**

#### 1. Terminal Emulator Settings

**iTerm2:**
- Preferences → Profiles → Keys → Key Mappings
- Find or add `⌥←Delete` (Option+Backspace)
- Set Action: "Send Escape Sequence"
- Set Esc+: `^H` (or `\x17` for Ctrl+W equivalent)

**Kitty** (`~/.config/kitty/kitty.conf`):
```
map alt+backspace send_text all \x17
```

**Ghostty** (`~/.config/ghostty/config`):
```
keybind = alt+backspace=text:\x17
```

#### 2. Shell Bindings (zsh)

Add to `~/.zshrc`:
```bash
# Word deletion bindings
bindkey '^[[3;3~' kill-word           # Alt+Delete (forward)
bindkey '\e^?' backward-kill-word     # Alt+Backspace (backward)
bindkey '\e\b' backward-kill-word     # Alternative binding
```

Then `source ~/.zshrc` or restart your shell.

#### 3. Ensure tmux passes modifiers

Add to `~/.tmux.conf`:
```bash
# Extended keys for modifier support
set -s extended-keys on
set -as terminal-features ',*:extkeys'
```

Then reload: `tmux source-file ~/.tmux.conf`

---

### How do I copy/paste in tmux?

See the [Clipboard Setup section in ainb-tui/CLAUDE.md](../../ainb-tui/CLAUDE.md#clipboard-setup) for full details.

**Quick reference:**

| Action | How |
|--------|-----|
| Copy (mouse) | Drag to select, release → clipboard |
| Copy (keyboard) | `prefix + [`, select with `v`, press `y` |
| Paste | `prefix + P` (Shift+P) or `Cmd+V` |

**Note:** Make sure you've installed the recommended tmux config:
```bash
cp config/tmux.conf ~/.tmux.conf
tmux source-file ~/.tmux.conf
```

---

### Why doesn't `say` or audio work in tmux?

macOS restricts access to user-session services (audio, notifications, keychain) from within tmux by default.

**Solution:** Install `reattach-to-user-namespace`:
```bash
brew install reattach-to-user-namespace
```

The recommended tmux config (`config/tmux.conf`) automatically uses it if installed.

---

### How do I reduce screen flickering in tmux?

Claude Code generates high-frequency screen updates (4,000+ scroll events/sec) which can cause flickering.

**Solution:** Use the recommended tmux config:
```bash
cp config/tmux.conf ~/.tmux.conf
tmux source-file ~/.tmux.conf
```

Key anti-flicker settings included:
- `escape-time 0` - Eliminates escape sequence delay
- `status-interval 30` - Reduces status bar refresh
- `automatic-rename off` - Reduces CPU overhead

---

## Sessions

### What are the different session types?

| Icon | Type | Description |
|------|------|-------------|
| Boss | Claude Code running in container | Full isolated environment |
| Tmux | Claude Code in tmux session | Lightweight, uses host environment |
| Shell | Plain shell session | No Claude, just terminal |
| Other | External tmux sessions | Sessions not created by ainb |

---

### How do I clean up orphaned sessions?

Press `x` from the session list to clean up:
- Orphaned Docker containers (`ainb-*`)
- Orphaned tmux sessions (`ainb-ws-*`, `ainb-shell-*`)

---

## Keyboard Shortcuts

### Session List

| Key | Action |
|-----|--------|
| `n` | New session |
| `Enter` | Attach to session |
| `d` / `D` | Delete session / with confirmation |
| `r` | Restart session |
| `x` | Cleanup orphaned sessions |
| `$` | Open shell in workspace |
| `o` | Open in editor |
| `F2` | Rename "Other" tmux session |
| `g` | Git view |
| `l` | Logs view |
| `c` | Config |
| `?` | Help |
| `q` | Quit |

---

## Troubleshooting

### Session won't start

1. Check Docker is running: `docker ps`
2. Check tmux is installed: `tmux -V`
3. Run dependency check: `agents-box` → follow onboarding wizard

### Can't paste into tmux

See [How do I copy/paste in tmux?](#how-do-i-copypaste-in-tmux) above.

### Terminal looks garbled

Try:
1. `Ctrl+L` to refresh screen
2. `reset` command in terminal
3. Restart tmux session

---

*Have a question not covered here? Open an issue on [GitHub](https://github.com/stevengonsalvez/agents-in-a-box/issues).*
