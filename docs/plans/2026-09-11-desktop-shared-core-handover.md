# Handover: desktop app on a shared core with the TUI

**Generated**: 2026-09-11 12:28:33
**Session**: tmux `tmux_agents-in-a-box--desktop-appc204d0f1--desktop-appc204d0f1`, worktree `~/.agents-in-a-box/worktrees/by-name/agents-in-a-box--desktop-appc204d0f1--adea5f3b`, branch `desktop-appc204d0f1` (merged, 0 ahead, disposable)
**Pick up from**: `main`, a fresh session. Nothing in this worktree is unmerged.

## Current work

Design phase complete and on `main` via PR #870 (merge commit `3bfd9f4c`, 2026-09-07). Implementation has not started. Next action is `/implement docs/plans/2026-09-05-desktop-p0-surface-safety.md` from a clean worktree off `main`.

```
research (2) ──▶ spec ──▶ plan slice 1 (P0 + S) ──▶ /implement   ◀── you are here
                   │
                   ├──▶ plan slice 2 (P1-P6 extraction)   not written, needs /plan
                   └──▶ plan slice 3 (D1-D4 desktop crate) not written, needs /plan
```

## Artefacts on main

| file | role | consumer |
|---|---|---|
| `research/2026-09-04_14-10-02_desktop-app-shared-core.md` | six-angle research: extraction map, plugin/daemon contracts, shared-core prior art, agent desktop survey, Tauri v2 mechanics, frontend pick | humans, later `/plan` runs |
| `research/2026-09-04_16-25-00_tauri-agent-apps-prior-art.md` | eight OSS Tauri apps that run coding agents, ranked by what to read (Termic, Codexia, Jean first) | before P3 / D1 |
| `docs/plans/2026-09-04-desktop-shared-core-spec.md` | interview-locked design spec, decisions D1-D9, all phases as a roadmap | humans, `/plan` for later slices |
| `docs/plans/2026-09-05-desktop-p0-surface-safety.md` | **the implementation plan to run**: P0 keymap + UiState + versioned sections, Phase S surface safety; `<!-- wave -->` headers and `[CHECKPOINT]` markers for `/implement` | `/implement` |
| memory `desktop-app-shared-core-research`, `subagent-report-truncation`, `gitconfig-ssh-signing-drift` | session memory under `~/.claude/projects/.../memory/` | any Claude session on this box |

## Task progress

| item | status |
|---|---|
| research, codebase + web, 7 agents | done, committed `0c978090`, `47f8cb76` |
| interview, 9 rounds, decisions D1-D9 | done |
| spec | done, committed `5521cefb`, amended in `36cbc50d` |
| plan slice 1 (P0 + S) | done, reviewed by Codex + Opus adversarial passes, 21 findings folded, Greptile 2 nits folded, committed `36cbc50d` + `8c05ffd6` |
| PR #870 to main | merged `3bfd9f4c` |
| `/implement` slice 1 | not started |
| plan slice 2 (P1-P6), slice 3 (D1-D4) | not written |
| spikes before CI matrix lock (D4) | not run |

## Decisions locked by Stevie (do not re-ask)

