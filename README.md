# Agents in a Box

A comprehensive toolkit for AI coding agents - featuring a terminal UI for managing Claude Code sessions and a complete rules/skills system for AI coding assistants.

<p align="center">
  <img src="https://img.shields.io/badge/Platform-macOS%20%7C%20Linux%20%7C%20WSL-blue" alt="Platform">
  <img src="https://img.shields.io/badge/License-MIT-green" alt="License">
  <img src="https://img.shields.io/github/v/release/stevengonsalvez/agents-in-a-box" alt="Release">
</p>

---

<!-- TODO: Add demo GIF - see https://github.com/stevengonsalvez/agents-in-a-box/issues/20 -->
<p align="center">
  <img src="docs/assets/demo.gif" alt="ainb demo" width="800">
  <br>
  <em>Creating a new Claude Code session with model selection</em>
</p>

---

## What's Inside

| Component | Description |
|-----------|-------------|
| **[ainb-tui](#ainb---terminal-ui)** | Terminal UI for managing Claude Code development sessions |
| **[toolkit](toolkit/README.md)** | AI agent skills, rules, commands, and multi-tool configurations |
| **[Knowledge System](docs/how-reflection-works.md)** | How `/reflect` and `/research` capture and retrieve learnings (GraphRAG, QMD, two-tier storage) |

---

## ainb - Terminal UI

A Rust-based terminal application for managing Claude Code development sessions with git worktree isolation, model selection, and persistent tmux sessions.

### Features

- **Session Management** - Create, monitor, and switch between Claude Code sessions
- **Git Worktree Isolation** - Each session gets its own isolated git worktree
- **Model Selection** - Choose between Sonnet, Opus, and Haiku models per session
- **Live Log Streaming** - Real-time log viewer with filtering and search
- **tmux Integration** - Persistent sessions that survive disconnects
- **Keyboard-Driven** - Fast navigation with vim-style keybindings

### Quick Start

#### Installation

**Homebrew (macOS/Linux)**
```bash
brew tap stevengonsalvez/ainb
brew install ainb
```

**One-liner Install**
```bash
curl -fsSL https://raw.githubusercontent.com/stevengonsalvez/agents-in-a-box/v2/ainb-tui/install.sh | bash
```

**Cargo (any platform)**
```bash
cargo install --git https://github.com/stevengonsalvez/agents-in-a-box --branch v2 agents-box
# The binary will be named 'agents-box'. You may want to alias it to 'ainb':
# alias ainb="agents-box"  # Add to ~/.bashrc or ~/.zshrc
```

#### Usage

```bash
# Launch the TUI
ainb

# Set up Claude authentication
ainb auth
```

### Platform Support

| Platform | Status | Method |
|----------|--------|--------|
| macOS Apple Silicon (M1/M2/M3) | ✅ Full Support | Pre-built binary |
| macOS Intel | ✅ Full Support | Build from source |
| Linux x86_64 | ✅ Full Support | Pre-built binary |
| Linux ARM64 | ✅ Full Support | Build from source |
| Windows (WSL) | ✅ Full Support | See [WSL Setup](#windows-wsl-setup) |
| Windows (Native) | ❌ Not Supported | Use WSL |

### Windows (WSL) Setup

ainb works great on Windows through WSL2:

```powershell
# 1. Install WSL2 (if not already installed)
wsl --install

# 2. Open Ubuntu/Debian terminal and install
curl -fsSL https://raw.githubusercontent.com/stevengonsalvez/agents-in-a-box/v2/ainb-tui/install.sh | bash

# 3. Install tmux (required)
sudo apt update && sudo apt install -y tmux

# 4. Run ainb
ainb
```

**Why not native Windows?**
ainb relies on tmux for persistent terminal sessions, which is Unix-only. WSL provides the best Windows experience.

### Requirements

- **tmux** - Required for session management
- **git** - For worktree operations
- **Claude Code CLI** - The `claude` command must be available

```bash
# Install tmux
# macOS
brew install tmux

# Ubuntu/Debian
sudo apt install tmux

# Verify Claude CLI
claude --version
```

### Screenshots

<!-- TODO: Add screenshots - see https://github.com/stevengonsalvez/agents-in-a-box/issues/20 -->

<details>
<summary><b>Home Screen</b></summary>
<br>
<img src="docs/assets/screenshots/home.png" alt="Home screen with session list" width="700">
<p><em>Main dashboard showing active sessions with status indicators</em></p>
</details>

<details>
<summary><b>New Session</b></summary>
<br>
<img src="docs/assets/screenshots/new-session.png" alt="New session creation" width="700">
<p><em>Creating a new session with repository, branch, agent, and model selection</em></p>
</details>

<details>
<summary><b>Live Logs</b></summary>
<br>
<img src="docs/assets/screenshots/logs.png" alt="Live log viewer" width="700">
<p><em>Real-time log streaming with level filtering and search</em></p>
</details>

<details>
<summary><b>Session View</b></summary>
<br>
<img src="docs/assets/screenshots/session.png" alt="Active session" width="700">
<p><em>Attached to a Claude Code session with tmux integration</em></p>
</details>

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `j/k` or `↑/↓` | Navigate sessions |
| `Enter` | Attach to session |
| `n` | New session |
| `d` | Delete session |
| `r` | Restart Claude in session |
| `l` | View logs |
| `q` | Quit |

---

## Toolkit

A complete AI coding agent toolkit with skills, rules, commands, and configurations for multiple AI coding assistants.

**[→ Full Toolkit Documentation](toolkit/README.md)**

### Highlights

- **Skills** - Reusable capabilities (webapp-testing, crypto-research, tmux-monitor, etc.)
- **Agents** - Specialized AI agent definitions (backend-developer, frontend-developer, tech-lead-orchestrator, etc.)
- **Commands** - Slash commands for structured workflows (`/plan`, `/implement`, `/validate`, etc.)
- **Multi-Tool Support** - Configurations for Claude Code, Cursor, Amazon Q, Gemini, and more

### Quick Setup

```bash
cd toolkit
npm install
node create-rule.js --tool=claude-code
```

This copies the complete toolkit to `~/.claude/` for use with Claude Code.

### Supported Tools

| Tool | Installation Type |
|------|-------------------|
| Claude Code | Home directory (`~/.claude/`) |
| Gemini CLI | Project directory (`.gemini/`) |
| Amazon Q | Project directory (`.amazonq/rules/`) |
| Cursor | Project directory |
| Cline/Roo | Project directory |

---

## Repository Structure

```
agents-in-a-box/
├── ainb-tui/           # Terminal UI application (Rust)
│   ├── src/            # Source code
│   ├── Formula/        # Homebrew formula
│   └── install.sh      # Installer script
├── toolkit/            # AI agent toolkit
│   ├── claude-code/    # Claude Code configurations
│   ├── agents/         # Agent definitions
│   ├── commands/       # Slash commands
│   └── skills/         # Reusable skills
└── .github/            # CI/CD workflows
```

---

## Development

### Building ainb from source

```bash
cd ainb-tui
cargo build --release
./target/release/agents-box
```

### Running tests

```bash
cd ainb-tui
cargo test
```

### Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'feat: add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

---

## Links

- [Releases](https://github.com/stevengonsalvez/agents-in-a-box/releases)
- [Homebrew Tap](https://github.com/stevengonsalvez/homebrew-ainb)
- [Issues](https://github.com/stevengonsalvez/agents-in-a-box/issues)

---

## License

MIT License - see [LICENSE](LICENSE) for details.
