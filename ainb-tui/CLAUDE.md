# ainb-tui

Terminal-based development environment manager for Claude Code agents. Built with Rust + ratatui.

## Quick Reference

```bash
# From monorepo root
cd ainb-tui

# Build & Run
cargo build                    # Debug build
cargo build --release          # Release build
cargo run                      # Run TUI
cargo run -- auth              # Run auth setup

# Test & Lint
cargo test                     # Run tests
cargo test -- --nocapture      # Tests with output
cargo clippy -- -D warnings    # Lint
cargo fmt                      # Format

# Just commands (if just installed)
just check                     # fmt + lint + test
just fix                       # Auto-fix formatting & lint
```

## Architecture

```
ainb-tui/                       # Cargo workspace root
├── Cargo.toml                  # [workspace] members + default-members = ainb-core
├── xtask/                      # Workspace task runner crate
└── crates/
    ├── ainb-core/              # The TUI + CLI binary (default build target)
    │   └── src/
    │       ├── main.rs         # Entry point, clap CLI, TUI loop
    │       ├── lib.rs          # Public API exports
    │       ├── app/            # Application state & event handling
    │       │   ├── state.rs        # App state machine
    │       │   ├── events.rs       # Event definitions
    │       │   └── attach_handler.rs
    │       ├── cli/            # CLI subcommands (run, list, fleet/, plugin/, ...)
    │       ├── components/     # TUI screen components (layout.rs, session_list.rs, ...)
    │       ├── widgets/        # Reusable UI widgets (message_router.rs, ...)
    │       ├── fleet/          # `ainb fleet` discover/read/send internals
    │       ├── docker/         # Container management
    │       ├── tmux/           # Tmux/PTY integration
    │       ├── git/            # Git operations
    │       ├── claude/         # Claude API client
    │       ├── providers/      # Agent provider integrations
    │       ├── plugins.rs      # Plugin host
    │       ├── models/         # Data models
    │       ├── config/         # Configuration handling
    │       └── agent_parsers/  # Parse agent output
    ├── ainb-plugin-protocol/   # v2 plugin JSON-RPC protocol types
    ├── ainb-plugin-runtime/    # Plugin host runtime
    ├── ainb-plugin-sdk-rust/   # Rust SDK for plugin authors
    ├── ainb-plugin-types-sessions/ # Shared session types
    ├── ainb-plugin-burndown/   # In-tree v2 plugin: usage analytics
    ├── ainb-plugin-notifyd/    # In-tree v2 plugin: notifications
    ├── ainb-plugin-session-reader/ # In-tree v2 plugin: data backend
    ├── ainb-plugin-cts-v2/     # Conformance test suite (14 axes)
    └── ainb-plugin-testkit/    # Plugin test harness for authors
```

## TUI Style Guide

All components MUST follow the color palette in `../.claude/skills/tui-screen/SKILL.md`:

```rust
// Primary
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);  // Borders
const GOLD: Color = Color::Rgb(255, 215, 0);               // Titles, CTAs
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);  // Active state

// Backgrounds
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);

// Text
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
```

**Mandatory patterns:**
- `BorderType::Rounded` on all panels
- Gold emoji + gold bold text for titles
- `▶` selection indicator with `SELECTION_GREEN`
- Bottom help bar: gold keys + muted descriptions

## Key Dependencies

| Crate | Purpose |
|-------|---------|
| `ratatui` | TUI framework |
| `crossterm` | Terminal handling |
| `tokio` | Async runtime |
| `bollard` | Docker API |
| `git2` | Git operations |
| `portable-pty` | PTY/tmux integration |

## Development Patterns

### Adding a New Component

1. Create `crates/ainb-core/src/components/my_component.rs`
2. Add state struct + render impl following template in skill
3. Add to `crates/ainb-core/src/components/mod.rs`
4. Add events to `crates/ainb-core/src/app/events.rs`
5. Wire into `crates/ainb-core/src/components/layout.rs`

### Adding a New Widget

1. Create `crates/ainb-core/src/widgets/my_widget.rs`
2. Add to `crates/ainb-core/src/widgets/mod.rs`
3. Use in components via `message_router.rs`

## Testing

```bash
# Unit tests
cargo test

# With visual debug output
cargo test --features visual-debug

# VT100 screen verification
cargo test --features vt100-tests

# E2E PTY tests
cargo test --test e2e_pty_tests
```

### Contract Tests: never rerun to green