- Swift `apps/ainb-fleet-macos` untouched, out of scope.
- Tauri v2 + SolidJS (Svelte 5 runner-up); separate `ainb-desktop` workspace crate; `ainb` binary unchanged.
- Renderer contract = **B, whole `AppState` mirror** in 19 `Versioned<Section>` structs (spec said 17; field audit produced 19), only changed sections cross per tick, `tauri-specta` TS regen with `tsc` in CI. Chosen over per-screen ViewModels because "every new desktop fact is a Rust change" was the deciding downside.
- Navigation state (scroll offsets, pane focus, Rects, plugin geometry, hover, `needs_redraw`) is renderer-local `UiState`; list selection and all flow state (wizard, modal, filter, active screen) stay in core because commands act on them.
- Keymap: const table in `ainb-app` + `~/.agents-in-a-box/keymap.toml` override; docs and palette generated; TUI chords use `ctrl`/`alt`/`shift` only, `cmd+` reserved for desktop.
- Plugins publish their own state struct on `ui.state` topic (free-form, zero protocol churn) plus new `plugin/handle_action`; hangar then burndown get desktop components; the rest fall back to `WireBuffer` painted in an xterm cell.
- Terminal primary over the existing `ainb-web` WS PTY bridge (tmux attach under portable-pty), attach on tab open, cap 8, cmd-chords only while focused; ACP chat card via a core transcript section fed by `fleet/transcript_subscribe`.
- Hosts: one `AppState` per host, desktop shell holds `Vec<HostApp>`; remote = ssh-forwarded Unix socket owned by the desktop; sidecar spawn reuses the daemon's existing flock singleton, never kill on exit, supervise with backoff.
- Extraction: strangler P0 → P1 → P2-P5 by screen → P6, `ainb-core` re-exports `ainb-app` so tripwires never break. Phase S (surface safety) runs in parallel.
- Proof bar: wdio tauri-service e2e on real window (macOS + Linux xvfb), parity fixtures through both renderers, 50MB WS throughput gate, vision screenshot review advisory, peekaboo/computer-use human-driver gating on release branch only.
- v1 = macOS + Linux, tabs, in-tree plugins. v2 = Windows native, tiled splits, third-party plugin bundles. Ticket import out.
- Surface concurrency: "both running or either one, all should work". Reject-second on answers (already built), no input lease on tmux, `window-size latest` pinned, minimal in-memory `ConnectionRegistry` in the daemon, daemon stamps `answered_by`.

## Plan slice 1 shape (what `/implement` will do)

```
wave 1   Phase 1 keymap-as-data + generated docs   S-A locks/atomic/tmux   S-B daemon ConnectionRegistry
wave 2   Phase 3 UiState carve-out                                          S-C card retirement on AttentionAnswered
wave 3   Phase 2 Versioned sections (after UiState, so bumps mean real change)
wave 4   S-D concurrency tests (answer race, resize-during-answer, surface combo smoke)
```

Checkpoints: one `human-verify` after Phase 1 (keymap.toml override + embed passthrough), one after Phase 3 (scroll and mouse behaviour). Everything else automated.

Facts the plan rests on, all verified against code on 2026-09-05:

- `events.rs:1473 handle_key_event`, 66 match blocks, 520 `KeyCode::` hits, 491 extracted rows; `AppEvent` 410 variants (`events.rs:24-626`); `process_event` single reducer at `events.rs:3922`.
- `AppState` `state.rs:3162-3521`, 106 fields, `Debug` only, `Default` at `:3821-3992`.
- Draw path mutates state every frame (`layout.rs:184,199`, `screens/builtin.rs:828`, `state.rs:4032`); `Screen::render` takes `&mut AppState` (`screens/mod.rs:85`). This is why UiState precedes versioning.
- Daemon singleton exists (`ainb-hangar-daemon/src/single_instance.rs:75`); first-answer-wins exists (`hangar-store/src/repo/attention.rs:344`, `daemon/src/answer.rs:71,116,160`); notifyd has `StartupLock` (`plugin-notifyd/src/listener.rs:105-138`) but its stale-recovery is racy (fix by atomic rename).
- Real data-loss hazards: `config.toml` three unlocked writers (`config/mod.rs:2029,1343,1362,2083`, `cli/config_cmd.rs:232,236`, `plugin-burndown/src/config.rs:169`); three in-place JSON writes (`favorites_store.rs:176`, `ssh_display_names.rs:59`, `onboarding.rs:104`); headroom proxy process-local lock + port 8787 (`headroom/mod.rs:32,100-155`).
- `fs2` is a workspace dep; `write_atomic` exists at `config/mod.rs:1307`; `is_text_input_context` predicate at `events.rs:1327-1440`; the 491-row keymap table must be committed as `crates/ainb-core/tests/fixtures/keymap_rows.txt` (the extraction lives only in a dead scratchpad now; regenerate from the 21 handler fns listed in the plan if needed).

