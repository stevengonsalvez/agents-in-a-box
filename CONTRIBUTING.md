# Contributing

Thanks for your interest in contributing to **agents-in-a-box**.

## Quick start

```bash
git clone https://github.com/stevengonsalvez/agents-in-a-box.git
cd agents-in-a-box

# Build the ainb binary (Rust) — replaces the legacy bootstrap.js
cd ainb-tui && cargo build --release
```

## How this repo is organized

| Path | Purpose |
|------|---------|
| `ainb-tui/` | The `ainb` binary (Rust) — TUI plus `source`, `skill`, `migrate`, `doctor`, `usage` CLI subcommands. This is the canonical deploy / update / sync surface. |
| `ainb-tui/plans/skill-manager/spec.md` | Full design + acceptance criteria for the unit manager. |
| `plugins/reflect/` | The `reflect` plugin — installable via `claude plugin install reflect@agents-in-a-box` |
| `reflect-kb/` | Python library (root-level) — `reflect` CLI engine; installs via `uv tool install --upgrade 'git+https://github.com/stevengonsalvez/agents-in-a-box.git#subdirectory=reflect-kb[graph]'` |
| `.claude-plugin/marketplace.json` | This repo's Claude plugin marketplace manifest |

The portable skills, agents, workflows, utilities, per-tool rule layouts
(`cursor/cline/roo/copilot/amazonq/…`), the `bootstrap.js` installer, the
`external-dependencies.yaml` manifest, and `catalog.yaml` live in the
**standalone [`stevengonsalvez/ainb-toolkit`](https://github.com/stevengonsalvez/ainb-toolkit)**
repo — flattened at its root (`skills/`, `agents/`, `workflows/`,
`utilities/`, `bin/generate-catalog.sh`). `ainb` consumes it as a pinned
external source. **To change a skill or agent, open a PR against
ainb-toolkit, not this repo.**

> **Legacy:** `bootstrap.js`, `external-dependencies.yaml`, and
> `scripts/update-externals.sh` (now in ainb-toolkit) were superseded in May
> 2026 by `ainb`. Migration path: clone ainb-toolkit, then
> `ainb migrate --from-bootstrap --toolkit-root ./ainb-toolkit`.

## Making a change

1. **Branch** from `main`.
2. **Atomic commits** — one concern per commit, conventional-commit prefix (`feat:`, `fix:`, `chore:`, `refactor:`, `docs:`).
3. **Don't bulk-commit** — if you've made multiple unrelated changes, split them with `git rebase -i` before pushing.
4. **No AI/Claude attribution** in commit messages. Write them as a human author.
5. **CI** runs on every PR (see `.github/workflows/toolkit-validation.yml`, "Skill Manager & Catalog CI"):
   - `validate-template-skill-refs` — clones the pinned `ainb-toolkit`, asserts every embedded agent-template skill ref resolves in its `skills/`, and sanity-checks the generated catalog-index pins `ainb-toolkit@<ref>`
   - `test-ainb-installations` — full `cargo test --workspace` across the ainb crates plus a smoke install with `AINB_USE_REAL_HOMES=1` against a tempdir `$HOME`
6. **Open a PR** against `main`.
7. **Merge with `--merge`** (not squash) so per-concern commit history is preserved.

## Adding a new skill

Skills live in the **[ainb-toolkit](https://github.com/stevengonsalvez/ainb-toolkit)**
repo, not here. Drop the skill under `skills/<name>/SKILL.md` there (and
`scripts/`, `assets/` if needed), regenerate the catalog with
`bash bin/generate-catalog.sh`, and open a PR against ainb-toolkit. Use
template placeholders:

- `{{HOME_TOOL_DIR}}` — interpolated to `~/.claude`, `~/.codex`, `~/.copilot` per tool
- `{{TOOL_DIR}}` — interpolated to `.claude`, `.codex`, `.copilot`

Never hardcode `~/.claude` — agent-agnostic skills must use placeholders. `ainb` rewrites placeholders per target tool's `template_substitutions()` map at apply time.

## Adding a new plugin

Claude Code plugins live at the repo root under `plugins/<name>/` (e.g.
`plugins/reflect/`):

1. Create `plugins/<name>/.claude-plugin/plugin.json`
2. Add the skills under `plugins/<name>/skills/`
3. Register in `.claude-plugin/marketplace.json` at repo root
4. Declare in your `~/.agents-in-a-box/manifest.yaml` under `units:` (or let `ainb skill install` populate it) so `ainb skill sync` will reconcile

## Reporting bugs / proposing features

Open a GitHub issue. For security-relevant issues see [SECURITY.md](./SECURITY.md).

## Code review

PRs are reviewed by humans + automated CI. Be patient — small fast PRs land faster than large ones.
