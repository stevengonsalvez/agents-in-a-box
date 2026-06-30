---
name: ainb-fleet:bridge
description: |
  Native phone bridge — relay messages two-way between a chat channel
  (Telegram, Slack, and/or Discord) and your ainb sessions. Inbound chat
  messages route to a target session (by `name:` prefix, else a conductor-
  first default) and are delivered via tmux send-keys; the session's reply is
  captured from its JSONL transcript and sent back to the chat. Config lives
  in ~/.agents-in-a-box/config/config.toml under [fleet.bridge.*]; tokens
  resolve from $ENV / keychain refs and are NEVER passed on argv. Use to run,
  install, or check the bridge — this is the channel ATC pages you through.
version: "0.1.0"
user-invocable: true
triggers:
  - ainb-fleet:bridge
  - fleet bridge
  - phone bridge
  - telegram bridge
  - slack bridge
  - discord bridge
  - relay my sessions to my phone
allowed-tools:
  - Bash
---

# ainb fleet:bridge — native phone bridge

A single-binary, two-way relay between a chat channel and your ainb fleet.
It folds every channel into the ainb binary — there is no separate Python
runtime to install or manage. Three channels share ONE relay/routing core over
one transport:

- **Telegram** — long-polling `getUpdates` over HTTP.
- **Slack** — socket-mode WebSocket.
- **Discord** — raw Gateway WebSocket.

```
chat message ──▶ bridge ──parse target──▶ tmux send-keys ──▶ ainb session
   reply    ◀── bridge ◀──capture from JSONL transcript tail ◀──┘
```

This is ATC's voice to you: when ATC escalates, the `NEED: …` line surfaces in
whichever channel the bridge runs.

## Subcommands

```bash
ainb fleet bridge run            # start the daemon (default if no subcommand)
ainb fleet bridge install        # install as a launchd/systemd service (idempotent)
ainb fleet bridge uninstall      # remove the service unit (returns the removed path, if any)
ainb fleet bridge status         # report INSTALL state of the service unit
```

`run` loads the config and drives every configured channel concurrently; it
runs until the process is stopped. `bridge` with no subcommand is the same as
`bridge run`.

> **`status` reports INSTALL state, not liveness.** For "is the bridge actually
> running and connected right now?" use [`/ainb-fleet:daemons`](../daemons/SKILL.md)
> — its heartbeat-backed probe distinguishes a live+connected bridge from a
> crashed one (stale heartbeat / recycled pid). `bridge status` only tells you
> whether the service unit is installed.

## Config — `~/.agents-in-a-box/config/config.toml`

At least ONE channel table must be present and valid. Tokens and ids are
resolved through ainb's secret resolver: a value may be a literal, a `$ENV_VAR`
ref, or a `keychain:service` ref — so the secret stays out of argv and out of
the launchd/systemd unit, read in-process only at startup. Config path honours
`$AINB_CONFIG_PATH`.

```toml
[fleet.bridge]
response_timeout = 300            # optional, seconds — shared default (per-channel overridable)

[fleet.bridge.telegram]
token = "$TELEGRAM_BOT_TOKEN"     # or "keychain:svc" or a literal
user_id = 123456789               # authorized Telegram user id (integer)
default_target = "conductor"      # optional: session to prefer when no name: prefix
require_mention_in_groups = true  # optional, default true
response_timeout = 300            # optional, overrides the shared default

[fleet.bridge.slack]
bot_token = "$SLACK_BOT_TOKEN"    # xoxb-… (Web API)
app_token = "$SLACK_APP_TOKEN"    # xapp-… (socket-mode)
user_id = "U0123ABC"             # authorized Slack user id (string)
default_target = "conductor"      # optional
listen_mode = "mentions"          # "mentions" (default) | "all"
response_timeout = 300            # optional

[fleet.bridge.discord]
token = "$DISCORD_BOT_TOKEN"      # Bot token (gateway + REST), secret ref
user_id = "123456789012345678"    # authorized Discord user id (snowflake string)
default_target = "conductor"      # optional
channel_id = "123456789012345678" # optional fallback channel for replies
response_timeout = 300            # optional
```

Missing `[fleet.bridge]`, or a `[fleet.bridge]` with no channel table, is a
hard error. A token/id that resolves to empty (e.g. an unset env var) errors at
startup rather than silently connecting as nobody.

## Routing — how an inbound message finds a session

A leading `<name>:` prefix selects a target session, but **only when `<name>`
addresses a real session** — matched case-insensitively against the session's
`ainb run --name` first, then its workspace/repo folder name as a fallback
alias. This guard stops an ordinary sentence like `note: fix this` from being
mis-routed to a session called `note`.

```
"backend: run the tests"   ─▶ session "backend", message "run the tests"
"just do it"               ─▶ default target,    message "just do it"
"note: fix this bug"       ─▶ default target,    message "note: fix this bug"  (no session named note)
```

With no resolvable prefix, the bridge relays to the **default target**:
a conductor/ATC session if one exists (names `conductor` or `conductor-*`),
otherwise the alphabetically-first session — or `default_target` from config.
Authorization is enforced per channel: only the configured `user_id` is
relayed.

## Run it for real (persistent)

```bash
# Install + manage as an OS service (survives logout / reboot):
ainb fleet bridge install
ainb fleet bridge status        # confirm the unit is installed
# …later:
ainb fleet bridge uninstall

# Or run it directly under tmux for a session you watch:
tmux new-session -d -s fleet-bridge "ainb fleet bridge run 2>&1 | tee ~/.ainb-bridge.log"
```

## Caveats

- **Fire-and-forget delivery.** Like the rest of the fleet, inbound delivery is
  a tmux send-keys with no ACK; the reply is captured from the target's JSONL
  transcript tail, bounded by `response_timeout` (default 300s). A session that
  takes longer than the timeout to finish its turn may have its reply missed.
- **`status` ≠ liveness.** It is the install-state of the service unit only —
  use `/ainb-fleet:daemons` for the running/connected/crashed verdict.
- **One authorized user per channel.** Messages from any other chat user are
  ignored; there is no multi-user ACL in v1.
