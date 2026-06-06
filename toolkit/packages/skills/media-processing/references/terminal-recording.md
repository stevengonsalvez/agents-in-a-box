# Terminal recording — CLI flows, TUIs, and live tmux

vhs is the default: a scripted PTY that emits a deterministic `.gif`/`.png`/`.mp4`
from a `.tape`, no real terminal needed (CI-friendly). Reach for **tmux** when you
need to *drive* or *assert against* a real long-lived binary; reach for
**asciinema** only when the session is genuinely interactive and can't be scripted.

## CLI command flow → gif (vhs)

A CLI recording is just a shell session: source env, run commands, sleep for the
output, animate.

```tape
Output "demo.gif"          # QUOTE the path — unquoted absolute paths tokenize wrong
Set Theme "Dracula"
Set FontSize 16
Set Width 1400
Set Height 760
Set Shell bash

Type "source ./env.sh; export PATH=./target/debug:$PATH; clear"
Enter
Sleep 1s
Type "myapp build --release"
Enter
Sleep 4s                   # real wall-clock — must cover the command's runtime
Type "myapp status | head -3"
Enter
Sleep 3s
```

## TUI → gif (vhs), skipping the cold-paint

A TUI that scans on launch (workspace/cache/network) idles for seconds; without
`Hide`/`Show` that loading screen dominates the gif. Launch hidden, sleep through
the cold paint, then `Show` and drive keys:

```tape
Output "tui.gif"
Set Theme "Dracula"
Set FontSize 16
Set Width 1400
Set Height 800
Set Shell bash

Hide
Type "HOME=/tmp/sandbox MYAPP_HOME=/tmp/sandbox/.cfg myapp"   # isolate (see traps)
Enter
Sleep 22s                  # measured cold-paint × ~1.3 — under-sleeping captures a loader
Show
Sleep 1s

Type "j"   Sleep 400ms     # bare nav keys take NO Enter
Down       Sleep 400ms     # arrows / Enter / Escape / Tab are bare directives, NOT Type "Down"
Type "/"   Sleep 300ms
Type "commit"  Sleep 1s
Escape
Type "q"
```

## Driving / asserting a live binary (tmux)

When you need a *real* terminal (tmux-aware apps, long sessions, or to read the
rendered pane), drive with `send-keys` and read back with `capture-pane`. This is
also how you measure the cold-paint sleep used above.

```bash
tmux new-session -d -s rec -x 200 -y 50
tmux send-keys -t rec "HOME=/tmp/sandbox myapp" Enter   # send-keys NEEDS a separate Enter
sleep 18                                                # cold paint
tmux send-keys -t rec "m"; sleep 1                      # a lone keystroke can omit Enter
tmux capture-pane -t rec -p | grep -i "expected text"   # assert the rendered output
tmux kill-session -t rec
```

`capture-pane -p` is text only. For a real pixel frame, record the same flow with
vhs (preferred); to stitch periodic captures into a gif, render each to png and
`magick -delay <cs> frames/*.png out.gif` (see the imagemagick references).

## asciinema (secondary — interactive real sessions)

When the flow can't be scripted (you type live, or attach to an existing tmux):

```bash
asciinema rec demo.cast          # records the real session; exit to stop
agg demo.cast demo.gif           # cast → animated gif
```

## Field-tested traps

- **Quote absolute `Output` / `Screenshot` paths** (`Output "/abs/x.gif"`); unquoted absolute paths tokenize wrong ("Invalid command").
- **`Hide` … `Sleep <cold-paint>` … `Show`** to trim the loading screen from a TUI gif.
- **`Sleep` is real wall-clock** — measure the cold paint once in tmux and add ~30%; under-sleeping silently captures a loader.
- **Bare key directives** for nav: `Down` / `Enter` / `Escape` / `Tab` — never `Type "Down"` (types the literal word).
- **`tmux send-keys` needs a separate `Enter`** to submit a command line; a lone keystroke (`m`, `q`) can omit it.
- **No in-tape file edits with `echo \"...\" >>`** — vhs quote-escaping mangles it. Do edits in pre-setup bash before the tape runs.
- **Env-isolation for determinism:** run against a throwaway `HOME` / app-config dir so recordings reproduce and never touch your real config.
- **mtime-sensitive flows:** when recording "edit a file then sync/diff", `sleep 1` before the edit so its mtime is unambiguously newer than the baseline.

Once you have the `.gif`/`.png`/`.mp4`, the rest of this skill (ffmpeg / imagemagick)
takes over — convert to mp4, pull thumbnails, composite, optimize.
