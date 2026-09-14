# G6 human checkpoint: slice 1

**Date:** 2026-09-12, re-verified 2026-09-13 against `v2` at `4cbab0970`, steps 1 to 7 re-run 2026-09-13 against `v2` at `026f40828`
**For:** Stevie, two terminals.
**Covers:** the `[CHECKPOINT:human-verify]` for Phase 1 (keymap) and Phase 3 (scroll and mouse), the manual rows for S-A and S-B, and the manual row for S-C.
**Also published as a page:** https://claude.ai/code/artifact/892c7758-68ef-48a3-831a-176386ac0657

Every command below is literal. The paths and chord names were re-run against `ainb 1.28.2 (4cbab0970)`, built from `v2` after all seven slice-1 nodes landed, not copied from the plan.

**What this is checking:** Phase 1 `2230c1b8`-era keymap, S-A, S-B, S-C `ba1590cc`, Phase 3 `2230c1b8`, Phase 2 `fbbe22e2`, S-D `83d1a87f`.

## Build the binary you are checking

From the root of any checkout of this repository (this adds a fresh worktree, it does not touch the checkout you run it from):

```
git fetch origin v2
git worktree add --detach ../g6-v2 origin/v2
cd ../g6-v2/ainb-tui
CARGO_INCREMENTAL=0 cargo build -p ainb -p ainb-hangar-daemon
bash scripts/build-plugins.sh
./target/debug/ainb --version
```

`target/debug/ainb` is the binary every step below means, and every `./target/debug/ainb` below is run from that `ainb-tui` directory. The version line must name the `v2` SHA you checked out. Do not build from a closed lane's worktree: those branches are behind `v2`.

## 1. Keymap override (Phase 1)

Terminal A:

```
mkdir -p ~/.agents-in-a-box
cp ~/.agents-in-a-box/keymap.toml ~/.agents-in-a-box/keymap.toml.bak 2>/dev/null
printf '[session_list]\nattach = "o"\n' > ~/.agents-in-a-box/keymap.toml
./target/debug/ainb keymap list --format json | jq -r '.[] | select(.context=="session_list" and .event=="attach") | "\(.event) -> \(.chord)"'
```

Expect exactly `attach -> o`.

Then start the TUI, press `s` for the session list, put the cursor on a session and press `o`.

- **Pass:** `o` attaches.
- **Pass:** `enter` no longer attaches.

Put your keymap back when you are done:

```
mv ~/.agents-in-a-box/keymap.toml.bak ~/.agents-in-a-box/keymap.toml 2>/dev/null || rm -f ~/.agents-in-a-box/keymap.toml
```

## 2. Embed passthrough (Phase 1)

Still terminal A, in the session list: press `A` to attach interactively.

- **Pass:** `ctrl+c` reaches the agent (it interrupts what the agent is doing; the TUI does not quit).
- **Pass:** `ctrl+q` detaches back to the session list.

## 3. Help overlay matches the generated doc (Phase 1)

Press `?` in the TUI, then compare against the page the binary generates:

```
./target/debug/ainb keymap list | less
```

Spot-check three bindings you use daily. They must agree.

## 4. Two writers, one config (S-A)

Terminal A:

```
./target/debug/ainb
```

Terminal B:

```
./target/debug/ainb
```

In A, open Config (`o`) and change one setting. In B, change a DIFFERENT setting. Quit both, start either one again.

How to change a setting on `v2` at `026f40828`: press `/`, move to the row with `up` / `down`, press `enter`, edit, `enter` to save. Plain `enter` on the settings list does not open the editor any more (the `config` keymap context has no `enter` row; only `config.search` does), even though the footer still says `Enter edit`. Quit with `ctrl+c`: on the home screen `q` is bound to `go_home`, not quit.

- **Pass:** both settings survive. Before S-A the second writer's read-modify-write dropped the first.

## 5. One headroom proxy (S-A)

With A still running, in terminal B:

```
cat ~/.agents-in-a-box/headroom/proxy.pid
./target/debug/ainb
cat ~/.agents-in-a-box/headroom/proxy.pid
```

There is only a pid to read if A has a live session with Headroom enabled and `headroom` is on `PATH`; nothing starts the proxy at TUI launch. The second `cat` above runs after B exits, so also run it from a third shell while B is still open, and check B's log (`~/.agents-in-a-box/logs/`, newest file) for `spawned headroom proxy`.

- **Pass:** the pid is unchanged, and B's log records no second spawn attempt.

## 6. The daemon knows which surfaces are connected (S-B)

Terminal A: `./target/debug/ainb`
Terminal B: `./target/debug/ainb web`
Terminal B (a third shell, or after backgrounding web):

```
./target/debug/ainb hangar connections list
```

Terminal A must be on the session list (`s`): a TUI parked on the home screen never dials the daemon.

- **Pass:** a `web` row with a pid and the daemon host. A `cli` row is the `connections list` command itself.
- **Known broken, do not fail the checkpoint on it:** there will be NO `tui` row. This is issue #963, found by S-D's surface-combination smoke and not by this
  page: `DaemonClient::from_env` labels every client `cli`, and separately the TUI holds no connection for the registry to list at all. S-D fixed the first
  half (the TUI now says `tui` when it dials); the connection lifecycle is S-B's and is still open.

  So what this step can still tell you: the registry answers, and it names the web surface correctly. If you see a `tui` row, #963 has been fixed and this
  note is stale.

## 7. An answered card retires everywhere (S-C, PR #936)

This one needs a live ASK. With the TUI open on the control center and `ainb web` open in a browser, raise an ASK in any session, then answer it FROM THE WEB.

