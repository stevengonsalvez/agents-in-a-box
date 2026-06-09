# /goal abtop ships as an ainb plugin — sidebar item "abtop (top-for-agents)" + global `t` launch the REAL abtop full-screen via tmux (all keys native), `ainb abtop` prints `abtop --once`, graceful platform-aware install-hint when absent — proven by tripwire_abtop.rs + a vhs frame-read recording per journey + a here.now explainer.

---

## STATUS: ✅ SHIPPED — 2026-06-09 (awaiting human merge gate)

**PR #240** — https://github.com/stevengonsalvez/agents-in-a-box/pull/240 · branch `feat/abtop` · 5 commits · +2462/−6 · 27 files.

### Delivered
- **Plugin crate** `ainb-tui/crates/ainb-plugin-abtop/` (cloned from witr, data-render stripped): present/absent detect, platform-aware install-hint empty-state, `abtop --once` CLI seam, 8 test files.
- **Host embed** (`ainb-core`): `SidebarItem::Abtop` (📡 "abtop" / "top-for-agents", shortcut `t`) + global `t` home key → `AppEvent::GoToAbtop` → first-launch consent → `AsyncAction::AttachAbtop` → `tmux new-session -A -d -s ainb-abtop "abtop --exit-on-jump"` + full-screen attach. `ids::ABTOP`, both `PLUGIN_SCREENS` copies, screen registration.
- **CLI**: `AbtopCommand` in `cli/registry.rs` → `abtop --once` (args verbatim, stdio inherited); absent → graykode install hint + exit 1. Count test 23→24.
- **First-launch consent**: `ConfirmAction::{SetupAbtopRateLimits, OpenAbtopSkipSetup, DismissAbtopSetup}` + `show_abtop_setup_prompt`/`should_offer_abtop_setup`/`dismiss_abtop_setup`; offers `abtop --setup`, never writes `~/.claude` without "Enable".
- **Test**: `tests/tripwire_abtop.rs` (tmux e2e, J6→J1).

### Verification
- `tripwire_abtop.rs` — green (tmux e2e). `ainb-plugin-abtop` — 42 tests green. Host — 896 lib tests pass (9 docker fails = no daemon, pre-existing/environmental). `ainb abtop` absent → graykode hint + exit 1. CLI count test 23→24. Stable rustfmt clean.
- **Frame-truth (vhs)**: frame@16s = sidebar `📡 abtop [t]` + "Enable abtop rate-limit tracking?" consent; frame@28s = real abtop v0.4.7 monitor full-screen (quota/tokens/projects/ports/mcp/sessions). Recording GIF+MP4.
- **Code review** — no blockers; 2 MEDIUM fixed (install hints repointed to canonical `graykode/abtop` since the fork has no release assets; `-A` on the `abtop --setup` tmux session).
- **Explainer (gate 5)**: https://humble-badge-33cw.here.now/

### Decisions made during build (flagged for Stevie)
- Install hints point at **canonical `graykode/abtop`** (fork has no releases/tap/crate). Flip back if you publish your fork's release infra.
- Sidebar renders label `abtop` + subtitle `top-for-agents` (two-line widget) rather than a single overflowing `abtop (top-for-agents)` string.

### Remaining (the only open item)
- [ ] **Human final gate** — review the recording/frames at the here.now link, then `gh pr merge 240 --merge`. This is the one success-criterion I cannot self-satisfy.

### 5 commits
`feat(plugin): add ainb-plugin-abtop crate` · `feat(tui): add abtop (top-for-agents) menu item + full-screen embed` · `feat(cli): add 'ainb abtop' snapshot command` · `test(tui): tripwire for abtop embed + first-launch setup consent` · `fix(abtop): canonical graykode install hints + reuse setup tmux session`

---

