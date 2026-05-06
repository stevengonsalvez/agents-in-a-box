# general-rules

**Cross-tool source-of-truth rules.** Generic guidance shared across every supported AI coding assistant (Cursor, Cline, Roo, Copilot, Amazon Q, Claude, Codex). When a user runs `node bootstrap.js`, these rules get deployed into the per-tool target directory in addition to the tool-specific rule from `../<tool>/`.

| Rule | Purpose |
|------|---------|
| `code-composition-rule.md` | DRY, separation of concerns, encapsulation defaults |
| `dependency-version-check-rule.md` | Verify package versions before importing/using |
| `environment-variables-rule.md` | Reading + validating env vars; never log secrets |
| `golang-consolidated-rule.md` | Go style + project conventions |
| `langraph-consolidated-rule.md` | LangGraph workflow patterns |
| `mcp-server-creation-consolidated-rule.md` | MCP server scaffolding + best practices |
| `postman-security-rule.md` | API testing + secret-handling in Postman |
| `jiraprocessing-rule.md` | Triaging + working from Jira tickets |
| `fep-docs-rule.md` | Internal frontend docs handling |
| `no-playwright-show-report-rule.mdc` | Don't run `playwright show-report` (blocks the agent) |

## Adding a new rule

1. Drop a `*.md` (or `*.mdc`) file here.
2. Use cross-tool language — don't reference Cursor-only or Cline-only behaviours.
3. Test that `node bootstrap.js` picks it up by selecting any tool — the rule should land in the target project.

## Why a separate dir

Per-tool dirs (`../cursor/`, `../cline/`, etc.) hold ONE rule each — the meta-rule that tells the tool how to use the rest. Everything else lives here so we maintain it in one place.
