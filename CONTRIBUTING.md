# Contributing

Thanks for your interest in contributing to **agents-in-a-box**.

## Quick start

```bash
git clone https://github.com/stevengonsalvez/agents-in-a-box.git
cd agents-in-a-box

# Install bootstrap dependencies (small)
cd toolkit && npm install
```

## How the toolkit is organized

| Path | Purpose |
|------|---------|
| `toolkit/bootstrap.js` | The orchestrator: deploys rules and skills into per-tool config dirs (`~/.claude`, `~/.codex`, etc.) |
| `toolkit/general-rules/` | Cross-tool source-of-truth rules (Go, deps, env, MCP, Postman, etc.) |
| `toolkit/packages/skills/` | Bundled skills (deployed by bootstrap) |
| `toolkit/packages/plugins/reflect/` | The `reflect` plugin — installable via `claude plugin install reflect@agents-in-a-box` |
| `toolkit/{cursor,cline,roo,copilot,amazonq}/` | Per-tool rule layouts targeted by bootstrap |
| `toolkit/external-dependencies.yaml` | Manifest of every external skill, plugin, npx package, and CLI dependency |
| `toolkit/scripts/update-externals.sh` | Refreshes everything tracked in the manifest |
| `.claude-plugin/marketplace.json` | This repo's Claude plugin marketplace manifest |

## Making a change

1. **Branch** from `main`.
2. **Atomic commits** — one concern per commit, conventional-commit prefix (`feat:`, `fix:`, `chore:`, `refactor:`, `docs:`).
3. **Don't bulk-commit** — if you've made multiple unrelated changes, split them with `git rebase -i` before pushing.
4. **No AI/Claude attribution** in commit messages. Write them as a human author.
5. **CI** runs on every PR: template substitution checks, package validation, and tool-install tests for claude/codex/gemini. Wait for it to go green before requesting review.
   - There's also a Jest suite under `toolkit/bootstrap.test.js` (`cd toolkit && npm test`). Some assertions are currently stale (drift between expected output paths and what `bootstrap.js` actually produces — tracked separately). Use it as a smoke check; don't treat red there as a hard blocker until the suite is fixed.
6. **Open a PR** against `main`.
7. **Merge with `--merge`** (not squash) so per-concern commit history is preserved.

## Adding a new skill

Drop the skill under `toolkit/packages/skills/<name>/SKILL.md` (and `scripts/`, `assets/` if needed). Use template placeholders:

- `{{HOME_TOOL_DIR}}` — interpolated to `~/.claude`, `~/.codex`, `~/.copilot` per tool
- `{{TOOL_DIR}}` — interpolated to `.claude`, `.codex`, `.copilot`

Never hardcode `~/.claude` — agent-agnostic skills must use placeholders.

## Adding a new plugin

1. Create `toolkit/packages/plugins/<name>/.claude-plugin/plugin.json`
2. Add the skills under `toolkit/packages/plugins/<name>/skills/`
3. Register in `.claude-plugin/marketplace.json` at repo root
4. Track in `toolkit/external-dependencies.yaml` under `claude-plugins:`
5. Update `toolkit/scripts/update-externals.sh` if it needs special install handling

## Reporting bugs / proposing features

Open a GitHub issue. For security-relevant issues see [SECURITY.md](./SECURITY.md).

## Code review

PRs are reviewed by humans + automated CI. Be patient — small fast PRs land faster than large ones.
