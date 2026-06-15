# `ainb fleet bridge` — native phone bridge (Telegram + Slack)

A single-binary Rust daemon, built into `ainb`, that relays messages **two-way**
between a chat app and a named `ainb` session — a conductor/ATC session when one
is present, any running session otherwise. Drive and observe a coding-agent
session from your phone while away from the keyboard, with **no separate Python
runtime** to install or manage.

It ports the proven logic of the Python `ainb_phone_bridge` (now deprecated) and
adds a **Slack** channel. Both channels share one relay/routing core.

```
  Telegram ─long-poll─▶                                  ┌─▶ ainb session
                        ainb fleet bridge ─tmux send-keys─┤
  Slack ─socket-mode─▶  (shared relay core)              └─ read JSONL transcript
     ▲                          │                              │
     └──── reply (split) ───────┴──── wait for assistant end_turn ◀──┘
```

## How it talks to ainb

The bridge reuses ainb's existing, verified mechanisms — it invents no new
transport:

| Concern | Mechanism |
|---|---|
| Discover sessions | `crate::fleet::discover_from_ainb` (`ainb list --format json`) → running `workspace_name` / `tmux_session_name` / `worktree_path` |
| Send a message | `tmux send-keys -t <session> -l -- <text>` then `Enter` (literal mode + `--` terminator — injection-safe, and a payload starting with `-` is treated as text, not a flag) |
| Capture the reply | Read the session's Claude JSONL transcript under `~/.claude/projects/<cwd-slug>/*.jsonl`, returning the text of the next assistant turn whose `stop_reason` is `end_turn` |

### Reply-capture correctness (the three verified fixes, preserved)

1. **Complete-line-only** — the read offset advances only past newline-terminated
   lines, so a reply landing in a not-yet-flushed partial write is re-read once
   the line completes instead of being skipped and lost.
2. **Rotation-follow** — Claude can roll to a new `*.jsonl` mid-turn (resume /
   compaction); each poll re-resolves the latest transcript and, on a switch,
   resets the offset to 0 so the end-of-turn row in the new file is not missed.
3. **Send-time guard** — only rows whose JSONL `timestamp` strictly post-dates
   the wall-clock send instant count as the reply. Without it, an offset reset on
   rotation would surface a rolled-up *pre-send* `end_turn` (carried into the new
   file by compaction) as the answer. A row with no parseable timestamp is
   rejected once the guard is active.

## Routing

A leading `name: message` prefix selects a named session (case-insensitive,
**only** when `name` matches a running session — so a normal sentence like
`note: fix this` is *not* mis-routed). A bare message hits the default target:
conductor/ATC sessions win, otherwise the alphabetically-first session (stable
across restarts). An unknown `name:` prefix is treated as plain text.

## Authorization

- **Telegram** — by `user_id`; unknown senders are silently ignored. In group
  chats the bot acts only when @mentioned or when the message is a reply to the
  bot (gated by `require_mention_in_groups`, default `true`).
- **Slack** — by Slack `user_id`; unknown senders ignored. `listen_mode`
  (`mentions` default, or `all`) decides whether the bot acts only on
  app-mentions/DMs or on every message in subscribed channels. The bot ignores
  its own and other bots' messages (no reply loops).

Tokens are resolved through the **secret resolver** and are **never** placed on
argv or written into the launchd/systemd unit:

| Reference | Resolves to |
|---|---|
| `$ENV_VAR` / `${ENV_VAR}` | the process environment variable (warns if unset) |
| `keychain:service` | `security find-generic-password -s service -w` (macOS) |
| anything else | the literal string |

## Configuration

Config lives in ainb's `~/.agents-in-a-box/config/config.toml` (override with
`AINB_CONFIG_PATH`). At least one channel table must be present.

```toml
[fleet.bridge]
response_timeout = 300              # optional shared default (seconds)

[fleet.bridge.telegram]
token = "$TELEGRAM_BOT_TOKEN"       # or "keychain:svc" or a literal
user_id = 123456789                # authorized Telegram user id
default_target = "conductor"        # optional: name to prefer with no prefix
require_mention_in_groups = true    # optional, default true
response_timeout = 300              # optional, overrides the shared default

[fleet.bridge.slack]
bot_token = "$SLACK_BOT_TOKEN"      # xoxb-… (Web API: auth.test, chat.postMessage)
app_token = "$SLACK_APP_TOKEN"      # xapp-… (socket-mode: apps.connections.open)
user_id = "U0123ABC"               # authorized Slack user id (string)
default_target = "conductor"        # optional
listen_mode = "mentions"            # "mentions" (default) | "all"
response_timeout = 300              # optional
```

### Slack app setup (socket-mode)

1. Create a Slack app, enable **Socket Mode**.
2. Bot token scopes: `chat:write`, `app_mentions:read`, `channels:history` (and
   `im:history` for DMs).
3. Event subscriptions (over socket mode): `app_mention`, `message.im` (and
   `message.channels` if you use `listen_mode = "all"`).
4. App-level token (`xapp-…`) with `connections:write`.

## Running

```bash
# Foreground (reads config.toml; runs every configured channel concurrently):
ainb fleet bridge run

# Install as a launchd (macOS) / systemd-user (Linux) service. The unit's
# ProgramArguments/ExecStart is `ainb fleet bridge run` — no token on the command
# line; the daemon reads it from config at startup. Idempotent.
ainb fleet bridge install

# Status / teardown:
ainb fleet bridge status
ainb fleet bridge uninstall
```

Logs go to `~/.agents-in-a-box/phone-bridge.log`.

## Migration: Python → Rust

| Python (`ainb_phone_bridge`) | Rust (`ainb fleet bridge`) |
|---|---|
| `python -m ainb_phone_bridge run` (in a venv) | `ainb fleet bridge run` |
| `python -m ainb_phone_bridge install` | `ainb fleet bridge install` |
| `[fleet.telegram]` config table | `[fleet.bridge.telegram]` |
| Telegram only | Telegram **and** Slack |
| launchd label `com.agentsinabox.phone-bridge` | same label (uninstall the Python service first to avoid two daemons) |

Steps:

1. `python -m ainb_phone_bridge uninstall` (or remove the old launchd/systemd unit)
   to stop the Python daemon — both use the same service label.
2. Move your `[fleet.telegram]` block to `[fleet.bridge.telegram]` (rename the
   table; keys are unchanged). Add `[fleet.bridge.slack]` if you want Slack.
3. `ainb fleet bridge install`.

The Python bridge is retained as the behavioral reference and for existing
installs; it will be removed in a later release.

## Tests

Pure logic is unit-tested without a live fleet or network (run from `ainb-tui/`):

```bash
cargo test -p ainb --lib fleet::bridge
```

Coverage: prefix routing + conductor-first default + degrade; markdown→HTML and
4096-char split (split-before-convert); secret resolution (`$ENV`/`${ENV}`/
`keychain:`/literal); config parsing/validation for both channel tables; the
shared relay core (every outcome, via an in-memory transport fake); the JSONL
reply scan (complete-line-only, send-time guard accept/reject, no-timestamp
rejection, multi-block concat); Telegram mention-gating + mention-strip; Slack
event classification (mentions vs all, auth, self/bot/subtype filtering) +
envelope parsing; and launchd/systemd unit rendering (argv correct, no token
leakage). A live-token end-to-end run is manual.
