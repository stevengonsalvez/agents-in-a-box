# Claude Code hooks

Hook scripts wired into Claude Code via `~/.claude/settings.json`. They run as separate `uv run` processes around lifecycle events (notifications, tool use, stop, session start, etc.) and produce side effects: notification logging, cost tracking, optional Langfuse traces, optional TTS announcements.

These files live canonically here in `toolkit/packages/utilities/hooks/` and are synced to `~/.claude/hooks/` on user machines.

## Files

| Hook | Event | Purpose |
|---|---|---|
| `notification.py` | Notification | Log notification, optionally announce via TTS |
| `stop.py` | Stop | Save chat, cost-track, optional Langfuse close, optional TTS completion |
| `subagent_stop.py` | SubagentStop | Same as stop, scoped to a subagent |
| `session_start.py` | SessionStart | Load session context, optional Langfuse trace start |
| `pre_tool_use.py` | PreToolUse | Pre-tool gates / logging |
| `post_tool_use.py` | PostToolUse | Post-tool logging |
| `user_prompt_submit.py` | UserPromptSubmit | Log user prompts |
| `pre_compact.py` | PreCompact | Handover + reflection before context compaction |
| `dev_server_tmux.py` | PreToolUse (Bash) | Auto-route long-running servers into tmux |
| `cost_tracker.py` | Stop | Async cost accounting |

Shared helpers under `utils/`:
- `hook_context.py` — session label, todo extraction, todo summary (used by TTS messages)
- `tts/` — TTS providers (ElevenLabs / OpenAI / cross-platform / pyttsx3 fallback chain)
- `langfuse/` — optional Langfuse tracing (no-op unless `LANGFUSE_ENABLED=true`)
- `llm/` — LLM helpers for completion-message generation

## TTS opt-in toggle

TTS announcements are **off by default**. Three sites (notification, stop, subagent_stop) all check for the same sentinel file before speaking:

```bash
# enable TTS
touch ~/.claude/.tts-on

# disable TTS
rm ~/.claude/.tts-on

# check current state
ls ~/.claude/.tts-on 2>/dev/null && echo ON || echo OFF
```

The check is implemented as one line at the top of each `announce_*()` function:

```python
if not (Path.home() / ".claude" / ".tts-on").exists():
    return
```

Granularity is all-or-nothing — one sentinel silences (or enables) every TTS site. Non-TTS side effects (chat saving, Langfuse traces, todo summaries, cost tracking) run regardless.

The TTS messages themselves are context-aware: they include the session label (derived from cwd), an optional `ENGINEER_NAME` env var prefix, and a short todo summary. So if enabled, you'll hear something like *"Stevie, agents-in-a-box is waiting for input. 3 todos complete, 2 remaining"* rather than a generic "your agent needs input".

### Provider priority

Each TTS-emitting hook resolves a provider script via this priority chain:

1. ElevenLabs — if `ELEVENLABS_API_KEY` is set
2. OpenAI — if `OPENAI_API_KEY` is set
3. Cross-platform — `say` on macOS, `espeak` etc. on Linux
4. pyttsx3 — pure Python fallback

If none resolve, the hook silently returns. TTS errors are also swallowed silently.

## Langfuse tracing (opt-in)

`stop.py` and `session_start.py` integrate with Langfuse for span-level tracing of session lifecycle. It's a no-op unless explicitly enabled:

```bash
export LANGFUSE_ENABLED=true
export LANGFUSE_PUBLIC_KEY=...
export LANGFUSE_SECRET_KEY=...
export LANGFUSE_HOST=...   # optional, defaults to cloud
```

See `utils/langfuse/` for the tracer implementation.

## Sync to user-level

These canonical files are copied to `~/.claude/hooks/` either by the toolkit installer or manually (e.g. via the `sync-learnings` skill workflow). When you edit a hook here, plan to sync to `~/.claude/hooks/` on each user machine that should receive the change — `settings.json` references the user-level paths.
