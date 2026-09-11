# Spike 9: Codex app-server reuse vs the ainb hook pipeline

Date: 2026-09-11. Box: linux, codex-cli 0.148.0, tmux 3.4.
Repo read-only: /home/claude/.ref/agents-in-a-box-desktop-app-part2

## Verdict

Prior-art failure does NOT reproduce for an interactive Codex TUI on 0.148.0. [fact]
An interactive `codex` runs its engine in-process and never dials an app-server, so
hook commands are children of the pane process and inherit `TMUX_PANE`. All three
runs produced complete identity.

A structurally identical failure DOES already exist in our production data, but on a
different path: hooks fired by the ainb-owned `codex app-server` (pid 30643,
`--remote-control --listen unix:///home/claude/.agents-in-a-box/codex-app-server.sock`)
carry `tmux_target: null` and `process_start_fingerprint: null`, 1215 of 1215 sampled
lines. [fact] That is correct behaviour, not a bug: those sessions have no pane.

## Precheck facts

| item | value |
|---|---|
| `grep -c AINB_AGENT=codex ~/.codex/hooks.json` | 12 [fact] |
| notify.sock listener | `ainb notifyd` pid 36711 (ainb 1.20.9), accepting [fact] |
| events.jsonl baseline | 864242 lines, 1458048964 bytes [fact] |
| notify.fallback.jsonl | absent before and after every run, so socket delivery never failed [fact] |
| pre-existing app-server 1 | pid 30643, ainb-owned, remote-control, started 20 Aug, owner marker `codex-app-server.sock.ainb-owner` [fact] |
| pre-existing app-server 2 | pid 2359534/2359566, launched by the Claude `openai-codex` plugin broker `app-server-broker.mjs --endpoint unix:/tmp/cxc-fnTb2W/broker.sock`, cwd `/home/claude/.ref/phone-whisper-android-whisper` [fact] |
| app-server 2 env | NO `TMUX`, NO `TMUX_PANE`, NO `AINB_*`. Only `CODEX_COMPANION_SESSION_ID` / `CODEX_COMPANION_TRANSCRIPT_PATH` pointing at a DIFFERENT repo's Claude session [fact] |
| model | `gpt-5.6-terra`, `model_reasoning_effort = high` [fact] |

Both pre-existing app-servers were left running and untouched. [fact]

## Method deviation (important)

The scratch git repo could not be used as cwd for the TUI runs. Codex 0.148.0 shows
a trust prompt for an untrusted directory, and accepting it writes a
`[projects."<path>"] trust_level` entry into `~/.codex/config.toml`, which the spike
rules forbid. `--sandbox read-only` does NOT suppress that prompt. [fact]

Runs A and B therefore used `/home/claude/d/git`, already `trust_level = "trusted"`
in the existing config. Run C used the scratch repo with `--skip-git-repo-check`,
which bypasses the trust gate non-interactively. `md5sum ~/.codex/config.toml` is
byte-identical before and after every run: `34efb57b8c3afe173103d54d19306365`. [fact]

An in-app update prompt (0.148.0 -> 0.150.1/0.154.0) appears on every launch; each
run answered "2. Skip". No update was installed. [fact]

## Per-run results

```
┌──────────────┐   ┌─────────────────┐   ┌──────────────────┐
│ codex TUI    │──▶│ engine IN-PROC  │──▶│ hook = child of  │
│ in tmux pane │   │ no app-server   │   │ pane, has TMUX   │
└──────────────┘   └─────────────────┘   └──────────────────┘
┌──────────────┐   ┌─────────────────┐   ┌──────────────────┐
│ ainb manager │──▶│ app-server 30643│──▶│ hook = child of  │
│ (no pane)    │   │ long-lived      │   │ daemon, no TMUX  │
└──────────────┘   └─────────────────┘   └──────────────────┘
```

| run | launch | app-server | events | session_id | cwd | transcript_path | tmux_target | fingerprint |
|---|---|---|---|---|---|---|---|---|
| A reuse | `codex` in tmux `reuse` | none new, none dialed | SessionStart, UserPromptSubmit, Stop | present | present | present | `reuse:1.1` | `pane=%0;pid=2988539;session_started=1789151817` |
| B fresh | `codex -c model=gpt-5.6-terra` in tmux `fresh` | none new, none dialed | SessionStart, UserPromptSubmit, Stop | present | present | present | `fresh:1.1` | `pane=%0;pid=3007464;session_started=1789151952` |
| C exec | `codex exec --sandbox read-only --skip-git-repo-check` in tmux `ctl` | none new, none dialed | SessionStart, UserPromptSubmit, Stop, SessionEnd | present | present | present | `ctl:1.1` | `pane=%0;pid=3028963;session_started=1789152125` |