## Traps hit this session (avoid repeating)

| trap | what happened | do this |
|---|---|---|
| global gitconfig drift | `~/.gitconfig` switched to ssh signing with unregistered `~/.ssh/fleet-node-signing.pub` on 2026-09-06 23:21; one commit went out `unknown_key` | sign per command: `git -c gpg.format=openpgp -c user.signingkey=907EC78C72C6AFF6 commit -S`; verify with `gh api repos/stevengonsalvez/agents-in-a-box/commits/<sha> --jq .commit.verification` |
| `/research/` is gitignored | `.gitignore:78` ignores it; tracked research docs are force-added by precedent | `git add -f research/<file>` |
| `.agents/*` ignored | brainstorm stub and spec lived there; final spec moved to `docs/plans/` | write durable docs to `docs/plans/` |
| subagent reports truncate at ~4k chars | four re-asks per agent | ask for tails "under 400 words, no tables repeated"; web-search-researcher cannot write files |
| `gh pr merge --auto` refused | "Pull Request is not mergeable" despite `allow_auto_merge` | wait for required checks, then `gh pr merge --merge` |
| CodeRabbit check stays pending | not in the main ruleset | ignore; required: Contracts, Test (both OS), hangar-e2e (both OS), installations, CLI freshness |
| tripwires are not a CI gate | CI runs two tripwire binaries (`ci.yml:326,484,494`) | run `cargo test -p ainb-core --tests` locally; plan adds named CI steps per phase |

## Resumption instructions

1. Fresh worktree off `main` (use `ainb run` or `/coding-agent` per the ainb-spawn skill, never bare checkout):
   ```bash
   git fetch origin && git worktree add ../desktop-p0 -b f/desktop-p0-keymap origin/main
   ```
2. Read in order: `docs/plans/2026-09-05-desktop-p0-surface-safety.md` (the plan), then the spec's "Renderer contract", "Extraction plan", "Concurrency between surfaces" sections, then `research/...shared-core.md` section A (extraction map).
3. Run `/implement docs/plans/2026-09-05-desktop-p0-surface-safety.md`. Wave 1 phases (1, S-A, S-B) have disjoint files and can run as three parallel worktrees, one PR each.
4. Before Phase 1 code: re-extract the keymap rows into `crates/ainb-core/tests/fixtures/keymap_rows.txt` from the 21 handler fns (`events.rs:1473-3507`); format `context | chord | event | line`, aliases `up|k` on one row, 12 inline modal guards as their own contexts.
5. Gate every phase locally with `cargo test -p ainb-core --tests` (all 101 tripwires) and `cargo clippy --workspace -- -D warnings`.
6. After slice 1 lands: `/plan` slice 2 (P1 `ainb-app` crate extraction, P2-P6) from the spec; run the four D4 spikes (universal dmg sidecar pickup, min macOS version, WS throughput on webkit2gtk, specta on a 14k-LOC struct) before slice 3.

## Open items carried forward

- Section boundaries are decided (19, table in the plan) but the per-field move order inside Phase 2 is one section per commit; expect two known borrow-split sites (`state.rs:4123`, `events.rs:3814`).
- `ConnectionRegistry` stale-entry bound after a SIGKILLed client relies on the rpc idle-timeout path; confirm during S-B.
- Transport for remote hosts is decided (ssh -L) but unbuilt; D4.
- `ainb-web` duplicates the daemon client (`ainb-web/src/daemon.rs`); migrates onto `ainb-hangar-client` in P6.
- Termic OSC 9;4 "agent finished" detection is worth lifting into the fleet pane-fallback classifier independently of the desktop.

## Notes

Stevie's instruction on 2026-09-07: "just push and commit, the plan will pick it up in another session separately. Need to be in main". Done. This worktree can be removed with `/cleanup-agent-worktree`.
