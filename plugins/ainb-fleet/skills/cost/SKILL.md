---
name: ainb-fleet:cost
description: |
  Show fleet spend — per-session, per-model, per-day, and per-group USD
  cost rollups for every claude/codex session, sourced live from ainb's
  burndown analytics (which already prices every provider call). Use when
  you need spend visibility across a multi-session fleet, want to find the
  most expensive session/model, or are checking whether any session/group
  has crossed a configured budget cap. Budget breaches are also delivered
  as notifyd alerts.
  Default output: text tables. Pass --format json for LLM consumption.
version: "0.1.0"
user-invocable: true
triggers:
  - ainb-fleet:cost
  - fleet cost
  - fleet spend
  - how much have my sessions cost
  - which session is most expensive
  - fleet budget
allowed-tools:
  - Bash
---

# ainb fleet:cost

Fleet-shaped spend rollups. ainb already tracks `cost_usd` per provider call
(the burndown plugin aggregates it); this verb reshapes that data into
per-session / per-model / per-day / per-group USD totals joined to the live
fleet, and evaluates configured budget caps.

## Run

```bash
ainb fleet cost                                  # text tables (default)
ainb --format json fleet cost                    # JSON for piping
ainb --format json fleet cost --period week      # window: today|week|30days|month|all
```

`--format` is a **global** flag — it goes before `fleet`, not after `cost`.
The `--period` window is passed through to the burndown plugin (default
`month`).

## Output fields (JSON)

| field | meaning |
|---|---|
| `totals` | fleet-wide `{cost_usd, bucket, session_count, model_count}` |
| `sessions[]` | per-session `{session_id, provider, project, cwd?, group?, cost_usd, bucket}`, sorted by descending cost |
| `models[]` | per-model `{model, cost_usd, bucket}`, sorted by descending cost |
| `daily[]` | per-day `{date, cost_usd, bucket}` |
| `groups[]` | per-workspace `{group, cost_usd, session_count, bucket}` (only sessions that match a live fleet cwd) |
| `budget_breaches[]` | `{scope, subject, cwd?, cost_usd, limit_usd}` — caps crossed this run (also delivered to notifyd) |

`bucket` is the token breakdown: `input_tokens`, `cache_creation_tokens`,
`cache_read_tokens`, `output_tokens`, `reasoning_tokens`, `call_count`,
`cost_usd`.

## Budget caps

Configure spend ceilings in `config.toml` (project config overrides user
config). A breach fires a notifyd alert (`Notification:budget_exceeded`,
surfacing as `WaitingOnUser`) and lands in `notifications.db`.

```toml
[fleet.cost]
session_usd = 5.0      # warn when any single session crosses $5
group_usd = 25.0       # warn when any workspace group crosses $25

[fleet.cost.session_overrides]
"abc123" = 50.0        # a long-running session gets its own ceiling

[fleet.cost.group_overrides]
"infra" = 100.0        # the infra workspace gets a higher ceiling
```

Override maps take precedence over the blanket `session_usd` / `group_usd`.
With no caps configured, `budget_breaches` is always empty.

## Composition patterns

```bash
# Most expensive session
ainb --format json fleet cost | jq '.sessions[0]'

# Total spend this month
ainb --format json fleet cost | jq '.totals.cost_usd'

# Any budget breaches right now?
ainb --format json fleet cost | jq '.budget_breaches'

# Spend by group, biggest first
ainb --format json fleet cost | jq '.groups[] | {group, cost_usd}'
```

## Caveats

- Cost data is sourced live from the burndown plugin. The first call on a
  cold cache can take ~40s while session-reader scans `~/.claude/projects`;
  raise the budget with `AINB_USAGE_TIMEOUT_SECS=<n>` for very large
  archives. Subsequent calls hit the cache and return in well under a second.
- The `group` on a session is only populated when its resolved `cwd` matches
  a session in the live fleet (via `workspace_name`). Sessions that have
  ended drop out of the group rollup but still appear in `sessions[]`.
- Budget alert delivery is best-effort: if notifyd's socket is unreachable
  the command still prints the report (with a warning on stderr).
- This verb never re-prices anything — it relies entirely on ainb's existing
  pricing. To change pricing or plan, use `ainb usage plan ...`.
