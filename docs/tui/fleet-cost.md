---
title: "ainb fleet cost"
description: "Per-session / model / day / group spend rollups + budget caps for an ainb fleet."
---

`ainb fleet cost` surfaces ainb's existing cost tracking as fleet-shaped
spend rollups, plus configurable budget caps that fire notifyd alerts.

ainb already prices every provider call — `cost_usd` is recorded on each
`ProviderCall` / `TokenBucket` and aggregated by the **burndown** plugin
(the same data behind `ainb usage`). This verb does **not** re-price
anything: it fetches burndown's `usage report --format json` payload live
(over the same plugin runtime + retry path `ainb usage` uses), reshapes it
into per-session / per-model / per-day / per-group rollups joined to the
live fleet, evaluates configured budget caps, and delivers a notifyd alert
for every breach.

## Usage

```bash
ainb fleet cost                                  # text tables (default)
ainb --format json fleet cost                    # JSON
ainb --format json fleet cost --period week      # today | week | 30days | month | all
```

`--format` is a global flag — it precedes `fleet`. `--period` (default
`month`) scopes the reporting window: the burndown plugin date-bounds its
call set and re-aggregates every rollup, so a narrower period returns less
spend. `all` reports lifetime totals.

## JSON shape

```jsonc
{
  "totals":  { "cost_usd": 6.0, "bucket": { /* tokens */ }, "session_count": 2, "model_count": 2 },
  "sessions": [ { "session_id", "provider", "project", "cwd?", "group?", "cost_usd", "bucket" } ],
  "models":   [ { "model", "cost_usd", "bucket" } ],
  "daily":    [ { "date", "cost_usd", "bucket" } ],
  "groups":   [ { "group", "cost_usd", "session_count", "bucket" } ],
  "budget_breaches": [ { "scope", "subject", "cwd?", "cost_usd", "limit_usd" } ]
}
```

`sessions` and `models` are sorted by descending cost. `groups` contains
only sessions whose resolved `cwd` matches a session in the live fleet
(joined via `workspace_name`). `bucket` is the token breakdown
(`input_tokens`, `cache_creation_tokens`, `cache_read_tokens`,
`output_tokens`, `reasoning_tokens`, `call_count`, `cost_usd`).

## Budget caps

Spend ceilings live in `config.toml` under `[fleet.cost]`. Project config
(`./.agents-box/config.toml`) overrides user config
(`~/.agents-in-a-box/config/config.toml`).

```toml
[fleet.cost]
session_usd = 5.0      # any single session over $5 → alert
group_usd = 25.0       # any workspace group over $25 → alert

[fleet.cost.session_overrides]
"abc123" = 50.0        # a specific session's own ceiling

[fleet.cost.group_overrides]
"infra" = 100.0        # a specific group's own ceiling
```

The override maps take precedence over the blanket `session_usd` /
`group_usd`. With no caps configured, no breaches are produced.

### Alert delivery

A breach is delivered through the existing **notifyd** substrate: a valid
`Envelope` is written to `~/.agents-in-a-box/notify.sock` with raw event
`Notification:budget_exceeded`. It classifies as `AlertKind::WaitingOnUser`,
passes notifyd's user-facing filter, and lands as a row in
`notifications.db` — the same path idle/permission prompts take. No new
alert kind is introduced. Delivery is best-effort: if the socket is
unreachable the report still prints, with a warning on stderr.

## Environment variables

| Var | Default | Effect |
|---|---|---|
| `AINB_USAGE_TIMEOUT_SECS` | `120` | Budget for the first (cold-cache) burndown scan. |
| `AINB_USAGE_TRACE` | unset | Emit plugin-dispatch trace lines on stderr. |

## Notes

- The first call on a cold cache can take ~40s while session-reader scans
  `~/.claude/projects`; subsequent calls hit the cache and return sub-second.
- Requires the burndown plugin (`ainb plugin install burndown`), the same
  dependency `ainb usage` has.
- To change pricing or the spend plan, use `ainb usage plan ...` — this verb
  consumes ainb's pricing, it does not own it.
