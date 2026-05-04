---
name: token-usage
description: 'Show Claude Code token usage across sessions — daily, weekly, per-project, and per-session breakdowns. Parses ~/.claude/projects/**/*.jsonl for consumption data. Use when the user asks about token usage, costs, how many tokens were used, session statistics, or wants a usage report.'
user-invocable: true
argument-hint: "[--days N] [--since YYYY-MM-DD] [--project NAME]"
---

# Token Usage

Analyze Claude Code token consumption across all sessions.

## How to Run

**IMPORTANT: Always use `--format markdown` and display the full output directly to the user as markdown tables. Do NOT summarize or truncate.**

Run the script and display the FULL output as-is:

```bash
python3 ~/.claude/skills/token-usage/scripts/token_usage.py --format markdown [ARGS]
```

Pass through any user-provided arguments (--days, --since, --project, --top-sessions).

### Examples

```bash
# All time usage
python3 ~/.claude/skills/token-usage/scripts/token_usage.py --format markdown

# Last 7 days
python3 ~/.claude/skills/token-usage/scripts/token_usage.py --days 7 --format markdown

# Since a specific date
python3 ~/.claude/skills/token-usage/scripts/token_usage.py --since 2026-04-01 --format markdown

# Filter to a specific project
python3 ~/.claude/skills/token-usage/scripts/token_usage.py --project shotclubhouse --format markdown

# Top 20 costliest sessions
python3 ~/.claude/skills/token-usage/scripts/token_usage.py --days 30 --top-sessions 20 --format markdown

# JSON output (for piping to jq)
python3 ~/.claude/skills/token-usage/scripts/token_usage.py --format json | jq '.grand_total'
```

## Display Instructions

After running the script:
1. **Show the ENTIRE markdown output** directly in your response — do not hide it behind a Bash tool call
2. The output is already formatted as markdown tables — just paste it verbatim
3. Do NOT add your own summary or interpretation unless the user asks for one
4. If the output is long, show all of it — the user wants to see the full report

## CLI Flags

| Flag | Description | Default |
|------|-------------|---------|
| `--days N` | Only include last N days | All time |
| `--since YYYY-MM-DD` | Only include since this date | All time |
| `--project NAME` | Filter to project (substring match) | All projects |
| `--top-sessions N` | Number of top sessions to show | 10 |
| `--format text\|markdown\|json` | Output format | text |
| `--projects-dir PATH` | Override projects directory | `~/.claude/projects` |

## Data Source

Parses `~/.claude/projects/**/*.jsonl` — Claude Code's session transcript files. Each `assistant` message contains a `usage` block with `input_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`, and `output_tokens`.

## Integration with ainb-tui

The ainb-tui has a built-in Usage Analytics screen (press `i` from home screen or select "Stats" in the sidebar) that provides the same data with a visual bar chart and provider selector. This skill is the CLI-only alternative.

## Notes

- Token counts are RAW — they don't map directly to cost since cache reads are heavily discounted and different models have different pricing
- The script is pure Python 3 with no external dependencies (uses only stdlib)
- For cost estimates, multiply by model-specific per-token pricing from Anthropic's pricing page
