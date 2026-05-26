# `ainb skill promote` (v1.1)

> One-shot command that turns a hand-edited orphan into a
> git-backed source. Bridges the gap between adopting a `local:`
> unit and tracking it as upstream — without forcing the user
> through `skill-creator` + manual `git init` / `git push`.
>
> Full design: spec
> `.agents/goals/ainb-skill-manager-v1.1-discovery-spec.md`
> §`ainb skill promote` — Detailed Design.

## What promote does

After discovery imports an orphan as
`local:~/.claude/skills@head/my-skill`, the unit is tracked but
the source of truth is still a single file under the user's home
directory — fragile, machine-local, and impossible to share or
version. `ainb skill promote` migrates that file into a git repo
the user controls, commits + pushes it, and rewrites the manifest
URI from `local:` to `gh:user/repo@<branch>/skills/<name>`. The
on-disk file at `~/.claude/skills/my-skill/` stays where it is —
that's the deployed instance now, with the git repo as upstream.

`ainb skill update my-skill` thereafter pulls upstream and
reconciles against the local copy via the lockfile's
`file_hashes`, no-op'ing when bytes match.

## Command surface

```
ainb skill promote <unit-name> --to <git-uri> [options]

Required:
  <unit-name>           Unit name as shown in the SkillManager Units panel
                        (i.e. the trailing path segment of the URI).
  --to <git-uri>        Target repo, e.g. gh:steven/my-skills,
                        gh:org/team-skills@develop.

Options:
  --message <text>      Override default commit message (see below).
  --dry-run             Print the plan; no clone, no commit, no push,
                        no manifest write.
  --yes                 Skip the interactive "Proceed? [y/N]" prompt.
```

## Locked design (4 interview decisions)

These were settled before implementation began and should not be
revisited without re-opening the spec interview:

1. **Clone location** — persistent cache at
   `~/.agents-in-a-box/promote-cache/<user>/<repo>/`. Cloned once,
   reused on subsequent promotes against the same repo (`git pull
   --ff-only`). Survives across runs. No automatic cache eviction
   (manual: `rm -rf ~/.agents-in-a-box/promote-cache/`).
2. **Push strategy** — direct push to the repo's default branch,
   resolved via `gh repo view --json defaultBranchRef`. No PR
   creation in v1.1. Users who want a PR push to a feature branch
   manually post-promote.
3. **Post-promote local-copy handling** — keep
   `~/.<tool>/skills/<name>/` files in place. Rewrite the manifest
   URI to `gh:user/repo@<branch>/skills/<name>`. Register the
   existing local files as the deployed instance in the lockfile:
   ```yaml
   deployed:
     path: ~/.claude/skills/<name>/
     file_hashes: { ... sha256 per file ... }
   ```
   Future `ainb skill update` pulls upstream, compares hashes,
   no-ops if unchanged.
4. **`gh` dependency** — hard requirement. Pre-flight runs `gh
   auth status` and bails fast with an exact remediation command
   if it fails.

## Default commit message

```
feat(<unit-name>): promote from local

Auto-promoted by `ainb skill promote` from ~/.<tool>/skills/<unit-name>/
on <ISO-8601 timestamp>.
```

Override via `--message "..."`. Always conventional-commits-shaped;
no AI / Claude attribution per repo commit hygiene.

## Step-by-step flow

