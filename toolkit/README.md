# Toolkit — Portable AI-Agent Skills & Configs

One canonical source. Many AI tools. Write a skill once, install it into Claude Code, Codex, Copilot, Gemini, Hermes, nanoclaw, Amazon Q, Cursor, Cline, Roo, or Clawdhub.

> **← [Back to main README](../README.md)**

---

## Quick start

```bash
cd toolkit
npm install

# Interactive (picks tool from menu)
node bootstrap.js

# Non-interactive (one tool)
node bootstrap.js --tool=claude-code-4.5
node bootstrap.js --tool=codex
node bootstrap.js --tool=copilot
node bootstrap.js --tool=gemini
node bootstrap.js --tool=hermes-agent
node bootstrap.js --tool=nanoclaw

# Verify everything landed (read-only)
node bootstrap.js --tool=claude-code-4.5 --verify

# Install external deps (git-cloned skills, npx-skills, claude plugins)
bash ~/.claude/setup-external.sh
```

**Repeat for each target tool.** Each tool gets its own home directory population (`~/.claude/`, `~/.codex/`, …). Hermes is special — it reads from `~/.claude/skills/` via `skills.external_dirs`, so installing for Claude transitively covers Hermes.

---

## What's in `packages/`

The canonical source tree. Everything under `packages/` gets distributed to tool home dirs on `bootstrap`.

```
packages/
├── skills/              86 skills — agent-invokable SKILL.md bundles
├── agents/              37 agents across 6 categories
│   ├── design/          UI/UX designers
│   ├── engineering/     backend, frontend, security, perf, code-review
│   ├── meta/            agentmaker (create new agents)
│   ├── orchestrators/   tech-lead, project-analyst, team-configurator
│   ├── swarm/           worker/leader primitives for multi-agent work
│   └── universal/       backend-developer, frontend-developer, superstar-engineer
├── utilities/
│   ├── config/          Shared tool config templates
│   ├── hooks/           Event hooks (session-start, pre-commit, etc.)
│   ├── output-styles/   Custom output formatters
│   ├── reflections/     Session reflection templates
│   └── utils/           Shell utility libraries
├── workflows/
│   └── single-agent/    Guided plan → implement → validate flows
├── plugins/
│   └── reflect/         Learning-capture plugin (reflect:capture, reflect:ingest)
└── knowledge/
    ├── docs-solutions-template/
    └── global-learnings-template/
```

### Skills at a glance (86 total, grouped by purpose)

<details>
<summary><b>Planning & Workflow</b> (14)</summary>

`plan` · `plan-gh` · `plan-tdd` · `implement` · `validate` · `workflow` · `discuss` · `brainstorm` · `interview` · `critique` · `research` · `handover` · `prime` · `recover-sessions`
</details>

<details>
<summary><b>Coding & GitHub</b> (12)</summary>

`coding-agent` · `spawn-agent` · `attach-agent-worktree` · `list-agent-worktrees` · `cleanup-agent-worktree` · `merge-agent-work` · `do-issues` · `gh-issue` · `make-github-issues` · `find-missing-tests` · `commit` · `sync-learnings`
</details>

<details>
<summary><b>Multi-agent / Swarm</b> (7)</summary>

`swarm-create` · `swarm-join` · `swarm-status` · `swarm-inbox` · `swarm-shutdown` · `swarm-orchestration` · `swarm-agent-troubleshooting`
</details>

<details>
<summary><b>Session & Learning</b> (9)</summary>

`session-info` · `session-metrics` · `session-summary` · `health-check` · `instincts` · `reflect` · `compound-docs` · `global-learnings` · `research-cache`
</details>

<details>
<summary><b>Testing</b> (5)</summary>

`expect-test` · `webapp-testing` · `browser-verify` · `test-driven-development` · `mobile-e2e-mcp`
</details>

<details>
<summary><b>Design & UI</b> (12)</summary>

`frontend-design` · `frontend-slides` · `liquid-glass` · `tui-style-guide` · `ui-ux-pro-max` · `react-components` · `stitch-design` · `stitch-loop` · `shadcn-ui` · `design-md` · `enhance-prompt` · `remotion` · `remotion-best-practices`
</details>

<details>
<summary><b>Dev infra & tooling</b> (10)</summary>

`tmux-monitor` · `tmux-status` · `start-local` · `start-ios` · `start-android` · `expose` · `plugins` · `oracle` · `debug-bridge` · `media-processing`
</details>

<details>
<summary><b>Security & Observability</b> (4)</summary>

`security-audit` · `security-scan` · `sentry-cli` · `posthog-replay-analysis`
</details>

<details>
<summary><b>Research & Knowledge</b> (3)</summary>

`crypto-research` · `nano-banana-pro` · `notebooklm`
</details>

<details>
<summary><b>Productivity</b> (5)</summary>

`resume-formatter` · `ats-resume-matcher` · `retro-pdf` · `skill-creator` · `token-usage`
</details>

<details>
<summary><b>Meta / autonomous patterns</b> (5)</summary>

`autonomous-loops` · `agent-ops` · `cost-aware-pipeline` · `scrapling-official` · `fireworks-tech-graph`
</details>

---

## External dependencies (`external-dependencies.yaml`)

Skills and plugins authored elsewhere, installed by bootstrap into each tool's home dir. Sections in the manifest:

