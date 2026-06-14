# ainb phone bridge (Telegram)

A standalone Python daemon that relays messages **two-way** between Telegram and a
named `ainb` session — a conductor session when one is present, any running
session otherwise. Drive and observe a coding-agent session from your phone while
away from the keyboard.

It is the Telegram-first port of agent-deck's proven `bridge.py`, re-pointed at
ainb's existing fleet transport instead of agent-deck's CLI.

```
   Telegram  ──long-poll──▶  bridge daemon  ──tmux send-keys──▶  ainb session
      ▲                          │                                    │
      └────── HTML reply ────────┴──── read JSONL transcript ◀────────┘
                                    (wait for assistant end_turn)
```

---

## How it talks to ainb

The bridge reuses ainb's **existing, verified** mechanisms — it invents no new
transport:

| Concern | Mechanism |
|---|---|
| Discover sessions | `ainb list --format json` → running `workspace_name` / `tmux_session_name` / `worktree_path` |
| Send a message | `tmux send-keys -t <session> -l <text>` then `Enter` (literal mode, injection-safe — the same path `ainb fleet broadcast` uses) |
| Capture the reply | Read the session's Claude JSONL transcript under `~/.claude/projects/<cwd-slug>/*.jsonl` |

### Response-capture contract (decision)

ainb has **no** `session send --wait` primitive like agent-deck, so the bridge
cannot return a blocking-send's stdout. The reliable equivalent is the JSONL
transcript that ainb-fleet's `sequence` verb already watches
(`wait_for_turn_end`). The bridge mirrors that contract:

1. Snapshot the transcript's current byte offset.
2. Send the message via `tmux send-keys`.
3. Poll the transcript from that offset until an `assistant` row with
   `message.stop_reason == "end_turn"` appears, then return the concatenated
   `text` blocks of that turn (or time out after `response_timeout`).

This is the semantic match for agent-deck's **template** contract
(`session output --json` → `data["content"]`), adapted to ainb's JSONL — chosen
over the standalone-bridge raw-`--wait`-stdout contract because ainb's send is
fire-and-forget tmux, with no stdout to capture.

The `cwd → project-slug` mapping (every non-`[A-Za-z0-9-]` char → `-`) matches
the Rust `cwd_to_project_slug` in `fleet/read/jsonl_tail.rs` exactly.

---

## Routing

- **`name: message`** — when `name` matches a running session (case-insensitive),
  the message goes to that session.
- **bare `message`** — goes to the default target: a conductor session if one is
  running, otherwise the alphabetically-first running session (so the bridge is
  useful **before** the separate conductor track lands).
- A leading token that doesn't match any running session (e.g. `note: buy milk`)
  is treated as plain text, not a route.

---

## Config

Merge a `[fleet.telegram]` table into ainb's config at
`~/.agents-in-a-box/config/config.toml` (see `config.example.toml`):

```toml
[fleet.telegram]
token = "$TELEGRAM_BOT_TOKEN"      # secret ref — never a literal in prod
user_id = 123456789               # authorized Telegram user id
# default_target = "conductor"    # optional preferred target for bare messages
require_mention_in_groups = true  # optional, default true
response_timeout = 300            # optional, seconds (default 300)
# proxy_url = "http://127.0.0.1:8080"  # optional HTTP/SOCKS proxy
```

| Key | Required | Meaning |
|---|---|---|
| `token` | yes | Bot token, resolved via the secret resolver (see below). Never passed on argv. |
| `user_id` | yes | Authorized Telegram user id. Messages from anyone else are silently ignored. |
| `default_target` | no | Session name preferred for prefix-less messages. |
| `require_mention_in_groups` | no | In group chats, only act on @mentions / replies to the bot. Default `true`. |
| `response_timeout` | no | Seconds to wait for the session's turn to end. Default `300`. |
| `proxy_url` | no | HTTP/SOCKS proxy for the Telegram API (SOCKS needs `aiohttp-socks`). |

### Secret resolution

`token` and a string `user_id` are passed through a resolver:

| Form | Resolves to |
|---|---|
| `$VAR` / `${VAR}` | `os.environ.get("VAR", "")` (warns if unset) |
| `keychain:svc` | `security find-generic-password -s svc -w` (macOS) |
| literal | returned as-is |

The token is **never** written to the launchd plist / systemd unit or passed on
the command line — the daemon reads it from config at startup.

---

## Install / run

```bash
cd plugins/ainb-fleet/bridge

# 1. Create the venv the daemon will use (NOTE: `venv`, not `.venv`).
python3 -m venv venv
./venv/bin/pip install -r requirements.txt

# 2. Add your [fleet.telegram] config (see above).

# 3. Run in the foreground to verify.
./venv/bin/python -m ainb_phone_bridge run

# 4. Install as a background service (launchd on macOS, systemd --user on Linux).
./venv/bin/python -m ainb_phone_bridge install

# Status (config + service):
./venv/bin/python -m ainb_phone_bridge status

# Teardown — removes the service cleanly. Safe when nothing is installed.
./venv/bin/python -m ainb_phone_bridge teardown
```

The daemon launches as `<bridge>/venv/bin/python3 -m ainb_phone_bridge run`. The
venv path uses `venv/bin/python3` (no leading dot). Install is **idempotent**:
re-running it overwrites the unit cleanly. The macOS plist sets `KeepAlive` +
`ThrottleInterval=10`; the systemd unit sets `Restart=always` / `RestartSec=10`.

---

## Conductor dependency

The conductor + hook/inbox/heartbeat orchestration core is a **separate track**.
This bridge does **not** build it — it targets a conductor session by name when
one exists and degrades to any running session otherwise. When the conductor
track lands, set `default_target = "conductor"` (or rely on the conductor-first
default) and bare messages route to it automatically.

---

## Tests

Pure logic (prefix routing, markdown→HTML, 4096 splitting, secret resolution,
target resolution incl. degrade-to-any, JSONL reply extraction, service-unit
generation) is fully unit-tested:

```bash
cd plugins/ainb-fleet/bridge
python3 -m venv venv && ./venv/bin/pip install pytest ruff
./venv/bin/python -m pytest        # 68 tests
./venv/bin/ruff check ainb_phone_bridge tests
./venv/bin/ruff format --check ainb_phone_bridge tests
```

The tests do **not** require aiogram, a live `ainb` binary, tmux, or a Telegram
token — every external boundary is mocked or driven through a temp transcript.

---

## Known limitations / follow-ups

- **Slack / Discord deferred** — Telegram only for this cut; the routing, format,
  and capture layers are platform-agnostic so a Slack adapter slots in cleanly.
- **No busy-queue / hooks** — agent-deck's per-conductor busy queue and
  pre/post-message hooks are not ported (they depend on the conductor track).
- **No heartbeat / NEED: escalation** — those belong to the conductor track.
- **tmux fire-and-forget** — like the rest of ainb-fleet, the send has no ACK;
  reply capture is what confirms delivery, bounded by `response_timeout`.
- **Claude transcript shape** — reply extraction assumes Claude's JSONL schema
  (`message.content[].type == "text"`); Codex/Gemini need adapters.
