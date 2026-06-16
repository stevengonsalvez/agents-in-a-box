---
name: reflect:cost
description: |
  Report reflect drain spend over a time window — tokens split by cached
  (cache_read), uncached writes (cache_creation), and io (input+output), with a
  $ estimate, grouped by day / outcome / model / transcript. Reads the drainer's
  cost log and surfaces outlier runs and cache-reuse health (the 41.5M-token
  failure mode = low cache reuse + high cache writes). Also reports the recall
  followup rate (the recall-quality self-monitor: how often a session searched
  again within 30s and got disjoint results). Use to answer "what is
  reflection costing me" for the last day / week, or "is recall satisfying".
version: "4.1.0"
user-invocable: true
triggers:
  - reflect:cost
  - reflect cost
  - reflection cost
  - cost of reflection
  - reflect spend
  - how much is reflect costing
allowed-tools:
  - Read
  - Bash
---

# /reflect:cost — Drain spend report

Shows what the reflect background drainer is spending: token volume split into
**cached** (cheap reuse), **uncached writes** (expensive cache_creation), and
**io** (input+output), plus a ballpark `$est`, grouped by day, outcome, model,
or transcript. This is the observability the 2026-05-31 incident lacked — one
drain run burned 41.5M tokens / ~$713 for zero net-new learnings and nothing
surfaced it until a manual backfill.

## When to Use

- "What did reflection cost me today / this week?"
- Spot an outlier run (a single transcript that blew up the spend).
- Check **cache reuse**: a healthy drain is cache_read-heavy; a low cache-reuse
  % with high cache_creation is the costly failure mode.
- Confirm the drain is running on the cheap model (should be `sonnet`, not
  `opus`) after a deploy.

## Window argument

Parse the window from the user's request and pass it as `--since`:

| User says | `--since` |
|-----------|-----------|
| "1 day", "today", "last 24h", (default) | `1d` |
| "this week", "last 7 days" | `7d` |
| "last hour" | `1h` |
| "last month", "30 days" | `30d` |

Default to `1d` when unspecified.

## Run it

Locate the cost reporter (prefer the running plugin, else the newest cached
version), then render. The script is `reflect_cost.py`, shipped in the reflect
plugin's `scripts/`.

```bash
WINDOW="1d"   # ← substitute from the table above

# Resolve reflect_cost.py robustly across deploy layouts.
COST_PY=""
for cand in \
  "${CLAUDE_PLUGIN_ROOT:-}/scripts/reflect_cost.py" \
  $(ls -t "$HOME"/.claude/plugins/cache/agents-in-a-box/reflect/*/scripts/reflect_cost.py 2>/dev/null); do
  if [ -n "$cand" ] && [ -f "$cand" ]; then COST_PY="$cand"; break; fi
done
if [ -z "$COST_PY" ]; then
  echo "reflect_cost.py not found — install/update the reflect plugin (v4+):"
  echo "  claude plugin update reflect@agents-in-a-box"
  exit 1
fi

# Headline: by outcome (where the spend goes), then model (cheap vs expensive),
# then the top transcripts (find the outlier).
python3 "$COST_PY" --since "$WINDOW" --by outcome
echo
python3 "$COST_PY" --since "$WINDOW" --by model
echo
python3 "$COST_PY" --since "$WINDOW" --by transcript --top 10
echo
# Writer health (M2): each run's output classification — a healthy drain is
# all `valid`; prose/idle/poisoned/malformed rows mean the writer is drifting
# (3 consecutive invalids poison the transcript as writer_drift).
python3 "$COST_PY" --since "$WINDOW" --by writer
echo
# Recall quality (A4): the followup-rate diagnostic. A "followup" = the same
# session searched again within the window (default 30s) with a DIFFERENT
# query and got a fully DISJOINT result set — i.e. the first recall didn't
# satisfy. High rate = tune rerank weights / graph arm budget / OOD threshold.
python3 "$COST_PY" --since "$WINDOW" --followup
echo
# Subscription quota (M3): per-window rate-limit snapshot the drainer ingested
# from its own claude -p runs, plus whether the writer gate is open or closed.
# A CLOSED gate means drains are deferred (reason=quota_near_limit) — queue
# entries are retained and replay automatically once the quota recovers.
python3 "$COST_PY" --quota
```

For a machine-readable view, add `--json`.

## Reading the followup rate

```
recall followup rate — last 7d

  searches tracked : 42
  followups        : 6 (re-search within 30s, disjoint results)
  followup rate    : 14%
```

- **searches tracked** — session-anchored, non-empty recalls (SessionStart's
  synthetic boot queries are excluded; empty result sets are knowledge gaps,
  not followups, and don't count).
- **followup rate** — the recall-quality self-monitor. Near 0% = first
  results are landing. Sustained high (>30%) = recall isn't satisfying on
  the first ask: consider rerank-weight tuning, more graph-arm budget, or a
  lower OOD threshold. The signal is *directional* — rapid legitimate topic
  switches inside 30s overcount.
- Window is tunable via `RECALL_FOLLOWUP_WINDOW_SECONDS` (default 30).

## If tokens show 0 (historical / pre-v4 events)

The drainer only began recording the full token envelope in v4. Older cost
events carry only `outcome` (no tokens/model), so the split shows `0`.
Reconstruct the real numbers from the raw session logs with the backfill — it
scans `~/.claude/projects`, finds the reflect runs, and sums their usage into a
separate `drain-cost-backfill.jsonl` (which `reflect_cost.py` reads alongside
the live log):

```bash
BACKFILL_PY="$(dirname "$COST_PY")/backfill_costs.py"
python3 "$BACKFILL_PY" --since "$WINDOW" \
  --projects-dir "$HOME/.claude/projects" \
  --state-dir "${REFLECT_STATE_DIR:-$HOME/.reflect}"
# then re-run the reflect_cost.py commands above
```

## Reading the output

```
outcome                 runs    tokens  cache_rd  cache_wr      io     $est
ok                        48      120M       95M       18M     7.0M   220.40
partial_max_turns          3       40M       30M        8M     2.0M    70.10
```

- **cache_rd** (cache_read) — cheap reuse, *should* dominate.
- **cache_wr** (cache_creation) — expensive writes; high here means caching is
  not amortizing (re-paying to rebuild context). The incident was cache_wr-heavy.
- **io** — input + output tokens.
- **$est** — authoritative `cost_usd` from `claude -p` where recorded, else an
  order-of-magnitude estimate from token buckets × ballpark list prices. Not a bill.
- **⚠ outlier** — any single run above `--outlier-tokens` (default 5M). One
  flagged transcript is usually where a spend spike lives.
- The **cache-reuse %** line at the bottom is the fastest health read: low % +
  high cache_wr = the 41.5M failure mode.

Then summarize for the user in one or two lines: total tokens, the cached vs
uncached split, the model, the $est, and call out any outlier transcript.

## Troubleshooting

- **"reflect_cost.py not found"** — the installed plugin predates v4. Run
  `claude plugin update reflect@agents-in-a-box` and restart.
- **All rows show model `?` and 0 tokens** — pre-v4 events only; run the
  backfill above.
- **No events at all** — the drain hasn't run in the window, or
  `REFLECT_STATE_DIR` points elsewhere (`ls "${REFLECT_STATE_DIR:-$HOME/.reflect}"`).