Run A vs Run B are indistinguishable. The `-c` override changed nothing about
app-server topology, because there was no app-server to reuse in either case. [fact]

Evidence that no app-server was involved: `ss -xp` on the Run A TUI pid 2988549
showed only four anonymous `AF_UNIX` socketpairs and zero ESTAB connections to
`/home/claude/.agents-in-a-box/codex-app-server.sock` or to the broker socket. [fact]
`ps` showed the TUI as `.../bin/codex` with no `app-server` argument and no
app-server child. [fact]

Every run delivered over the notify socket. `notify.fallback.jsonl` was never
created. [fact] No daemon was restarted.

## Isolated control: the mechanism

Same payload, same `notify.sh`, scratch `AINB_HOME`, `AINB_NOTIFY_DISABLE_LAZY_SPAWN=1`:

| env | tmux_target | process_start_fingerprint | line appended |
|---|---|---|---|
| inside a tmux pane | `ctl2:1.1` | `pane=%1;pid=3037328;session_started=1789152196` | yes |
| `env -u TMUX -u TMUX_PANE` | `null` | `null` | yes |

Delivery is unaffected. Only identity is lost. [fact]

## Source: how tmux_target is derived

`crates/ainb-core/src/cli/fleet/atc.rs:2533` `current_tmux_identity()`:

```rust
let pane = std::env::var_os("TMUX_PANE").filter(|value| !value.is_empty())?;
```

then `tmux display-message -p -t <pane> -F "#{session_name}\t#{window_index}\t#{pane_index}\t#{pane_id}\t#{pane_pid}\t#{session_created}"`.

`crates/ainb-core/src/cli/fleet/atc.rs:2550` `parse_hook_tmux_identity()` formats
`target = "{session_name}:{window_index}.{pane_index}"` and
`fingerprint = "pane={pane_id};pid={pane_pid};session_started={session_created}"`.
Both fields are `None` unless all six tmux fields are non-empty. [fact]

It reads the hook process env ONLY. There is no process-tree walk, no `/proc`
ancestry, no fallback. [fact] `tmux` is resolved from `PATH` and the server is
located from the inherited `$TMUX`, so a hook in a pane of a private tmux server
still resolves correctly. [fact] Confirmed live: the `-L ainb-spike9` panes produced
correct targets.

Emitted at `crates/ainb-core/src/cli/fleet/atc.rs:2627` inside
`build_event_line_for_agent`, into the canonical events.jsonl line at
`atc.rs:2648`.

## Source: what the daemon does with a null identity

`crates/ainb-hangar-daemon/src/fleet.rs:345-357` reads both fields straight off the
hook line and computes `exact_tmux_identity = tmux_target.is_some() && process_start_fingerprint.is_some()`.

Attribution does NOT depend on them. `crates/ainb-hangar-daemon/src/fleet.rs:326`
keys the row by `SessionKey::managed(provider, observation.provider_session_id)`,
which is `"{provider}:{provider_session_id}"`
(`crates/ainb-fleet-core/src/fleet/types.rs:63`). The tmux-derived
`SessionKey::legacy` form (`types.rs:71`) is only for panes with no provider id. [fact]

A null patch field never blanks a stored value:
`crates/ainb-hangar-store/src/repo/fleet.rs:1445` uses `assign_option_if_some`, so a
later hook WITH a pane can still fill the row in. [fact]

What a persistent null costs:
- `crates/ainb-hangar-daemon/src/fleet.rs:423` skips `retire_correlated_legacy`, so a
  tmux-discovered duplicate row for the same pane is never retired. [fact]
- `crates/ainb-hangar-daemon/src/rpc/mod.rs:5222` returns
  `ProviderError::Stale("exact tmux target is unavailable")`, so send-keys control of
  that session is refused. [fact]