Five test binaries are deterministic contracts, gated by their own CI job
(`Contracts`, see `.github/workflows/ci.yml`):

- `argv_golden_matrix` (`crates/ainb-hangar-daemon/tests/`): frozen per-provider CLI argv
- `migration_upgrade_full_chain` (`crates/ainb-hangar-store/tests/`): migration replay over a populated, adversarially-seeded db
- `axes`, `real_plugin_axes`, `wire_surface_gate` (`crates/ainb-plugin-cts-v2/tests/`): 14-axis plugin protocol conformance + wire-surface semver gate

They compare a deterministic rendering to a committed golden/lock file, so
they cannot legitimately flake. **If one goes red, never rerun the job to
green it.** Fix the code, or update the golden and commit the diff. A red
contract means a frozen guarantee actually broke, not noise.

To regenerate a golden after an intentional change:
- argv matrix: `UPDATE_GOLDEN=1 cargo test -p ainb-hangar-daemon --test argv_golden_matrix`
- wire surface: bump `version` in `crates/ainb-plugin-protocol/Cargo.toml`, then `UPDATE_WIRE_SURFACE=1 cargo test -p ainb-plugin-cts-v2 --test wire_surface_gate`
- migration chain: extend the seed in `migration_upgrade_full_chain.rs` directly; there is no separate regen command

## Recommended tmux Configuration

Claude Code generates high-frequency screen updates (4,000+ scroll events/sec) which causes flickering in tmux. See `config/tmux.conf` for recommended settings:

```bash
# Install recommended config
cp config/tmux.conf ~/.tmux.conf
tmux source-file ~/.tmux.conf
```

**Key settings:**
- Anti-flicker: `escape-time 0`, `status-interval 30`, `automatic-rename off`
- Clipboard: `set-clipboard on`, mouse drag → pbcopy, `prefix + P` to paste

### Clipboard Setup

The config enables clipboard integration for macOS. After installing:

| Action | How |
|--------|-----|
| Copy (mouse) | Drag to select, release → clipboard |
| Copy (keyboard) | `prefix + [`, select with `v`, press `y` |
| Paste | `prefix + P` (Shift+P) or `Cmd+V` |

**Terminal-specific setup:**

- **iTerm2**: Enable Preferences → General → "Applications in terminal may access clipboard"
- **Kitty/Ghostty**: Works out of the box with OSC 52
- **Warp**: Limited tmux support - use iTerm2/Kitty for tmux work

**macOS audio/notifications in tmux** (for `say` command, etc.):
```bash
brew install reattach-to-user-namespace
```
The config auto-detects and uses it if installed.

## Configuration

Configuration files are loaded from (in order of precedence):
1. `./.ainb/config.toml` (project-level; legacy `./.agents-box/config.toml` still read)
2. `~/.agents-in-a-box/config/config.toml` (user-level)
3. `/etc/agents-in-a-box/config.toml` (system-level)

See `config/example.config.toml` for all available options with documentation.

**Key settings:**

| Section | Option | Description |
|---------|--------|-------------|
| `[authentication]` | `claude_provider` | Auth method: system_auth, api_key, etc. |
| `[docker]` | `timeout` | Connection timeout in seconds (default: 60) |
| `[workspace_defaults]` | `branch_prefix` | Prefix for new branches (default: "agents/") |
| `[workspace_defaults]` | `exclude_paths` | Patterns to exclude from repo scanning |
| `[ui_preferences]` | `show_container_status` | Show container mode icons |
| `[ui_preferences]` | `show_git_status` | Show git changes in session list |
| `[mcp_pool]` | `enabled` | Share one MCP server process across host sessions (default: true) |
| `[mcp_pool]` | `idle_grace_secs` | Reap a pooled server N seconds after its last session detaches (default: 300) |
| `[mcp_servers.*]` | `shared` | Per-server pool opt-out — set false for stateful servers (default: true) |

### Daemon claude credential (no repeating keychain prompt)

The Hangar daemon hands its spawned `claude` child a `CLAUDE_CODE_OAUTH_TOKEN`
because that child cannot self-authenticate (it runs under a Seatbelt profile
that denies securityd, with a task-isolated HOME, so it reaches neither the
Keychain nor `~/.claude`). The daemon resolves that token in this order
(`crates/ainb-hangar-daemon/src/claude_cred.rs::resolve`):

