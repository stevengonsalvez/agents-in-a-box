---
name: ainb-fleet:ainb-spawn
description: |
  Spawn a coding-agent session correctly with `ainb run`: always into a git
  worktree, never bare into a plain checkout. Use whenever you are about to
  start a Claude/Codex/Gemini/Copilot session in a terminal. Guards the
  invocations that silently produce a `(broken)` workspace row, two agents
  sharing one working tree, a `--parent` flag that never fires, or a teardown
  that cancels itself and leaks the session.
version: "0.1.0"
user-invocable: true
triggers:
  - ainb-fleet:ainb-spawn
  - ainb spawn
  - spawn a coding agent
  - run claude in a worktree
  - start an agent session
allowed-tools:
  - Bash
---

# ainb spawn, start a session correctly

One command, two legal shapes for `--repo`. Getting that choice wrong is what
shares a working tree between two agents or nests a worktree inside a worktree.

## The two legal shapes

Fresh session off a repo root (the common case):

```bash
ainb run \
  --repo /absolute/path/to/repo-root \
  --worktree --create-branch fix/issue-101 \
  --tool claude \
  --dangerously-skip-permissions \
  -p "$(cat /tmp/task-101.md)"
```

Existing task worktree (isolation already done, do not nest another):

```bash
ainb run --repo /absolute/path/to/existing/worktree --tool claude -p "$(cat task.md)"
```

Which shape? One command decides it (`.git` is a *file* in a real worktree, a
*directory* in a normal clone):

```bash
test -f "$P/.git" && echo "worktree: pass bare" || echo "repo root: add --worktree --create-branch"
```

## Flags, and why

| Flag | Why |
|---|---|
| `--repo <abs-path>` | Repo root, or an existing task worktree. Absolute, always. |
| `--create-branch <branch>` | Implies `--worktree` and names the branch. Prefer over bare `--worktree`, which invents `ainb/session-<shortid>`. |
| `--worktree` | Isolation. Lands at `~/.agents-in-a-box/worktrees/by-name/<repo>--<branch>--<shortid>`. |
| `--tool claude\|codex\|gemini\|copilot` | Which CLI to launch (default `claude`). |
| `--model <id>` | Passed through to the provider unchanged. Optional. |
| `--dangerously-skip-permissions` | Unattended runs only. |
| `-p "<task>"` | Initial prompt, sent once the input box is ready. |
| `--name <handle>` | Optional stable tmux handle. Legal: it changes the session/tmux name only, never `workspace_name`, and fleet routes on the tmux name. Prefer the minted name unless you want something to type at. |
| `--parent <session-id>` | Routes the child's completions to your inbox. Needs *your own* session id, which you must resolve (see below), not an env var. |

## Parent linkage, resolve your own id

There is **no `$AINB_SESSION_ID`**. ainb exports only `AINB_PARENT_SESSION` into
a session, and that holds the *parent's* id, not the session's own. Resolve
yours from the tmux session name:

```bash
MY_TMUX=$(tmux display-message -p '#{session_name}' 2>/dev/null)
MY_ID=$(ainb list --format json \
        | jq -r --arg t "$MY_TMUX" '.[] | select(.tmux_session_name == $t) | .session_id')

ainb run --repo "$REPO" --worktree --create-branch "$BRANCH" --tool claude \
  ${MY_ID:+--parent "$MY_ID"} -p "$(cat "$TASK_FILE")"
```

Empty `MY_ID` (not inside an ainb session) omits the flag and spawns unparented,
which is fine. The `${...:+...}` guard is not cosmetic: an unresolved id fails
two different ways, and only one of them is loud.

## Traps

| Trap | What happens | Do instead |
|---|---|---|
| `--repo <repo-root>` with no `--worktree` / `--create-branch` | Agent runs in the repo's own working tree; two agents stomp each other. The TUI used to render that row as `(broken)`; it now names it after the owning repo, so nothing in `ainb list` flags the mistake. | Add `--worktree --create-branch <branch>` |
| `--parent ""` (quoted, empty or whitespace-only) | Accepted by the parser, then trimmed and dropped by `ainb run`. The child spawns UNPARENTED, no warning, and its completions never reach your inbox. | `${MY_ID:+--parent "$MY_ID"}` so an empty id emits no flag |
| `--parent $MY_ID` unquoted with `MY_ID` unset (e.g. `"$AINB_SESSION_ID"`, which ainb never exports) | The token vanishes before clap sees it, so clap consumes the NEXT token as the parent id. Next token starts with `-`, or `--parent` was last: clap aborts with `error: a value is required for '--parent <PARENT>'`, exit 2, nothing spawns. Next token is a bare word (path, branch): clap swallows it as the parent id and the flag it belonged to is lost, corrupting the spawn. | Resolve `MY_ID` from the tmux name (above), then `${MY_ID:+--parent "$MY_ID"}` |
| `ainb kill <id>` / `ainb git cleanup` without `--force` | Both prompt `[y/N]` on stdin with no tty check. From a tool call the read returns empty, they print `Cancelled.` and exit 0, so the session leaks while you believe it is gone. | Always `--force` |
| `rm -rf <dir>` right after a bare `ainb kill` | Deletes the checkout out from under a still-live agent. | `--force`, then confirm via `ainb list` before deleting |
| `tmux new-session ... claude -p` for repo coding | No worktree, no session record, invisible to every fleet verb. | Use `ainb run` |

## Verify

```bash
ainb list --format json | jq -r '.[] | "\(.session_id)\t\(.workspace_name)\t\(.worktree_path)"'
ainb status <id>              # one-shot state
ainb logs <id> --lines 80     # recent output, -f to follow
ainb kill <id> --force        # teardown, exact id only; --force is mandatory here
ainb git cleanup --dry-run    # preview the worktree prune
ainb git cleanup --force      # perform it
```

`workspace_name == "(broken)"` means ainb could not resolve the session root to
a git repository at all: no ancestor directory holds a usable `.git`, and the
directory name carries no `<repo>--<branch>--<shortid>` hint to fall back on.
Kill it and respawn with `--worktree --create-branch`.

A plain checkout, or a subdirectory of one, is **not** broken. It resolves to the
repository that owns it and is labelled with that repository's name. It is only
unisolated, and `ainb list` will not tell you that, so a real-looking
`workspace_name` is not proof you passed the isolation flags.

## Note: subdirectory sessions carry the owning repo's name

Point `--repo` at a subdirectory of a checkout and the TUI workspace row shows
the **owning repository's** name, not the subdirectory's. That is intended (the
workspace row is the grouping key and the repo owns the group), but it reads as a
rename to anyone who remembers the old labels. Not a bug, and not a sign the
wrong path was passed.

## See also

- [`/ainb-fleet:standup`](../standup/SKILL.md) to confirm the session joined the fleet.
- `/coding-agent` (toolkit skill) for choosing between `ainb run`, a Task subagent, and raw tmux.