— CONTEXT —
· Project: Integrate the external `abtop` TUI ("htop for AI coding agents", github.com/stevengonsalvez/abtop) into ainb-tui EXACTLY the way `witr` is integrated — a discoverable plugin crate (`ainb-plugin-abtop`) for runtime detect + install-hint empty-state + a `ainb abtop` CLI, PLUS hardcoded host wiring that, on menu select/hotkey, suspends ainb and full-screen tmux-attaches the real `abtop` binary (so every key is natively abtop's).
· Stack: Rust; host ratatui 0.30 + crossterm + tokio + clap; plugin uses `ainb-plugin-sdk-rust`; tmux for the attach. abtop is an EXTERNAL Rust binary (ratatui 0.29, MIT) — launched, never vendored. Clone template: `crates/ainb-plugin-witr/`.
· Current state: branch `feat/abtop` is EVEN with origin/main @ 46d127b4 (just rebased; 39 commits landed so witr/host line numbers DRIFTED — re-read files, trust symbols not line numbers). No abtop code yet. Full spec + research live (local-only, gitignored) at `research/2026-06-05_17-16-56_abtop-plugin-integration-spec.md` and `research/2026-06-05_17-16-56_abtop-plugin-integration.md` — but THIS goal is self-contained; appendices below carry everything.
· Working dir: /Users/stevengonsalvez/.agents-in-a-box/worktrees/by-name/agents-in-a-box--feat-abtop--732f85f6 (ainb workspace root is `ainb-tui/`)
· Constraints: detect-at-runtime, NEVER bundle the abtop binary; NEVER modify ~/.claude/settings.json without explicit consent; atomic per-concern commits via /commit (never raw git commit, never git add -A); merge commit never squash; SCOPED clippy -D warnings (main carries pre-existing -D debt in plugin crates — scope to crates you touch, prove pre-existing reds vs HEAD~1); keep BOTH PLUGIN_SCREENS copies (builtin.rs + state.rs) in sync; macOS AMFI SIGKILLs unsigned staged plugins (exit 137) — re-sign via `just stage-plugins`; rustfmt YOUR own files only (`rustfmt <file>`, repo is stable-clean), never `cargo fmt -p`.
· Audience: ainb users — developers running multiple Claude Code / Codex / OpenCode agents who want a "top for agents" view one keypress away.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. Selecting "abtop (top-for-agents)" in the sidebar OR pressing `t` on the home screen suspends ainb and full-screen-attaches the REAL abtop (tmux session `ainb-abtop`, launched as `abtop --exit-on-jump`); quitting abtop returns to ainb. Proven by `tripwire_abtop.rs` (tmux e2e) AND a vhs recording whose frames are READ (via /media-processing) and asserted to show abtop's real agent UI — not "screen shows something".
2. `ainb abtop [args]` runs `abtop --once [args]` (args forwarded verbatim) and prints the snapshot to stdout; when abtop is absent BOTH the menu empty-state and the CLI show the platform-aware install hint (macOS → `brew install graykode/tap/abtop`, Linux → installer script, fallback `cargo install abtop` + repo URL) and the CLI exits nonzero. Covered by plugin-crate tests + a vhs recording of the empty-state.
3. Full /tmux-verify gauntlet GREEN: `cargo test --workspace` + scoped `clippy -D warnings` + `tripwire_abtop.rs` all pass; one vhs recording per user journey (J1–J6 below) with frames read & asserted to EXACT outcomes; an /explain-to-me explainer (tests + commits + GIFs) published to here.now; human final-gate sign-off.
4. Final deliverable runs without errors
5. You can show proof (screenshot · test output · URL)

— IMPLEMENTATION APPROACH — run as a full /workflow (multi-phase, gated) —
Build sequentially within shared-crate files (no parallel same-file agents); review in parallel. Open the PR after Phase 1 and ROLL subsequent phases into it. Each phase ends green (build + scoped clippy + that phase's tests) before the next starts.

· Phase 0 — Recon & pin. Re-read the CURRENT host files (line numbers drifted post-rebase): `ainb-core/src/components/sidebar.rs`, `app/events.rs`, `app/state.rs`, `app/screens/{mod,builtin}.rs`, `main.rs` (the `AsyncAction::AttachWitr` arm + `WITR_SESSION`), `cli/registry.rs` (the `built_ins()` register list + its exact-count test — read the ACTUAL number, don't assume 23). Install abtop locally (`cargo install abtop`) and capture its real `--version` line, `--once` output, and a stable on-screen string for the tripwire assertion.
· Phase 1 — Plugin crate. Scaffold `crates/ainb-plugin-abtop/` by copying `crates/ainb-plugin-witr/`; STRIP the data-plane (`model.rs`, `publish.rs`, `slash.rs`, `render/{processes,ports,containers,locks,detail,tabs}.rs` — they parse `witr --json`, impossible for abtop). KEEP & adapt: `detect.rs` (present/absent only — `which abtop` resolves + runs; NO version gate), `exec.rs` (shell `abtop --once [args]`, capture stdout text), `cli.rs` (thin pass-through), `render/empty.rs` (platform-aware install hint + "press r to re-check"), `plugin.rs` (on_init detect, render empty-state only, key `r`=re-check, cli_dispatch), `main.rs`, `lib.rs`. `manifest.toml`: `name="abtop"`, `spawn_subprocess=["abtop"]`, `provides.cli_namespaces=["abtop"]`, `screens=["abtop"]`, NO `event_bus`/snapshots. Rename crate/lib/bin to `ainb-plugin-abtop`/`abtop`. Add to `ainb-tui/Cargo.toml` members + `scripts/build-plugins.sh` (`build_plugin ainb-plugin-abtop abtop`). Tests: `manifest_parse`, `detect_subprocess`, `cli_dispatch`, `stdio_smoke`, `real_abtop_smoke` (skips if abtop absent).
· Phase 2 — Host wiring (menu + embed). `SidebarItem::Abtop` with icon, `label()`="abtop (top-for-agents)", `description()`, `shortcut()`="t", + `all()` entry. `ids::ABTOP="abtop"`. Add `(ids::ABTOP,"abtop")` to BOTH PLUGIN_SCREENS copies; register `PluginScreen::new(ids::ABTOP)`. `AppEvent::GoToAbtop`; bind global `t` on home; `SidebarItem::Abtop` select arm; both → `AsyncAction::AttachAbtop`. `main.rs` arm: `tmux new-session -A -d -s ainb-abtop "abtop --exit-on-jump"` + `AttachHandler::attach_to_session()`, `const ABTOP_SESSION="ainb-abtop"`. Add `ids::ABTOP` to the two `is_known_screen_id` allowlists. Update the screen-registry tests.
· Phase 3 — CLI. `AbtopCommand` (`CliCommand` impl, model on `RunCommand`) running `abtop --once` with args forwarded verbatim; missing binary → print install hint + nonzero exit. `r.register(AbtopCommand)` in `built_ins()`. Bump the exact-count test by 1 (use the real number from Phase 0). Run /code-review on the host+CLI diff; fix findings in the SAME run.
· Phase 4 — First-launch `--setup` consent. Host-side, BEFORE `AttachAbtop`: read-only check whether the abtop StatusLine hook exists in `~/.claude/settings.json`; if absent, show an ainb consent modal "Enable abtop rate-limit tracking? (runs `abtop --setup`, modifies ~/.claude/settings.json) [y/N]" → `y` runs `abtop --setup` + marks done; `N` skips with a one-time note. Never write settings.json without the `y`.
· Phase 5 — Tripwire. `crates/ainb-core/tests/tripwire_abtop.rs` cloned from `tripwire_witr.rs`: skip gracefully if `tmux` or `abtop` absent; seed isolated $HOME with completed `onboarding.toml`; launch real `ainb`; press `t`; assert tmux session `ainb-abtop` exists running abtop AND the pane shows the real on-screen string from Phase 0 AND NOT "command not found". Heed tmux-ui-tripwire traps: AMFI exit-137 (re-sign), wizard key-interception (onboarding.toml), substring-OR false-greens (assert exact strings).

— VERIFICATION — full /tmux-verify (all 6 gates) —
Record ONE vhs per journey; READ the frames with /media-processing and assert the EXACT user-visible outcome. vhs sleep budget = measured cold-paint ×1.3 (abtop ~30–60s; under-sleeping silently captures a loading screen).
· J1: home → `t` → real abtop full-screen (frames show abtop's agent table headers)
· J2: in abtop → select a session → Enter → tmux jumps to that agent's pane AND abtop exits (`--exit-on-jump`), control returns
· J3: in abtop → `q` → ainb home resumes
· J4: abtop NOT installed → menu empty-state shows the platform-aware install hint; press `r` re-checks
· J5: `ainb abtop` → prints `--once` snapshot; `ainb abtop` with abtop absent → install hint + nonzero exit
· J6: first launch → consent modal offers `abtop --setup`
Gate 1 = tripwire_abtop green. Gates 2–4 = the vhs frame-reads above + fix-loop until acceptance. Gate 5 = /explain-to-me explainer (tests + commits + GIFs) hosted on here.now. Gate 6 = human final sign-off.

— OPERATING RULES — NON-NEGOTIABLE —
1. PLAN FIRST. Output a numbered task list before writing any code.
2. WORK AUTONOMOUSLY. Don't ask clarifying Qs unless genuinely blocked.
3. SELF-VERIFY. After every step: run tests, inspect output, confirm it worked.
4. DEBUG YOURSELF. If it fails, diagnose + fix. Don't hand it back.
5. USE EVERY TOOL. MCPs · terminal · web · code exec · pull real data.
6. NO PLACEHOLDERS. No TODOs · no stubs · real components + real states.
7. PROGRESS LOG. Track completed · in-flight · decisions · blockers.
8. STAY ON GOAL. Discoveries off-spec? Note + keep moving.
9. IF BLOCKED. Log the wall · continue everything parallelizable.
10. CHECK SUCCESS BEFORE STOPPING. Re-read criteria · confirm each is met.

— QUALITY BAR —
· Code: clean, typed, follows project conventions
· Design: looks like a well-funded startup shipped it
· Output: survives a senior code review
· Docs: every new pattern / env var / decision logged

— FINAL DELIVERABLE —
✅ Confirmation each criterion is satisfied
📂 Every file created / modified
🚀 How to run / test / deploy
📊 Proof (screenshot · test output · URL)
📝 Decisions made + anything to know
⚠️ Known limitations + follow-ups

— APPENDIX A · 9 LOCKED DECISIONS (do not relitigate) —
1. Menu embed = full-screen tmux attach (witr PATH B: `AsyncAction::AttachWitr` analogue). Gives 100% native keys — it IS the real binary.
2. `ainb abtop` CLI = `abtop --once`, args forwarded VERBATIM.
3. Plugin scope = full witr-style crate MINUS data-render tabs.
4. Version gate = PRESENT/ABSENT only (no MIN_VERSION; abtop is a churning fork).
5. Home access = global `t` key + sidebar item (label "abtop (top-for-agents)"; `t`=top, `a` taken by Agents).
6. Install hint = platform-aware (mac brew graykode tap / linux installer script / cargo fallback + repo URL).
7. Enter/jump = launch `abtop --exit-on-jump` (Enter jumps to agent pane AND exits abtop → clean return).
8. `abtop --setup` (rate-limit StatusLine hook) = OFFER on first launch via consent modal; never silently touch ~/.claude/settings.json.
9. Acceptance = full tmux-verify (tripwire + vhs frame-reads per journey + here.now explainer + human gate).

— APPENDIX B · abtop FACTS —
· Binary name `abtop`. Install: `cargo install abtop` · `curl -sSL .../abtop-installer.sh | sh` · brew tap `graykode/homebrew-tap`. Self-update `abtop --update`.
· Modes: full-screen interactive TUI (alt-screen/raw/mouse). Flags: `--once` (HUMAN TEXT snapshot, NOT JSON), `--setup` (installs Claude StatusLine hook), `--theme`, `--demo`, `--exit-on-jump`, `--version`, `--update`. NO `--json`, NO machine-readable mode, NO library crate (binary-only). This is WHY content must be the embedded real binary, not a native re-render.
· Rich modal keybindings (main/config/view/filter/help); `Enter`=tmux jump. All handled for free by the full-screen attach.
· License MIT. ratatui 0.29 (host is 0.30 — irrelevant, we launch the binary).

— APPENDIX C · FILE EDIT CHECKLIST —
New crate `crates/ainb-plugin-abtop/`: Cargo.toml · manifest.toml · src/{main,lib,plugin,detect,exec,cli}.rs · src/render/{mod,empty}.rs · tests/{manifest_parse,detect_subprocess,cli_dispatch,stdio_smoke,real_abtop_smoke}.rs
Host `ainb-core`: components/sidebar.rs (SidebarItem::Abtop ×6: variant/icon/label/description/shortcut/all) · app/screens/mod.rs (ids::ABTOP) · app/screens/builtin.rs (PLUGIN_SCREENS #1 + register + tests) · app/state.rs (AsyncAction::AttachAbtop + defer-to-main + PLUGIN_SCREENS #2) · app/events.rs (AppEvent::GoToAbtop + global `t` + sidebar select arm + GoToAbtop handler + 2× is_known_screen_id) · main.rs (AttachAbtop arm: tmux ... "abtop --exit-on-jump" + ABTOP_SESSION) · cli/registry.rs (AbtopCommand + register + count-test bump) · tests/tripwire_abtop.rs
Build/infra: ainb-tui/Cargo.toml (workspace member) · scripts/build-plugins.sh (build_plugin line)
NO edits needed (generic/automatic): plugins.rs discovery (manifest-driven), crossterm_to_protocol_key, PluginScreen render loop.

— APPENDIX D · REFERENCES —
· Clone template: `crates/ainb-plugin-witr/{manifest.toml,src/detect.rs,src/exec.rs,src/cli.rs,src/plugin.rs,src/render/empty.rs}`
· Host witr precedent: `ainb-core/src/main.rs` (AttachWitr arm), `app/events.rs` (GoToWitr + select arm), `app/state.rs` (AttachWitr + PLUGIN_SCREENS), `components/sidebar.rs` (SidebarItem::Witr), `tests/tripwire_witr.rs`
· Plugin contract: `docs/plugins/spec-v2.md`, `docs/plugins/authoring.md`, `docs/plugins/witr.md`
· If present in this worktree: `research/2026-06-05_17-16-56_abtop-plugin-integration-spec.md` (full spec), `research/2026-06-05_17-16-56_abtop-plugin-integration.md` (research)
· Skills: /workflow (orchestration), /tmux-verify + tmux-ui-tripwire (acceptance), /media-processing (vhs + frame reads), /explain-to-me + /here-now (explainer), /code-review, /commit

Begin by outputting your plan. Then execute end-to-end without checking
in until done or genuinely blocked.