1. `HANGAR_CLAUDE_OAUTH_TOKEN` in the daemon's own env (override), else
2. your SYSTEM `claude` login — the `Claude Code-credentials` Keychain item —
   read by shelling out to `/usr/bin/security`, else
3. the legacy stored token (`ainb-hangar::global` / `claude.oauth_token`), else
4. nothing — the run reaches `claude` and fails loudly.

Step 2 means **no setup step**: `just dev` and an installed `ainb` both just use
the `claude` login you already have. You may see **one** macOS keychain prompt
the first time ("security wants to use … Claude Code-credentials") — click
**Always Allow** and it never asks again.

Why one prompt, not one per launch: a Keychain ACL trusts the *requesting binary*
by code signature. The daemon is an unsigned binary whose hash changes on every
`just dev` / release rebuild (and every `brew upgrade` for installed `ainb`), so
an in-process read is never on the ACL and prompts every time. `/usr/bin/security`
is Apple-signed and stable, so "Always Allow" attaches to it and sticks across
all daemon rebuilds and upgrades. The access token is re-read fresh per dispatch,
so it self-heals past the ~8h token TTL as long as your `claude` login is current.

If step 2 can't read a token (not logged in), behavior is unchanged — the daemon
falls through to the legacy store, with no hard break.

**The ~8h TTL caveat:** the system access token (step 2) expires after ~8h and the
confined child cannot refresh it. Because the daemon re-reads it fresh per
dispatch, an idle-then-active daemon self-heals as long as your interactive
`claude` login is still valid. If the token IS expired at dispatch, the daemon
logs a clear hint (`system claude login token has expired … Open Claude Code to
refresh …`) and the run fails to authenticate rather than silently succeeding.
For a fully unattended daemon that must outlive the 8h window, set
`HANGAR_CLAUDE_OAUTH_TOKEN` (step 1) to a long-lived `claude setup-token` value —
that override wins and never expires on the 8h clock.

### Shared MCP Pool

With `[mcp_pool]` enabled, `ainb run` (Claude sessions) ensures a standalone
`ainb mcp daemon` is running and merge-writes the worktree's `.mcp.json` so
each pooled server points at the `ainb mcp proxy <socket>` stdio shim. The
daemon spawns each MCP server ONCE (lazily, on first attach) behind a unix
socket under `~/.agents-in-a-box/mcp/sockets/`, so N concurrent sessions
share 1 node/bun process instead of spawning N. Inspect with
`ainb mcp status`, stop with `ainb mcp stop`, validate end-to-end with
`scripts/validate-mcp-pool.sh` (repo root). Host/tmux sessions only —
Docker sessions keep their per-container MCP init. Servers whose commands
don't resolve on the host (e.g. the built-in container-path defaults) are
skipped automatically.

No hand-written TOML required: stdio servers found in a worktree's existing
`.mcp.json` are auto-imported into the pool at session create (sessions
push definitions to the running daemon over the control socket), and
`ainb mcp import [--user]` persists project `.mcp.json` + Claude user-scope
servers into `[mcp_servers.*]` config. `ainb mcp install --codex --copilot`
points `~/.codex/config.toml` / `~/.copilot/mcp-config.json` at the pool
shim (with `.bak` backups) so Codex and Copilot sessions share the same
backend processes as Claude.

## Conventions (paths & plugins)

- **All ainb state lives under `~/.agents-in-a-box/`** — config, plus the Hangar
  SQLite DB, control socket, daemon token, state and logs
  (`~/.agents-in-a-box/hangar.db`, `~/.agents-in-a-box/hangar.sock`,
  `~/.agents-in-a-box/hangar/…`). **Never `~/.ainb/`.** New persistence resolves
  from `dirs::home_dir()?.join(".agents-in-a-box")` (or `$AINB_HANGAR_HOME`).
- **TUI plugins live in `ainb-tui/crates/ainb-plugin-<name>/`** as workspace
  members — folder name == package name (`ainb-plugin-hangar`, `ainb-plugin-witr`,
  …). The repo-root `plugins/` directory is **only** for Claude Code harness
  plugins (`ainb-fleet`, `ainb-hooks`, `reflect`); never put a TUI/subprocess
  plugin there.

## Monorepo Context

The curated skills/agents/installer/catalog live in the standalone
`stevengonsalvez/ainb-toolkit` repo (flattened at its root); `ainb` consumes
it as a pinned external source. Git operations work against the monorepo root.

---

*Parent context: @../CLAUDE.md for commit conventions and global instructions*
