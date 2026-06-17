# Contributing

Thanks for your interest in contributing to **agents-in-a-box**.

## Quick start

```bash
git clone https://github.com/stevengonsalvez/agents-in-a-box.git
cd agents-in-a-box

# Build the ainb binary (Rust) — replaces the legacy bootstrap.js
cd ainb-tui && cargo build --release
```

## How the toolkit is organized

| Path | Purpose |
|------|---------|
| `ainb-tui/` | The `ainb` binary (Rust) — TUI plus `source`, `skill`, `migrate`, `doctor`, `usage` CLI subcommands. This is the canonical deploy / update / sync surface. |
| `ainb-tui/plans/skill-manager/spec.md` | Full design + acceptance criteria for the unit manager. |
| `toolkit/general-rules/` | Cross-tool source-of-truth rules (Go, deps, env, MCP, Postman, etc.) |
| `toolkit/packages/skills/` | Bundled skills (deployed via `ainb skill install` / `ainb skill sync`) |
| `plugins/reflect/` | The `reflect` plugin — installable via `claude plugin install reflect@agents-in-a-box` |
| `reflect-kb/` | Python library (root-level) — `reflect` CLI engine; installs via `uv tool install --upgrade 'git+https://github.com/stevengonsalvez/agents-in-a-box.git#subdirectory=reflect-kb[graph]'` |
| `toolkit/{cursor,cline,roo,copilot,amazonq}/` | Per-tool rule layouts (still in tree as units that `ainb` deploys) |
| `toolkit/catalog.yaml` | Auto-generated discovery surface (`toolkit/bin/generate-catalog.sh`) |
| `.claude-plugin/marketplace.json` | This repo's Claude plugin marketplace manifest |

> **Legacy:** `toolkit/bootstrap.js`, `toolkit/external-dependencies.yaml`,
> and `toolkit/scripts/update-externals.sh` were retired in May 2026 in
> favour of `ainb`. Migration path: `ainb migrate --from-bootstrap --toolkit-root ./toolkit`.

## Making a change

1. **Branch** from `main`.
2. **Atomic commits** — one concern per commit, conventional-commit prefix (`feat:`, `fix:`, `chore:`, `refactor:`, `docs:`).
3. **Don't bulk-commit** — if you've made multiple unrelated changes, split them with `git rebase -i` before pushing.
4. **No AI/Claude attribution** in commit messages. Write them as a human author.
5. **CI** runs on every PR (see `.github/workflows/toolkit-validation.yml`):
   - `validate-packages` — package directory structure
   - `test-ainb-installations` — full `cargo test --workspace` across the ainb crates plus a smoke install with `AINB_USE_REAL_HOMES=1` against a tempdir `$HOME`
   - `check-claude-code-thin-layer` — verifies the claude-code thin layer
6. **Open a PR** against `main`.
7. **Merge with `--merge`** (not squash) so per-concern commit history is preserved.

## Adding a new skill

Drop the skill under `toolkit/packages/skills/<name>/SKILL.md` (and `scripts/`, `assets/` if needed). Use template placeholders:

- `{{HOME_TOOL_DIR}}` — interpolated to `~/.claude`, `~/.codex`, `~/.copilot` per tool
- `{{TOOL_DIR}}` — interpolated to `.claude`, `.codex`, `.copilot`

Never hardcode `~/.claude` — agent-agnostic skills must use placeholders. `ainb` rewrites placeholders per target tool's `template_substitutions()` map at apply time.

## Adding a new plugin

1. Create `toolkit/packages/plugins/<name>/.claude-plugin/plugin.json`
2. Add the skills under `toolkit/packages/plugins/<name>/skills/`
3. Register in `.claude-plugin/marketplace.json` at repo root
4. Declare in your `~/.agents-in-a-box/manifest.yaml` under `units:` (or let `ainb skill install` populate it) so `ainb skill sync` will reconcile

## Reporting bugs / proposing features

Open a GitHub issue. For security-relevant issues see [SECURITY.md](./SECURITY.md).

## Code review

PRs are reviewed by humans + automated CI. Be patient — small fast PRs land faster than large ones.
