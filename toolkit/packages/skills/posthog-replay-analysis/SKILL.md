---
name: posthog-replay-analysis
description: Decode and analyse PostHog session replay recordings programmatically via the API. Use when investigating a user-reported incident (crash, infinite loop, unexpected navigation) where a PostHog session replay exists, and you want to extract the URL sequence, network request timeline, console logs, or DOM state WITHOUT opening the replay UI. Especially valuable when diagnosing iOS Safari/WebKit crashes ("A problem repeatedly occurred"), request storms, or any bug where the replay UI can't be scripted. Bypasses the broken `rrvideo` npm packages with a direct decode-and-chart approach.
license: MIT
---

# PostHog Replay Analysis Skill

PostHog session replays are **not video files** — they are [rrweb](https://www.rrweb.io/) event streams (JSON). This skill is the decode recipe for pulling useful diagnostic data out of them from the command line.

## When to Use

- User reports a page crash / blank screen / reload loop and PostHog has a recording
- You need the **network timeline** on a specific page (request storm diagnosis)
- You need the **DOM state** at the moment the page appeared broken
- You need the **console logs** around a crash (PostHog `$exception` capture may be off)
- You need to **correlate** page loads across a watchdog-kill sequence (iOS Safari)

**Do NOT use for:** generic media conversion (see `media-processing`), or when you only need a human-readable replay (just use the PostHog UI).

## Do Not Waste Time On

- `@rrweb/rrvideo` npm package — **404 not found**.
- `rrvideo@0.2.1` unscoped — ships deprecated Puppeteer 5 and silently produces **no output file with no error**. You'll `ls *.mp4` and find nothing.
- Fetching a blob with only `blob_key=N` — API v2 returns `{"type":"validation_error","code":"invalid_input","detail":"Must provide both start blob key and end blob key"}`. You must pass **both** `start_blob_key` and `end_blob_key`.

## Prerequisites

You need three things from the target project:

| What | Where to find it |
|---|---|
| PostHog personal API key | PostHog UI → Settings → Personal API keys. **Must** have `session_recording:read` scope. Also frequently stored in repo `.env*` files under `POSTHOG_PERSONAL_API_KEY` or similar — check there first. |
| Project ID (numeric) | PostHog UI → Settings → Project ID. NOT the project name. |
| Region host | `https://eu.posthog.com` or `https://us.posthog.com` — check the URL when you're logged into the PostHog UI. |

```bash
pip install --quiet matplotlib   # only if you want the chart at the end
```

If the project has a config skill (e.g. `shot-debug-auth`, `incident-investigate`, or similar), check it first — common IDs and token paths may already be documented there.

Export for convenience (substitute your own values):

```bash
export PH_HOST="https://eu.posthog.com"          # or us.posthog.com
export PH_PROJECT_ID="<your-project-id>"          # numeric
export PH_TOKEN="phx_…"                            # personal API key
export PH_RECORDING_ID="<recording-uuid>"          # from a replay URL
```

The recording ID is the last path segment in a PostHog replay URL: `…/project/{id}/replay/{recording-id}`.

## Step 1 — Confirm the Recording Exists

```bash
curl -sS -H "Authorization: Bearer $PH_TOKEN" \
  "$PH_HOST/api/projects/$PH_PROJECT_ID/session_recordings/$PH_RECORDING_ID/" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); print({k:d[k] for k in ['distinct_id','recording_duration','start_time','end_time','click_count','console_warn_count','console_error_count','start_url'] if k in d})"
```

Useful signals in the metadata:
- `console_warn_count` / `console_error_count` — quick smell test
- `click_count` vs `mouse_activity_count` — if clicks are very low but mouse activity high, user was stuck
- `recording_duration` vs `active_seconds` — big gap means the tab was dead for chunks of the session

## Step 2 — List Snapshot Sources (Find the Gaps)

```bash
curl -sS -H "Authorization: Bearer $PH_TOKEN" \
  "$PH_HOST/api/projects/$PH_PROJECT_ID/session_recordings/$PH_RECORDING_ID/snapshots/" \
  | python3 -m json.tool
```

Each entry has `blob_key`, `start_timestamp`, `end_timestamp`. **Time gaps between consecutive blobs are direct evidence of renderer death** — rrweb can't emit while the tab is killed. A 10–60s gap on a short session is the fingerprint of an iOS Safari WebKit process kill.

## Step 3 — Fetch the Full Blob Range

```bash
N=$(curl -sS -H "Authorization: Bearer $PH_TOKEN" \
  "$PH_HOST/api/projects/$PH_PROJECT_ID/session_recordings/$PH_RECORDING_ID/snapshots/" \
  | python3 -c "import json,sys; print(len(json.load(sys.stdin)['sources'])-1)")

curl -sS -H "Authorization: Bearer $PH_TOKEN" \
  "$PH_HOST/api/projects/$PH_PROJECT_ID/session_recordings/$PH_RECORDING_ID/snapshots/?source=blob_v2&start_blob_key=0&end_blob_key=$N" \
  -o blobs.raw
```

Output is **NDJSON**: each line is `[session_id, event_object]`.

## Step 4 — Decode the Events

Events with `cv:"2024-10"` have a **gzip-compressed** `data` field. The payload is a latin-1 encoded binary string — decompress it in Python:

```python
import json, gzip
events = []
with open('blobs.raw','rb') as f:
    for line in f:
        try: _sid, ev = json.loads(line)
        except: continue
        if ev.get('cv') == '2024-10' and isinstance(ev.get('data'), str):
            raw = gzip.decompress(ev['data'].encode('latin-1')).decode('utf-8')
            ev['data'] = json.loads(raw)
            ev.pop('cv', None)
        events.append(ev)
events.sort(key=lambda e: e.get('timestamp', 0))
json.dump(events, open('events.json','w'))
```

## Step 5 — rrweb Event Taxonomy

Filter by `type`:

| type | meaning | use for |
|---|---|---|
| `2` | **FullSnapshot** — complete DOM | DOM state at crash (search for spinner / error text / specific components) |
| `3` | IncrementalSnapshot — DOM mutation | detailed DOM diffs (rarely needed for quick triage) |
| `4` | **Meta** — new page load with `href` + viewport | **page-load sequence**: if you see 4 Meta events on the same URL in 90s, the tab is reload-looping |
| `5` | Custom | app-specific events |
| `6` | **Plugin** — see `data.plugin` | network + console (the good stuff) |

Plugin sub-types in `data.plugin`:
- `rrweb/network@1` — every fetch/XHR/img. `data.payload.requests[]` has `name`, `duration`, `initiatorType`, `decodedBodySize`.
- `rrweb/console@1` — `data.payload.level` + `payload.payload` (yes, nested).

## Step 6 — Useful Extraction Recipes

### URL sequence + console warnings

```python
for ev in events:
    t = ev.get('type')
    ts = ev.get('timestamp')
    d = ev.get('data')
    if t == 4:
        print(f"PAGE LOAD {ts}  {d['href']}  {d['width']}x{d['height']}")
    if t == 6 and isinstance(d, dict):
        p = d.get('payload', {})
        if p.get('level'):
            print(f"CONSOLE   {ts}  [{p['level']}] {p.get('payload')}")
```

### Network timeline bucketed by second

```python
from collections import defaultdict
buckets = defaultdict(int)
start = min(e['timestamp'] for e in events if e.get('timestamp'))
for ev in events:
    if ev.get('type') != 6: continue
    for r in (ev.get('data',{}).get('payload',{}).get('requests') or []):
        sec = (ev['timestamp'] - start) // 1000
        buckets[sec] += 1
for s in sorted(buckets): print(f"{s:>3}s  {'█'*buckets[s]}  ({buckets[s]})")
```

Look for:
- **Sustained >50 req/sec** = fan-out storm (likely N+1)
- **Flat zero for >5s mid-session** = tab dead (watchdog kill / network offline)
- **Repeated identical URLs** = missing dedup (e.g. staleTime=0 + many consumers)

### Top repeated endpoints (N+1 detector)

```python
from collections import Counter
urls = Counter()
for ev in events:
    if ev.get('type') != 6: continue
    for r in (ev.get('data',{}).get('payload',{}).get('requests') or []):
        u = (r.get('name') or '').split('?')[0]
        urls[u] += 1
for u, c in urls.most_common(15):
    print(f"{c:>4}  {u[:120]}")
```

> Rule of thumb: if a single URL appears >20× in a short session, it's either a runaway polling loop or an N+1 over a parent list.

### DOM state at the crash moment

```python
for ev in events:
    if ev.get('type') != 2: continue     # FullSnapshot
    data = ev.get('data')
    if isinstance(data, dict):
        text = json.dumps(data)
        print(f"Snapshot @ {ev['timestamp']}  size={len(text)}")
        # Substring search is cheaper than parsing the rrweb DOM tree
        for needle in ['Loading', 'spinner', 'Error', 'Something went wrong']:
            if needle in text:
                print(f"  contains: {needle!r}")
```

## Step 7 — Render a Timeline Chart

Combine the per-second buckets with matplotlib + the Meta events (vertical green lines) and red axvspans for dead zones. See `media-processing` for the ffmpeg/ImageMagick recipes to stitch the chart into a composite image or slideshow mp4 afterwards.

```python
import matplotlib; matplotlib.use('Agg')
import matplotlib.pyplot as plt
fig, ax = plt.subplots(figsize=(16,5))
xs = sorted(buckets)
ax.bar(xs, [buckets[s] for s in xs], color='#ff6b6b')
for ev in events:
    if ev.get('type') == 4:
        ax.axvline((ev['timestamp']-start)/1000, color='green', ls='--', lw=1.5)
# Hand-mark dead zones from Step 2 gaps:
# ax.axvspan(14, 24, color='red', alpha=0.12)
ax.set(xlabel='seconds', ylabel='requests/s', title='Session network timeline')
fig.savefig('timeline.png', dpi=140, bbox_inches='tight')
```

## Canonical Incident Report

For a complete write-up, attach:
1. The timeline chart (`timeline.png`)
2. A side-by-side composite of the crash screenshot + chart (see `media-processing`: *Incident Evidence Composite*)
3. Raw metadata numbers (request totals, distinct URLs, page load count, dead-zone durations)
4. The original replay URL: `$PH_HOST/project/$PH_PROJECT_ID/replay/$PH_RECORDING_ID`

## Related

- **`media-processing`** — turn chart/screenshot PNGs into mp4 slideshows and side-by-side composites for sharing in PRs or incident reports
- **`sentry-query`** *(if available)* — pair with Sentry to correlate replay timeline against JS stacktraces; PostHog typically strips PII while Sentry has user IDs
- **Project-level auth/debug skills** *(if available)* — e.g. a project may ship its own `<project>-debug-auth` or `incident-investigate` skill that wraps this one with known project IDs, env paths, and distinct_id → user lookups. Prefer those when investigating incidents in that project.
