# Skill-manager recordings — expected outcomes (validation contract)

The recordings are PROOF-OF-FEATURE. Each must demonstrate the **real
user-visible outcome**, not just "a gif rendered". This file is the contract the
validator agent checks each recording's actual frames against. A recording
passes only when its final (and where noted, intermediate) frames match the
**Expected** column and contain **none** of the **Fail signatures**.

Validation method per use-case:
1. `magick "<gif>" -coalesce /tmp/f-%04d.png` to get full frames (vhs delta-encodes).
2. Read frames at ~50% / ~75% / ~90% (last frame is usually post-`q` blank shell — skip it).
3. Assert the Expected; scan for Fail signatures.
4. Also run the **live binary/CLI path** in the sandbox to confirm the product
   itself produces the outcome (the recording can only be trusted if the live
   path does too).

Sandbox always: `HOME`/`AINB_HOME`/`AINB_TOOL_HOME_CLAUDE`/`AINB_TOOL_HOME_CODEX`
point at a throwaway root (`scripts/skill-manager-sandbox.sh up --tier <t> --root <r>`).
**Never the real ~/.claude.**

---

## 1. Discover & import

**TUI** (`tui-discover-import`) — minimal tier (empty manifest)
- Action: launch → `m` → discovery banner → `Enter`.
- Expected: banner reads "Detected existing units…" with marketplace-plugin + orphan-unit counts; after `Enter`, **Units table has ≥1 imported row**, Sources panel lists the discovered sources, full help bar visible.
- Fail signatures: empty Units after Enter · no banner · `panic` / `Error` / red screen.

**CLI** (`cli-discover-import`) — `ainb migrate --discover`
- Expected: stdout shows `marketplace plugins discovered: N`, `orphan units discovered: M`, `+ source …`, `+ unit …`, `manifest written to …`.
- Fail: nonzero exit · `0` discovered · panic.

## 2. Find & install a new skill  ← KNOWN BROKEN (lands in error)

**TUI** (`tui-browse-install`) — full tier, `AINB_CATALOG_MOCK=1`, `AINB_CATALOG_MOCK_INSTALL_URI=git:file://…`
- Action: `m` → `b` → type query → `Enter` (search) → results modal (≥1 hit) → `Enter` (install selected).
- Expected: results render; install proceeds; **final frame shows the newly installed unit in the Units table + a success toast/notification**. NO error string, NO red error screen.
- Secondary (no-key path): mock OFF + no `AINB_SKILLS_API_KEY` → browse shows a **graceful "set AINB_SKILLS_API_KEY" empty-state**, not a crash.
- Fail signatures: any error/`failed to install`/`panic` frame (the current bug) · empty results with no graceful message · install that ends back on an unchanged Units table.

**CLI** (`cli-browse-install`)
- Expected: `ainb skill browse <q>` (mock) prints ranked results; `ainb skill install <git-uri> --targets claude --yes` prints an install confirmation and the unit is deployed under the sandbox `.claude/skills/`.
- Fail: nonzero exit · error text.

## 3. Keep your own skills / Library  ← KNOWN SUSPECT (looks identical to others)

**TUI** (`tui-own-library`) — full tier seeded with a `library.yaml` own-skill
- Action: `m` → `l`.
- Expected: a **DISTINCT Library view** — a header/title that says Library (own/authored skills) and lists the seeded own-skill (e.g. `my-helper`). Must be **visibly different** from the Units/Sources discovery screen.
- Fail signatures: same screen as discover/import · no library header · frame is indistinguishable from `tui-discover-import` / `tui-update-remove` (the "all recordings look the same" bug).

**CLI** (`cli-own-library`)
- Expected: `ainb skill library new my-helper` → `registered own-skill 'my-helper' → .claude/skills/my-helper`; `ainb skill library list` → lists `my-helper`.
- Fail: error · empty list after a `new`.

## 4. Sync an edit

**TUI** (`tui-sync-edit`) — full tier; a deployed file edited before launch
- Action: `m` → select the synced unit → `s`.
- Expected: a **sync result notification/toast** (e.g. `sync: …`); no error.
- Fail: error frame · no visible feedback after `s`.

**CLI** (`cli-sync-edit`)
- Expected: `ainb skill sync --to-repo --yes` prints a sync summary; the bare remote's `git log` shows a new commit.
- Fail: error · no new commit on the remote.

## 5. Update & remove

**TUI** (`tui-update-remove`) — full tier
- Action: `m` → select unit → `u` (update) → `r` (remove).
- Expected: `u` yields an update notification; after `r` the **removed unit is gone from the Units table**.
- Fail: error · unit still present after `r`.

**CLI** (`cli-update-remove`)
- Expected: `ainb skill check` prints a drift summary; `ainb skill remove <uri> --yes` confirms removal and the unit is gone.
- Fail: error · unit still listed.

## 6. Search & navigate (TUI-only)

**TUI** (`tui-search-nav`) — full tier, ≥2 units
- Action: `m` → `Down`/`Up` (cursor moves, Detail tracks) → `/` → type a filter → list narrows → `Esc`.
- Expected: cursor highlight moves with nav and the Detail pane updates to the selected unit; `/` opens the "Search units" modal; typing **narrows** the Units list.
- Fail: nav no-op · Detail doesn't change · filter doesn't narrow · modal doesn't open.

---

## Cross-cutting "all look the same" check

The validator must diff a representative frame of **every** TUI recording
against every other and assert they are **not** near-identical. Three of six
TUI journeys end on the same Units/Sources screen by design (discover, update-
remove, sync all show the manager) — so the discriminator is the **transient
state**: banner (discover), Library view (`l`), search modal (`/`), install
toast (browse), removed-row delta (remove), sync toast (sync). If two
recordings are pixel-near-identical across their whole timeline, that is a
fail — the distinguishing action was not captured.
