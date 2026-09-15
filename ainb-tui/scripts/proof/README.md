# Proof harness

Runs every delivered programme node against the built `ainb` binary and records
what was expected against what the running binary did. Rust tests are not the
proof here: each scenario drives the real TUI, CLI, web surface and daemon
through tmux and reads the result off the screen, the CLI, or the daemon's own
socket.

```
run.sh ──▶ for each node: ( source lib.sh + scenarios/<node>.sh )
             │
             ├─ world_up     private HOME, TMUX_TMPDIR, PATH stubs, ainb init
             ├─ scenario     keys in, captures out, check / observe
             ├─ write_result proof-out/<node>/result.json
             └─ world_down   quit TUIs, kill by exact name, stop daemon, rm world
           ──▶ summarize.py  proof-out/summary.json + summary.md, exit status
```

## Run it

From `ainb-tui/`:

```
scripts/proof/run.sh --build                  # build ainb, the daemon and plugins, then every node
scripts/proof/run.sh                          # every node against the current target/debug build
scripts/proof/run.sh --only s-c-answered      # one node (repeatable); other results are kept
scripts/proof/run.sh --out /tmp/proof-out     # results somewhere else
```

Needs `tmux`, `jq`, `git`, `curl` and `python3`. A full run of the first 17
nodes took 6 min 42 s on claude-gcp, plus the build when `--build` is given. Exit
status is 0 only when every node passes; a node whose failure is a filed
defect still fails, and its row names the issue.

## What a result holds

`proof-out/<node>/result.json`:

| field | meaning |
|---|---|
| `node` | scenario name |
| `expected` | the scenario's one-line `EXPECT` |
| `observed` | every observation in order; checks read `ok: ...` or `FAILED: ...` |
| `pass` | true only when no check failed and at least one was recorded |
| `issue` | the filed defect behind a failure, else null |
| `capture` | file names beside the result: `*.txt` plain pane or CLI output, `*.ans` the same pane with ANSI colour |
| `binary` | `ainb --version` of the binary under test |
| `started_at` | UTC start of the scenario |

The internal hostname is replaced by `<host>` in every capture and observation.

## The world each scenario runs in

```
$PROOF_WORLD/                       mktemp -d, removed at the end
  home/                   HOME
    .agents-in-a-box/     also AINB_HANGAR_HOME
  tmux/                   TMUX_TMPDIR: server `-L proof` hosts the harness
                          panes; the default server holds `ainb run` sessions
  bin/                    first on PATH: `claude` fixture agent, `headroom` stub
  repo/                   git repository the fixture sessions are spawned from
```

Decisions that are easy to get wrong:

- **`AINB_HANGAR_HOME` is `$HOME/.agents-in-a-box`.** The daemon tails
  `$AINB_HANGAR_HOME/events.jsonl`, the hook appends to
  `~/.agents-in-a-box/events.jsonl`. Split them and no hook-raised card ever
  reaches the daemon.
- **The TUI and `ainb web` run with `TMUX` and `TMUX_PANE` unset.** Inside a
  `-L proof` pane they would otherwise treat the harness server as their tmux.
  The one exception is `issue-1094-own-session`, which keeps `TMUX` on purpose
  so the TUI lists the tmux session it runs in.
- **The fixture agent** is a bash loop named `claude` that prints
  `agent tick N` every second and prints `AGENT GOT SIGINT` instead of dying.
- **ASK cards** go through the real hook path,
  `ainb fleet atc hook --event PreToolUse --matcher AskUserQuestion`, with the
  fixture session's cwd and pane. Asks on one pane collapse into one card, so a
  scenario that needs two cards spawns two sessions.
- **Web answers** use the body `frontend/app.js` posts to `/api/answer`. The
  web's snapshot can lag 120 s behind at startup (#1055), so scenarios wait for
  it to list the fixture session before answering.
- **Wire probes** (`rpc_call`, `hello_probe`) speak Content-Length framed
  JSON-RPC on the private socket with the daemon token, exactly as a surface
  does, to read `fleet/status` and to check protocol negotiation.
- **Mouse** is SGR mouse bytes sent to the pane (`click`, `double_click`).
- **Teardown** never kills by pattern. Sessions die by exact name; a process is
  signalled only when its environment carries this world's
  `AINB_HANGAR_HOME`. `run.sh` fails the run if any process from a proof world
  is still alive at the end.

## Environment

| variable | set by | meaning |
|---|---|---|
| `PROOF_OUT` | `run.sh` (`--out`) | results directory, default `ainb-tui/proof-out` |
| `PROOF_BASE_PATH` | `run.sh` | system PATH a world starts from (`/usr/local/bin:/usr/bin:/bin`) |
| `AINB_BIN`, `BINARY_LINE` | `run.sh` | binary under test and its version line |
| `AINB_HEADROOM_PORT` | `lib.sh` | free port the headroom stub serves `/health` on |
| `AINB_PLUGIN_ROOT` | `lib.sh` | `ainb-tui/dist/plugins`, staged by `build-plugins.sh` |

## Adding a scenario

1. `scenarios/<node>.sh` with a one-line `EXPECT` and a `scenario` function.
2. Drive with `start_tui`, `keys`, `type_text`, `click`, `open_hangar_screen`,
   `fixture_session`, `raise_ask`, `start_web`, `web_answer`, `rpc_call`.
3. Record with `capture <session> <name>`, `save_output <name> <cmd...>`,
   `observe "<text>"` and `check "<claim>" <command...>`. Poll with
   `wait_screen` or `wait_for`, never a bare sleep before an assertion.
4. When a check fails because the product is wrong, file the issue with the
   capture and call `known_issue <number>` for that failure only.
5. Add the node to `ALL_NODES` in `run.sh` and run `shellcheck -x` on it.