| Section | Install mechanism | Target |
|---|---|---|
| `bundled-skills` | (included in `packages/skills/`) | copied by bootstrap |
| `agent-skills` | `git clone <repo>` | `~/.TOOL/skills/<name>/` |
| `claude-plugins` | `claude plugin marketplace install` | Claude plugin cache |
| `npx-skills` | `npx skills add <pkg> --yes` | `~/.agents/skills/` (then symlinked into tool dirs) |
| `nanoclaw-skills` | Synced from `stevengonsalvez/nanoclaw` container/skills/ | `~/.claude/skills/` (shared with claude-code-4.5) |
| `security-skills` | Installed into security-focused agents | tool-specific |
| `mcp-servers` | `claude mcp add` (user scope) | `~/.claude.json` |
| `mcporter-servers` | `mcporter.json` | `~/.mcporter/` |
| `marketplaces` | `claude plugin marketplace add` | Claude plugin cache |

### Key manifest flags

- **`applies-to: [claude, codex, copilot, gemini, hermes-agent, nanoclaw]`** — controls which tools install this entry
- **`catalog-only: true`** — listed for discoverability, never installed (useful for documenting alternatives)
- **`subpath: <dir>`** — repo has `SKILL.md` under a subdirectory; bootstrap extracts just that
- **`multi-subpath: <dir>`** — repo bundles multiple skills under `<dir>/<name>/SKILL.md`; bootstrap flattens each as a sibling install

### How bootstrap uses it

1. `node bootstrap.js --tool=X` — writes tool-specific files to `~/.TOOL/` and generates `~/.TOOL/setup-external.sh`
2. `bash ~/.TOOL/setup-external.sh` — executes the external-install commands (git clones, npx installs, plugin marketplace adds)
3. `--verify` — checks every applicable manifest entry has its `SKILL.md` at the expected path, reports per-tool parity and orphans

---

## Supported tools

Run `bootstrap.js` with `--tool=<X>`:

| Tool key | Home dir | Notes |
|---|---|---|
| `claude-code-4.5` | `~/.claude/` | Primary. Plugins + npx-skills + agent-skills |
| `codex` | `~/.codex/` | AGENTS.md symlinked to claude-code-4.5/CLAUDE.md |
| `copilot` | `~/.copilot/` | GitHub Copilot CLI |
| `gemini` | `~/.gemini/` | Gemini CLI, project-scoped |
| `hermes-agent` | `~/.hermes/` | Reads from `~/.claude/skills/` via `external_dirs` — no direct external installs needed |
| `nanoclaw` | `~/.claude/` (shared) | Successor to OpenClaw, uses same Claude dir |
| `amazonq` | `PROJECT/.amazonq/rules/` | Project-scoped |
| `cursor` `cline` `roo` | `PROJECT/.<tool>/rules/` | Project-scoped rule files |
| `clawdhub` | workspace/skills/ | Clawdhub skill format |

---

## Workflow: add a new skill end-to-end

1. **Author** — create `packages/skills/<name>/SKILL.md` with frontmatter `name`, `description`, and a matching body.
2. **Test locally** — copy to `~/.claude/skills/<name>/` and invoke via the skill harness. (Or just run `node bootstrap.js --tool=claude-code-4.5 --homeDir=$HOME` to reinstall.)
3. **Propagate** — `node bootstrap.js --tool=<each-target>` for every tool you care about. Or drive all of them in a loop.
4. **Verify** — `node bootstrap.js --tool=<X> --verify` should report the skill present on every tool in its applies-to.
5. **Commit** — canonical changes go in `toolkit/packages/skills/<name>/` (note: `.gitignore` has a broad `skills/` rule — use `git add -f` for new skill dirs).

For **externally-authored** skills (git clones), add an entry to `external-dependencies.yaml` under `agent-skills:` and set `applies-to` appropriately. Bootstrap will generate the `git clone` in each tool's `setup-external.sh`.

---

## Spec-Driven Development (SDD)

Spec Kit integration — drives a spec → plan → tasks workflow via slash commands in Claude Code.

```bash
# Install SDD assets into a project (clones Spec Kit to a temp folder)
node bootstrap.js --sdd --targetFolder=<project>

# Then in Claude Code within the project:
/specify "Your feature"
/plan
/tasks
```

- `SPEC_KIT_REPO` / `SPEC_KIT_REF` env vars point to a fork/branch
- Artifacts land under `specs/<feature-branch>/` (spec.md, plan.md, research.md, data-model.md, contracts/, quickstart.md, tasks.md)
- Works on any `^[0-9]{3}-` feature branch

---

## Rule registry (legacy, for non-frontmatter-aware tools)

For tools that can't parse frontmatter (Claude Desktop, Goose, etc.), bootstrap generates a `rule-registry.json` listing each rule's path, globs, and `alwaysApply` status.

```json
{
  "rulestore-rule": { "path": ".amazonq/rules/rulestore-rule.md", "globs": ["*-rule.md"], "alwaysApply": true }
}
```

Frontmatter-native tools (Cursor, AmazonQ, Claude Code) use the frontmatter directly and ignore the registry.

---

## References

- **Manifest**: [`external-dependencies.yaml`](external-dependencies.yaml)
- **Bootstrap source**: [`bootstrap.js`](bootstrap.js)
- **Parity regression test**: [`test-bootstrap-parity.sh`](test-bootstrap-parity.sh)
- **Main project README**: [`../README.md`](../README.md)
