# cli_burndown fixture sessions

Deterministic per-provider session JSONL files used by `tests/cli_burndown.rs`
for the Phase 6e CI gate. Layout mirrors what `~/.claude/projects/<project>`
looks like on a real install, but the on-disk dirs are renamed (`claude_projects`
instead of `.claude/projects`) so they can be checked into git despite the
project-level `.gitignore` rule that excludes `.claude/`.

`with_fixture_home` (in `tests/cli_burndown.rs`) materialises this layout
into a tempdir at the proper hidden-dir paths each test, then points
`HOME` and `AINB_HOME` at it so:

- the `session-reader` plugin reads JSONL from `<HOME>/.claude/projects/`
  via its `read_claude_logs` capability, and
- the `burndown` plugin's cache lives under `<AINB_HOME>` instead of the
  developer's real `~/.cache/ainb/`.

Adding fixtures:

- Drop new Claude-provider session files under
  `claude_projects/<project>/<session-id>.jsonl`. Use a fixed
  `timestamp` (RFC 3339) and a fixed `model` so the resulting
  baseline is byte-stable.
- Update the byte baselines under `tests/baselines/usage_cli_*.snap`
  to reflect the new aggregate counts.
- For codex / gemini / copilot fixtures: add a sibling `codex_sessions/`
  / `gemini_sessions/` / `copilot_sessions/` tree, then teach
  `with_fixture_home` to copy that root into `<HOME>/.codex/sessions/`
  etc. before running the closure.