Getting to the control center on `v2` at `026f40828`: from home press `g`, then `ctrl+p`, type `control`, press `enter`. The hangar no longer binds `C`. If the hangar shows the `danger-full-access` notice first, press `y`.

- **Pass:** the TUI card disappears at once, and the title row reads `answered by web@<your host>` for about three seconds.
- **Pass:** answering from the TUI instead closes the web card's options and reply box as soon as the request returns, with `answered by tui@<your host>` under the card.

## 8. Preview scroll, and Esc out of it (Phase 3, PR #945)

**Attach and press:** `env -u TMUX TMUX_TMPDIR=/tmp/g6h/tmux tmux attach -t g6-manual`, then `s`, leave the cursor on row 1 (`ainb/session-e855a16b`, a live pane printing `agent tick`), then `shift+up`, `up` `k` `down` `j` `pageup` `pagedown`, then `esc`; leave with `ctrl+b` `d`. Never select the `g6-manual` row under `Other tmux`: selecting the TUI's own tmux session panics it.

The chords in steps 8 and 9 are not transcribed from the plan. They are what
the binary itself prints:

```
./target/debug/ainb keymap list --format json | jq -r '.[] | select(.context=="preview_scroll" or .context=="session_list.logs_pane") | "\(.context) \(.event) \(.chord)"'
```

Session list (`s`), cursor on a session that has a live tmux pane, `preview` tab.

```
shift+up
```

- **Pass:** the pane leaves the live tail and shows scrollback.
- **Pass:** `up` / `k` / `down` / `j` / `pageup` / `pagedown` keep moving inside the pane, and the session cursor in the left list does NOT move while they do.
- **Pass:** `esc` returns to the live tail, and does not quit the TUI.

Before Phase 3 those six keys were `AppEvent` variants that the reducer handed straight back to the layout. They are `UiAction::Scroll` rows now and `UiState::apply` is the only thing that moves the pane. The reason `esc` cannot fall through to Quit is `preview_scroll_route` in `main.rs`; that is the part worth pressing twice.

## 9. Logs scroll and auto-scroll (Phase 3)

**Attach and press:** `env -u TMUX TMUX_TMPDIR=/tmp/g6h/tmux tmux attach -t g6-manual`, then `s`, move to a row whose right pane shows the `[Space]AutoScroll:ON` hint, click once inside the right pane, then `up` `up` `up`, `space`, `end`, `home`. The G6 run on 2026-09-13 could not produce such a row: the log stream only renders for a selected row with no tmux session name, and every `ainb run` session and every `Other tmux` row has one, so if no row shows the hint, report that as the result of this step.

Same screen, a session with no tmux pane, so the right pane is the live log stream. The hint line at its foot reads `[Space]AutoScroll:ON`.

Click once inside the right pane, then:

```
up  up  up
```

- **Pass:** the log scrolls back, and the hint now reads `AutoScroll:OFF`. Scrolling by hand turns the follow off; that is the point of it.
- **Pass:** `space` flips the hint back to `ON`, and new lines resume pulling the view down.
- **Pass:** `end` jumps to the newest line and leaves `AutoScroll:ON`; `home` jumps to the oldest and leaves it `OFF`.

The click matters: focus follows the mouse into the right pane, and the scroll rows only resolve while that pane owns the keyboard.

## 10. Mouse: three on the sidebar, one on the legend (Phase 3)

**Attach and press:** `env -u TMUX TMUX_TMPDIR=/tmp/g6h/tmux tmux attach -t g6-manual` (the private server has `mouse off`, so clicks reach ainb), then from the session list `q` for home; click a sidebar item once, click it twice inside 300 ms, drag the sidebar's right border; quit with `ctrl+c`, relaunch with `/tmp/g6h/start-manual.sh`, attach again and check the width; then `s` and click the bottom legend, then the collapsed hint row.

Home screen (`q` from the session list).

- **Pass:** a single click on a sidebar item selects it and moves focus to the sidebar.
- **Pass:** a second click on the SAME item within 300 ms opens it, exactly as `enter` would. Slower than 300 ms and it stays a selection.
- **Pass:** press on the sidebar's right-hand border and drag: the sidebar resizes under the cursor, and the width it is released at survives a quit and restart.

Then `s` for the session list:

- **Pass:** a click anywhere on the bottom keymap legend collapses it, exactly as the `M` binding does (the legend writes it `⇧M`). A click on the collapsed hint row brings it back.

Every rect these four clicks hit test against now lives in `UiState`, published by the renderer after each draw instead of being written into `AppState` mid-frame. A stale or unpublished rect shows up here as a click that lands on nothing.

## What to do with the result

Reply `approved`, or name the step and what you saw instead. A failure here is a real regression: every step above has an automated test behind it, so a red step means the test is lying about something.

## Proof, if you want to check my working rather than the app

| Node | PR | Merge |
|---|---|---|
| slice-1 CI gates | #933 | `82f8e90a` |
| Phase 3 UiState | #945 | `2230c1b8` |
| S-C card retirement | #936 | `ba1590cc` |
| Phase 2 versioned sections | #956 | `fbbe22e2` |
| S-D concurrency | #964 | `83d1a87f` |
| G6 page, Phase 3 half | #950 | `cd597bb9` |

S-D's merged head is the first in this lane with a fully green board: 26 pass, 1 skip, 0 fail, both OS legs.

Known limitations you may run into, all filed rather than absorbed: #963 (above), #951 (`tripwire_burndown_keys` asserts a `p filter:` chip the plugin no longer
renders, so it cannot pass on any head), #966 (four sessions tripwires die mid-key-sequence on macOS, gated to Linux with the evidence attached).
