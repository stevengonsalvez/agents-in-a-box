# /goal #983 is closed on v2: nothing sensitive can reach a serialised AppState frame, proven by four independent tests over the serialised JSON (name deny-list, type deny-list, canary, value-shaped tripwire) plus a locked key-path fixture, so W0-mirror can ship Serialize on sections

— CONTEXT —
· Project: agents-in-a-box (ainb) desktop programme, slice 4 node W0-mirror (`docs/plans/2026-09-12-desktop-programme.md`), which mirrors ainb-app sections to the desktop, browser and phone hosts. Spec `docs/plans/2026-09-11-multi-surface-decisions-spec.md` D15 (renderer contract, per-section frames) and the W0-mirror row. Issue #983 is the security review of P1c's serialised AppState: 47 sensitive fields reachable from the 19 sections (12 high: API-key edit buffers on three screens, the Ctrl+K keychain popup pre-filled with the literal, Grafana token, the whole `[fleet.bridge]` table with bot tokens, MCP and container env maps, repo preset env maps, cached tmux scrollback, full diff content), its trust-boundary diagram, and the five checks it prescribes. P1c shipped without Serialize on AppState for this reason; P2 and P3 kept that rule.
· Stack: Rust workspace under `ainb-tui/`; `ainb-app` (`app/state.rs`, `app/sections.rs`, 19 `Versioned<Section>` fields, `tests/state_serde.rs` which asserts `sections.len() == SectionId::COUNT`); the existing redactor `ainb-app/src/fleet/bridge/redact.rs` (`scrub`, `REDACTED`, four token regexes); `StatuslineProbe` (holds the verbatim statusLine.command string, on the deny-list per the P2 security pass); serde with `#[serde(skip)]`; the `typescript-bindings` feature.
· Current state: `v2` after P3 (#1008) and #1014; no section derives Serialize; the mirror has no transport yet, so every finding is latent and this goal is what makes Serialize safe to add. Lane F is on P4 in `ainb-app` (`app/*`, `components/*`) and lane C is on T0-section (`app/sections.rs`, the plugin, the daemon); rebase daily, name shared files in the PR, and keep your edits to the redaction module, the serde attributes and the tests wherever possible.
· Working dir: the Orca worktree this session was launched in (`g6-verify` on claude-gcp, warm build target), branch `feat/983-redaction-layer` from `origin/v2`.
· Constraints: one PR targeting `v2` (a second one is allowed for the field-level `serde(skip)` and type splits if it lands first), one file per commit, commit with `git -c commit.gpgsign=false commit` (the orchestrator re-signs at merge), say so in the PR body, no attribution trailers; no em-dashes on lines you author; never touch `ainb-core/src/app/*`; do not add Serialize to AppState or any section in this PR, the tests must instead serialise through a `wire::section_json(&AppState, SectionId) -> serde_json::Value` seam you add (per-section, which is what W0-mirror needs) so that the derive can land later behind the tests; every high finding in #983 is fixed at the type level (split `SecretInput` off `ConfigPopupType::TextInput`, `serde(skip)` on every credential buffer with a serialised length or masked glyph count where a renderer needs one, `[fleet.bridge]` and the env maps never serialised whole, scrollback and diff content out of the frame or scrubbed through `redact::scrub`), not by scrubbing at the sink alone; the type deny-list is the load-bearing check: any field reachable from a section whose type is `toml::Value`, `serde_json::Value`, `HashMap<String, String>`, `Vec<String>`, `Vec<LogEntry>` or `PathBuf` must be explicitly allow-listed with a one-line reason or the test fails; no new dependencies beyond what the workspace has (`regex` is present); gates: `cargo test -p ainb-app`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --all -- --check`; update the W0-mirror row's gate text in the programme doc in the PR that closes #983.
· Audience: W0-mirror (next lane), the desktop host, the security reviewer who re-checks the sweep, and Stevie, whose API keys and bot tokens live in these buffers.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. All 12 high findings in #983 are closed at the type level and each of the 47 fields is either skipped, split into a non-serialised type, scrubbed with a stated reason, or allow-listed with a one-line reason in the type deny-list; the PR body carries the 47-row table with the disposition of each.
2. Four tests over the serialised per-section JSON (via the new `section_json` seam) fail on `origin/v2` and pass on the branch: a case-insensitive name deny-list over JSON keys at any depth using the word list in #983 section 1; the type deny-list from section 2 with explicit allow-listing; a canary test that writes a unique marker into every writable text field of a fresh AppState (including the three API-key buffers, the keychain popup, the OTEL token form, the edit buffer, the search and filter inputs) and asserts no marker appears; and a value-shaped tripwire over the whole blob using the token patterns in section 4 (Anthropic, OpenAI, GitHub, GitLab, AWS, Google, PEM, JWT, userinfo in a URL), extending `redact.rs` rather than duplicating it.
3. The exact set of leaf key paths in the serialised JSON of every section is locked against a committed fixture (`state_serde.rs` extended), so a new serialised field fails until triaged; `ainb doctor` or a CLI subcommand can print the fixture diff; the programme doc's W0-mirror row names this goal as its met gate.
4. Final deliverable runs without errors
5. You can show proof (screenshot · test output · URL)

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

Begin by outputting your plan. Then execute end-to-end without checking
in until done or genuinely blocked.
