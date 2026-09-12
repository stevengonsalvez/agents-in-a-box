# G6 human checkpoint: slice 1, wave 1 plus S-C

**Date:** 2026-09-12
**For:** Stevie, two terminals.
**Covers:** the `[CHECKPOINT:human-verify]` for Phase 1 (keymap), the manual rows for S-A and S-B, and the manual row for S-C.
**Also published as a page:** https://claude.ai/code/artifact/892c7758-68ef-48a3-831a-176386ac0657
**Does not cover:** the Phase 3 checkpoint (scroll and mouse after the UiState seal). Phase 3 is not merged, so there is nothing to check yet; that half of G6 stays open.

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

## What to do with the result

Reply `approved`, or name the step and what you saw instead. A failure here is a real regression: every step above has an automated test behind it, so a red step means the test is lying about something.
