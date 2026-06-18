# OpenTelemetry → Grafana Cloud

Ship Claude Code (and Codex) metrics, logs, and traces to Grafana Cloud via a
local [Grafana Alloy](https://grafana.com/docs/alloy/) collector. Wired into
both the `ainb` CLI and the first-run TUI onboarding wizard.

```
┌────────────┐  OTLP            ┌──────────────┐            ┌──────────────┐
│ Claude Code│ ──:4318/:4317──▶ │ Grafana Alloy│ ──OTLP───▶ │ Grafana Cloud│
│  (+ Codex) │                  │  (local tmux)│  + auth    │              │
└────────────┘                  └──────────────┘            └──────────────┘
```

## What you need

| Thing | Why | How to get it |
|-------|-----|---------------|
| Grafana Cloud account | Destination for telemetry | Free tier at grafana.com |
| OTLP endpoint URL | Where Alloy ships data | Grafana Cloud → **Connections → OpenTelemetry (OTLP)** (ends in `/otlp`) |
| Instance ID | Basic-auth username | Same OTLP page ("Instance ID") |
| API token | Basic-auth password | Same page → generate a token with **metrics + logs + traces** write scope |
| `alloy` binary | The local collector | `brew install grafana/grafana/alloy` (the CLI offers to install it) |

> The three Grafana values come from one screen: **Connections → OpenTelemetry
> (OTLP)** in your Grafana Cloud stack. The "Instance ID" there is the Basic-auth
> username — it is NOT your org id.

## Setup (CLI)

```bash
ainb otel setup            # interactive, full-auto
ainb otel status           # check the local pipeline
ainb otel start            # (re)start Alloy in its tmux session
```

`ainb otel setup` will:

1. Write the Alloy config + launcher + dashboards to `~/.agents-in-a-box/otel/`.
2. Prompt for the three Grafana Cloud values (or read them from
   `GRAFANA_OTLP_ENDPOINT` / `GRAFANA_INSTANCE_ID` / `GRAFANA_API_TOKEN` env
   vars if set).
3. Write `~/.agents-in-a-box/otel/grafana-cloud.env` (mode `0600` — secrets).
4. Ensure the generic, non-secret OTEL keys are in `~/.claude/settings.json`.
5. Wire your shell rc to `source` the env file (backed up to `<rc>.bak`).
6. Offer to `brew install` Alloy if missing, then start it in tmux.

## Setup (TUI)

The onboarding wizard has an optional **Telemetry** step. Fill the three fields
(Tab to move between them) and press Enter to set up, or press Enter with the
fields empty to skip. Re-run later any time with `ainb otel setup`.

The TUI does not `brew install` Alloy — if it's missing the config is still
written; finish with `ainb otel setup` or `brew install grafana/grafana/alloy`
then `ainb otel start`.

## Where things live (and why)

| File | Contents | Secret? |
|------|----------|---------|
| `~/.claude/settings.json` (`env`) | Generic OTEL flags (exporter/protocol/intervals/log toggles) | **No** — this file syncs to a public repo, so nothing machine-specific or secret goes here |
| `~/.agents-in-a-box/otel/grafana-cloud.env` | `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_RESOURCE_ATTRIBUTES` (host.name), `GRAFANA_*` creds | **Yes** — `0600`, never committed, sourced by your shell + read by Alloy |
| `~/.agents-in-a-box/otel/config.alloy` | Alloy OTLP fan-in pipeline (Delta→Cumulative for Mimir) | No |
| `~/.agents-in-a-box/otel/dashboards/*.json` | Grafana dashboards to import (Claude Code + Codex) | No |

Shell env wins over the `settings.json` env block, so the machine-specific
values in `grafana-cloud.env` are authoritative per machine.

## Verify

```bash
ainb otel status        # env file / config / alloy install + tmux session
ainb doctor             # alloy shows under the "otel" consumer group
```

Alloy's local UI/health is at <http://127.0.0.1:12345>. Import the dashboards
under `~/.agents-in-a-box/otel/dashboards/` into your Grafana Cloud stack to see
the data.