- `crates/ainb-hangar-daemon/src/rpc/mod.rs:5808` returns `ActionReceiptStatus::Unknown`
  for interview-liveness reconciliation. [fact]
- `capabilities: claude_managed_capabilities(exact_tmux_identity)` at
  `fleet.rs:398-402` is gated on `provider == Provider::Claude`, so codex rows do not
  get their capability set narrowed by this path at all. [fact]

## Historical reconciliation

Last 400MB of `~/.agents-in-a-box/events.jsonl`, 1668 codex hook lines, 17 distinct
sessions:

| bucket | count | tmux_target |
|---|---|---|
| transcript under `~/.agents-in-a-box/codex-home/` (ainb app-server) | 1215 | null in 1215 of 1215 |
| transcript under `~/.codex/sessions/` (direct CLI) | 88 | set in 53, null in 35 |
| transcript empty | 365 | null in 356, set in 9 |

The correlation is exact for the app-server bucket. [fact] Those sessions are driven
over the app-server WebSocket and genuinely occupy no pane, so a null pane binding is
the truth about them, not a dropped field. [inference] The 35 direct-CLI nulls are
consistent with launches outside tmux, for example a bare shell or a CI-style
`codex exec`. [inference]

## Decision input

**Does the prior-art failure reproduce for us?** No, not on the path the report
describes. [fact] Two reasons, and only the first is about us:

1. Codex 0.148.0's interactive TUI has no app-server to reuse. Its engine is
   in-process, proven by socket and process inspection. [fact] The prior-art scenario
   requires a TUI that dials a daemon; that shape does not exist on this version.
2. Even if it did, our hook script would still deliver. `notify.sh` reads
   `session_id` and `cwd` from the PAYLOAD, not from env
   (`plugins/ainb-hooks/hooks/notify.sh`, the jq block that extracts
   `.session_id // .sessionId // .resourceId` and `.cwd // .working_directory`). [fact]
   The only env-keyed identity in the whole pipeline is `AINB_AGENT`, which is
   hardcoded into the managed command string itself
   (`AINB_AGENT=codex /home/claude/.agents-in-a-box/hooks/notify.sh`), so it survives
   any environment. [fact] The prior-art report's "hook script keys identity off
   launcher env and silently drops every event" is not our script's design. [fact]

**What degrades if a daemon ever does fire our hooks?** Attribution of the pane, not
delivery and not session attribution. [fact] The event is still written, still routed,
still materialized, and still lands on the right session row via
`SessionKey::managed`. The row simply has no pane binding, which costs send-keys
control, legacy-row retirement, and interview-liveness reconciliation, per the
file:line list above.

**Smallest fix: none needed now.** [inference] Do not add a `-c` override per launch.
It buys nothing measurable here (Run A and Run B were identical) and it re-opens the
app-server orphan class that PR #521 `fix/no-orphaned-codex-app-servers` closed; note
that `docs/solutions/no-orphaned-codex-app-servers.md` does not exist in this worktree,
only the merge reference in `ainb-tui/CHANGELOG.md:2961`. [fact]

If the daemon-fired shape ever appears, the cheap fix is payload-based correlation,
not env repair: the app-server knows the pane it was spawned for, so carry the pane in
the payload and have `build_event_line_for_agent` prefer a payload-supplied
`tmux_target` over `current_tmux_identity()`. Roughly a five-line change at
`atc.rs:2627`, and it needs no launch-flag change and no fresh daemon. [inference]

Pointing the TUI at the daemon's own app-server socket is the wrong direction: 0.148.0
gives no TUI flag to attach to an existing `--listen` socket, and doing so would REMOVE
the pane binding we currently get for free. [inference]

**Watch item.** The box is pinned at 0.148.0 while 0.154.0 ships. The in-process-engine
property is a version fact, not a contract. Re-run runs A and B after any codex
upgrade; the check is two minutes and the assertion is a non-null `tmux_target` on a
`SessionStart` from a tmux pane. [inference]

## Cleanup

`tmux -L ainb-spike9 ls` reports "no server running". [fact] Sessions `reuse`, `fresh`,
`ctl`, `ctl2` were killed by exact name. All spawned codex pids are gone. Both
pre-existing app-servers (30643, 2359534/2359566) are alive and untouched.
`~/.codex/config.toml`, `~/.codex/hooks.json` and `~/.agents-in-a-box/*` config were
not modified.
