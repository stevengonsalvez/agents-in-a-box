# G6 human checkpoint: slice 1

**Date:** 2026-09-12
**For:** Stevie, two terminals.
**Covers:** the `[CHECKPOINT:human-verify]` for Phase 1 (keymap) and Phase 3 (scroll and mouse), the manual rows for S-A and S-B, and the manual row for S-C.
**Also published as a page:** https://claude.ai/code/artifact/892c7758-68ef-48a3-831a-176386ac0657

Every command below is literal. The paths and chord names were run against `ainb 1.28.2` from this branch, not copied from the plan.

## Build the binary you are checking

```
cd ~/orca/workspaces/agents-in-a-box/p0-closure/ainb-tui
cargo build -p ainb -p ainb-hangar-daemon
bash scripts/build-plugins.sh
```

`target/debug/ainb` is the binary every step below means.

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

- **Pass:** both settings survive. Before S-A the second writer's read-modify-write dropped the first.

## 5. One headroom proxy (S-A)

With A still running, in terminal B:

```
cat ~/.agents-in-a-box/headroom/proxy.pid
./target/debug/ainb
cat ~/.agents-in-a-box/headroom/proxy.pid
```

- **Pass:** the pid is unchanged, and B's log records no second spawn attempt.

## 6. The daemon knows which surfaces are connected (S-B)

Terminal A: `./target/debug/ainb`
Terminal B: `./target/debug/ainb web`
Terminal B (a third shell, or after backgrounding web):

```
./target/debug/ainb hangar connections list
```

- **Pass:** one `tui` row and one `web` row, each with a pid and the daemon host.

## 7. An answered card retires everywhere (S-C, PR #936)

This one needs a live ASK. With the TUI open on the control center (`g`, then `C`) and `ainb web` open in a browser, raise an ASK in any session, then answer it FROM THE WEB.

- **Pass:** the TUI card disappears at once, and the title row reads `answered by web@<your host>` for about three seconds.
- **Pass:** answering from the TUI instead closes the web card's options and reply box as soon as the request returns, with `answered by tui@<your host>` under the card.

## 8. Preview scroll, and Esc out of it (Phase 3, PR #945)

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

Home screen (`q` from the session list).

- **Pass:** a single click on a sidebar item selects it and moves focus to the sidebar.
- **Pass:** a second click on the SAME item within 300 ms opens it, exactly as `enter` would. Slower than 300 ms and it stays a selection.
- **Pass:** press on the sidebar's right-hand border and drag: the sidebar resizes under the cursor, and the width it is released at survives a quit and restart.

Then `s` for the session list:

- **Pass:** a click anywhere on the bottom keymap legend collapses it, exactly as the `M` binding does (the legend writes it `⇧M`). A click on the collapsed hint row brings it back.

Every rect these four clicks hit test against now lives in `UiState`, published by the renderer after each draw instead of being written into `AppState` mid-frame. A stale or unpublished rect shows up here as a click that lands on nothing.

## What to do with the result

Reply `approved`, or name the step and what you saw instead. A failure here is a real regression: every step above has an automated test behind it, so a red step means the test is lying about something.
