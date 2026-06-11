# Continuous explainer cadence (G5 + G6)

A single living HTML page that shows the feature being proven, refreshed as work lands, hosted publicly so the human can watch and finally validate.

## Stable slug
Pick ONE slug at the start (e.g. `ainb-warp-diff`) and keep it for the whole goal. Every republish overwrites the same URL so the link never changes. Never spin a new slug per phase.

## Build + publish
- Build the page with `/explain-to-me` — the **status-report** or **PR-review** template fits best (progress + test output + embedded media).
- Publish with `/here-now` (the page + its GIF assets as one dir).
- `scripts/publish-explainer.sh --dir <dir> --slug <slug>` validates the dir then prints the exact `/here-now` command (there is no here-now CLI; publishing is the skill's job).

## Required sections (every republish)
1. **Header** — feature name, the one-line outcome, current phase (e.g. `Phase 3/6`), overall status badge (red/amber/green).
2. **Test execution** — the latest `cargo test -p ainb-core`, `cargo clippy -p ainb-core -- -D warnings`, and `cargo test --test tripwire_<feature>` output (the real, current run — not stale).
3. **Commits** — the commit log so far on `feat/...` (hash + subject), newest first.
4. **Journeys** — one card per journey: the embedded `<name>.gif`, its one-line outcome, and a ✅/❌ for "frames read + asserted".
5. **Known gaps / blockers** — anything not yet green.

## Cadence
Republish after EVERY phase commit (and whenever a journey flips to green). The page is a build log, not a final report — it should visibly advance.

## G6 — Final HTML-solid validation (before declaring done)
FETCH the live URL and verify:
- [ ] Page loads; no 404/empty body.
- [ ] Every internal link resolves.
- [ ] Every journey GIF loads (not a broken-image icon) and plays.
- [ ] Test-output block shows the LATEST green run (matches local).
- [ ] No section is a placeholder/empty.
- [ ] Commit log matches `git log`.

Then hand the human the live URL. **The human is the final acceptance gate on the HTML** — do not self-approve the goal.
