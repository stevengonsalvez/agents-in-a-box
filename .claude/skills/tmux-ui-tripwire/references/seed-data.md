# seed-data.md — synthetic fixture data for tripwire assertions

## When you need this

Asserting on real-data markers like `Total Calls: <n>`, `$<digit>.<digit>`,
or non-zero session counts requires the isolated HOME to contain data
session-reader can scan. An empty `tempdir` HOME gives a zero-state
render — burndown shows `Total: 0 tokens · 0 days · 0 projects · 0 sessions`
which proves the pipeline works but won't satisfy strict assertions.

## Claude session — synthetic jsonl

session-reader's claude parser walks `~/.claude/projects/<project>/*.jsonl`
and looks for `type:"assistant"` lines with `message.usage.input_tokens` /
`output_tokens`.

Seed shape (one assistant turn = one priced call):

```rust
let proj_dir = home
    .join(".claude")
    .join("projects")
    .join("-tripwire-fixture-project");
fs::create_dir_all(&proj_dir).expect("create claude project dir");

let session_jsonl = r#"{"type":"assistant","timestamp":"2026-05-10T12:00:00.000Z","sessionId":"fixture-session-1","cwd":"/tmp/x","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":1000,"output_tokens":500,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}
"#;
fs::write(
    proj_dir.join("fixture-session-1.jsonl"),
    session_jsonl,
)
.expect("seed synthetic claude jsonl");
```

Notes:

- `model` must match a known pricing entry in `ainb-plugin-burndown` —
  `claude-sonnet-4-5` works as of May 2026. If pricing tables move,
  burndown will render the call but with `$0.00` cost.
- One assistant line per file is enough — the parser handles JSONL
  line-by-line.
- Multiple files in the project dir aggregate; multiple project dirs
  bump the "projects" count.
- `timestamp` controls which day bucket the call lands in (UTC).
  Recent timestamps (within last 30 days) show up in "Last 7 days" and
  similar default windows.

## Codex / Gemini / Copilot

Same pattern, different paths:

| Provider | Path | Format |
|---|---|---|
| Codex | `~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-*.jsonl` | check `crates/ainb-plugin-session-reader/src/parsers/codex.rs` |
| Gemini | (see `parsers/gemini.rs`) | TBD |
| Copilot | (see `parsers/copilot.rs`) | TBD |

For tripwires that only need *some* data, claude is the simplest seed —
shallowest dir tree, well-documented shape.

## What the seed proves

A single 1000-input/500-output sonnet-4-5 call produces:

- Total Calls: `1`
- Total Cost: ~`$0.0105` (sonnet-4-5 input $3/M, output $15/M)
- Renders `$0.` substring → satisfies `c.contains("$0.")` in test assertions
- 1 project, 1 session, 1 day

## Pitfalls

| Pitfall | Symptom | Fix |
|---|---|---|
| Forgot `cwd` field | Parser may drop the line | Include `"cwd":"/tmp/x"` |
| Used unknown model name | Calls show but $0.00 cost | Use `claude-sonnet-4-5` or check current pricing table |
| Timestamps in distant past | Default-window aggregation filters them out | Use recent timestamps (within last 7-30 days) |
| Forgot trailing newline in jsonl | Parser may miss the last line | End the string with `\n` |
| jsonl with non-`type:"assistant"` lines only | Zero data even though file exists | Need at least one assistant turn with usage |
