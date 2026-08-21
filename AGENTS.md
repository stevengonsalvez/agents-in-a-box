# Agent Instructions

## Communication

Caveman mode is mandatory default for all agent responses. Use caveman-full: drop articles, filler, pleasantries, and hedging; keep technical terms exact. Resume caveman after any necessary safety/clarity exception. Stop only if Stevie explicitly says `normal mode` or `stop caveman`.

## Beads

This project uses **bd** (beads) for issue tracking. Run `bd onboard` to get started.

## Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --status in_progress  # Claim work
bd close <id>         # Complete work
bd sync               # Sync with git
```

## Landing the Plane (Session Completion)

**When ending a work session**, complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd sync
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds

## Fleet macOS app: validation is end-to-end or it is a failure

Testing the macOS Fleet app means driving it to a **terminal user-visible
outcome**, not confirming it launched, connected, or rendered a card.

For an interview, exactly one of these is a pass:

1. **It completes.** The answer reaches the target session — verified in that
   session's JSONL `tool_result` (and, where it matters, the model's next
   message acting on it), not just a `DELIVERED` receipt or a line drawn in a
   tmux pane.
2. **It shows the right status.** The action was refused and the app says so,
   with the daemon's `detail`, matching the `fleet_action_receipt` row.

Anything else is a FAILURE and must be reported as one:

- could not click the control / automation could not focus the window
- the card rendered but the answer was never submitted
- the receipt says `FAILED` while the app shows success (or vice versa)
- the run was abandoned part-way for any reason

**Never record an unfinished GUI run as a success, and never let unit tests
stand in for it.** Tests prove the mapping; only a driven run proves the app.
If the automation cannot complete the flow, say the validation FAILED and why —
do not describe it as "verified by tests instead", and do not present a
partially-driven run as evidence.

Check the receipt's timestamp before citing it. A stale row from an earlier run
reads exactly like a fresh success.