```
1. ainb skill promote my-skill --to gh:steven/my-skills

2. Pre-flight (all fail-fast before any disk writes):
   - gh auth status                          → exit 1 + remediation
   - manifest has unit "my-skill"?           → bail if not
   - unit's current URI is local:?           → bail if already on gh:/marketplace:
   - resolve <git-uri> via `gh repo view`    → fetch defaultBranchRef
   - print plan + ask "Proceed? [y/N]"        (skipped if --yes)

3. Clone / pull:
   - if ~/.agents-in-a-box/promote-cache/steven/my-skills/ exists:
       git -C <cache> pull --ff-only
   - else:
       git clone gh:steven/my-skills <cache>

4. Copy unit:
   - mkdir -p <cache>/skills/my-skill/
   - cp -r ~/.claude/skills/my-skill/* <cache>/skills/my-skill/

5. Commit + push:
   - git -C <cache> add skills/my-skill/
   - git -C <cache> commit -m "<default or --message>"
   - git -C <cache> push origin <defaultBranch>

6. Rewrite manifest + lockfile:
   - manifest: remove `local:~/.claude/skills@head/my-skill`
   - manifest: add `gh:steven/my-skills@<defaultBranch>/skills/my-skill`
                + `[source."gh:steven/my-skills"]` if not present
   - lockfile: update unit row with new URI, new sha (commit sha
               post-push), keep existing `deployed{path, file_hashes}`
               (same bytes).

7. Print summary:
   ✓ Promoted my-skill to gh:steven/my-skills@main/skills/my-skill
     Local files preserved at ~/.claude/skills/my-skill/ as deployed instance.
     Repo:     https://github.com/steven/my-skills/blob/main/skills/my-skill/
     Manifest: ~/.agents-in-a-box/manifest.yaml updated.
```

## Failure modes

| Failure                                         | Behavior                                                                                              |
|-------------------------------------------------|-------------------------------------------------------------------------------------------------------|
| `gh` not installed                              | Error: `gh CLI required. Install: brew install gh` + exit 1.                                          |
| `gh auth status` fails                          | Error: `Authenticate: gh auth login` + exit 1.                                                        |
| Repo doesn't exist / no write access            | Error: `Cannot push to gh:user/repo: <gh error>` + exit 1.                                            |
| Clone / pull fails mid-flight                   | Leave cache in dirty state (user can `cd` and inspect); print remediation.                            |
| Push fails (network, conflict)                  | Local commit is made; print `cd <cache> && git push` for retry. **Manifest is NOT rewritten** yet.    |
| Network down during pre-flight                  | Fail before any disk writes.                                                                          |
| Unit name has reserved chars / not on disk      | Bail with a clear error before any clone.                                                             |

The pattern across every failure mode: **never leave the manifest
in a partial state.** Either the rewrite is atomic post-push, or
it does not happen at all.

## Out of scope for v1.1 (deferred to v1.2)

The following are explicitly deferred — please do NOT add them as
follow-up beads under the v1.1 epic without re-opening the spec
interview:

- **`--pr` flag** — open a PR via `gh pr create` instead of
  pushing directly to the default branch.
- **`--new-repo` flag** — create the target repo via `gh repo
  create` if it doesn't exist.
- **`--workdir` flag** — promote into a named branch in the cache
  for manual PR follow-up.
- **`ainb skill pull`** — bidirectional sync from a promoted
  upstream back to the local copy. v1.1 promote is one-shot.
- **Multi-unit promote** — loop manually for now
  (`for u in a b c; do ainb skill promote $u --to ...; done`).
- **`--clean-cache` flag** — explicit eviction of
  `~/.agents-in-a-box/promote-cache/`. Manual `rm -rf` works.

## Open questions (tracked for v1.2 design)

- Should promote pull `.amazonq/rules/...`-style linked-file
  references into the new repo, or warn the user to handle them
  manually? (Current behaviour: warn.)
- How long should the promote-cache persist? (Current behaviour:
  forever until manual deletion. A future `--clean-cache` flag
  could automate.)

## Reference

- Spec: `.agents/goals/ainb-skill-manager-v1.1-discovery-spec.md`
- Discovery flow (the input to promote):
  [`docs/skill-manager/discovery.md`](./discovery.md)
- v1 skill-manager CLI surface:
  `ainb-tui/plans/skill-manager/spec.md` §8 (`source`, `skill`,
  `migrate`, `doctor`, `usage`).
