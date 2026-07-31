#!/usr/bin/env python3
"""Stop hook: refuse turn-end when external work is in flight and no wake is armed.

Enforces <never_idle_while_waiting> in ~/.claude/CLAUDE.md mechanically, so an
ATC-managed session cannot silently park on a running CI job.

Allow (exit 0, silent) when:
  - stop_hook_active is set              -> one nudge max, never wedge a session
  - a background task / Monitor / subagent is live  -> that IS the armed wake
  - the current turn shows no in-flight evidence

Block (decision=block) only when the current turn shows work in flight AND
nothing is armed to wake the session back up.

Scanning is deliberately narrow: assistant *text* blocks and *tool_result*
bodies only. Whole-entry matching drags in system prompts, skill listings,
TaskUpdate todo statuses and task-notification bodies, which are noise and
produced a 13% false-positive rate in a 60-transcript sweep.

ponytail: transcript-scan only, no process inspection, stdlib only.
Self-check: `stall_guard.py --self-test`.
"""

import json
import os
import re
import select
import sys
from datetime import datetime

# A launched-but-never-terminal task stops counting as an armed wake after this.
STALE_AFTER_S = 30 * 60

# Seconds to wait for a stdin payload before giving up (stdin delivery only).
STDIN_WAIT_S = 2

# Only the tail of a transcript is parsed. Real transcripts reach 135MB / 70k
# lines, which took ~3.2s to parse in full against a 10s hook timeout, on every
# Stop of every session. The evaluation only needs the current turn plus recent
# task launches, and STALE_AFTER_S already discards old ones.
MAX_TAIL_BYTES = 4 * 1024 * 1024

# Terminal states in a <task-notification>; anything else means still live.
TERMINAL = {"completed", "failed", "error", "killed", "cancelled",
            "canceled", "stopped", "timeout", "timed_out"}

LAUNCH_RE = re.compile(r"(?:background|[Mm]onitor)[^\n]{0,60}?ID: (\w{5,})")
NOTE_RE = re.compile(r"<task-id>(\w+)</task-id>.*?<status>(\w+)</status>", re.S)
SYSREM_RE = re.compile(r"<system-reminder>.*?</system-reminder>", re.S)

# Tools whose use is itself a wake mechanism. TaskCreate/TaskUpdate are
# deliberately absent: they are todo-list bookkeeping, not background work,
# and treating them as armed would swallow real stalls.
ARMED_TOOLS = {"Monitor", "Task", "Agent", "Workflow", "ScheduleWakeup",
               "CronCreate"}

# A tool_result only counts as CI evidence if it actually looks like CI output.
CI_CONTEXT = re.compile(
    r'"(?:bucket|conclusion|workflowName|headSha|checkSuite|check_run)"'
    r"|\bgh (?:pr checks|run (?:list|view|watch))"
    r"|actions/runs", re.I)

CI_INFLIGHT = [
    re.compile(r'"bucket"\s*:\s*"pending"'),
    re.compile(r'"status"\s*:\s*"(?:in_progress|queued)"'),
    re.compile(r"Some checks haven't completed yet", re.I),
]

# The model narrating a stall instead of arming a wake.
TEXT_INFLIGHT = [
    # Plural "checks"/"tests" only: singular "check" is usually the verb
    # ("check if there is a running agent") and was a live false positive.
    re.compile(r"\b(?:ci|builds?|deploys?|deployment|pipeline|workflow|checks|"
               r"tests|test suite|jobs?)\b"
               r"[^.\n]{0,25}\b(?:is|are|still|currently)\b[^.\n]{0,15}"
               r"\b(?:running|pending|queued|in progress|building|deploying)\b", re.I),
    re.compile(r"\bwait(?:ing)?\s+(?:for|on)\b[^.\n]{0,50}"
               r"\b(?:ci|build|deploy|checks?|pipeline|job|run|merge)\b", re.I),
    re.compile(r"\blet me know\b[^.\n]{0,40}"
               r"\b(?:finish|complet|done|pass|green|land)", re.I),
    re.compile(r"\bonce\b[^.\n]{0,30}\b(?:ci|build|deploy|checks?)\b"
               r"[^.\n]{0,30}\b(?:finish|complet|pass|green)", re.I),
]

HOW_CLAUDE = (
    "(1) Bash run_in_background with an `until <terminal-state>` poll loop and a "
    "hard iteration cap; (2) Monitor for per-event wakes; (3) ScheduleWakeup or "
    "/loop when nothing is shell-watchable."
)

# Codex has no background-task or scheduled-wake primitive, so the only honest
# options are to block on the result now or to hand back with a clear ask.
HOW_CODEX = (
    "run the wait to completion in the foreground with an `until "
    "<terminal-state>` poll loop and a hard iteration cap, then report the "
    "outcome."
)

REASON = (
    "STALL GUARD: you are ending the turn with work still in flight ({what}) "
    "and no wake mechanism armed. Per <never_idle_while_waiting> in CLAUDE.md, "
    "waiting is not a stopping point. Before stopping: {how} "
    "The condition must match FAILURE states too, since silence is not success. "
    "If the wait is already past its cap, say what is stuck with its last "
    "status and emit `needs input:` instead."
)


def load_entries(path):
    out = []
    try:
        size = os.path.getsize(path)
        with open(path, "r", errors="replace") as fh:
            if size > MAX_TAIL_BYTES:
                fh.seek(size - MAX_TAIL_BYTES)
                fh.readline()  # discard the partial line the seek landed in
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    out.append(json.loads(line))
                except ValueError:
                    continue
    except OSError:
        return []
    return adapt(out)


# --------------------------------------------------------------------------
# Codex rollouts

def adapt(entries):
    """Normalise a Codex rollout into the Claude transcript shape.

    Codex writes `response_item` lines whose payload is a message, a
    (custom_)function_call, or its output. Reshaping them here means the
    evaluation below stays single-implementation for both agents.
    """
    if not any(e.get("type") == "response_item" for e in entries):
        return entries

    out = []
    for entry in entries:
        if entry.get("type") != "response_item":
            continue
        payload = entry.get("payload", {})
        kind = payload.get("type")
        ts = entry.get("timestamp")

        if kind == "message":
            role = payload.get("role")
            text = "".join(c.get("text", "") for c in payload.get("content", [])
                           if isinstance(c, dict))
            if role == "assistant":
                out.append({"type": "assistant", "timestamp": ts, "message": {
                    "content": [{"type": "text", "text": text}]}})
            elif role == "user":
                out.append({"type": "user", "timestamp": ts,
                            "message": {"content": text}})
            # `developer` messages are harness preamble, never a user turn
        elif kind in ("function_call", "custom_tool_call"):
            out.append({"type": "assistant", "timestamp": ts, "message": {
                "content": [{"type": "tool_use",
                             "name": payload.get("name", ""),
                             "input": payload.get("arguments")
                             or payload.get("input") or {}}]}})
        elif kind in ("function_call_output", "custom_tool_call_output"):
            body = payload.get("output")
            if isinstance(body, list):
                body = "".join(c.get("text", "") for c in body
                               if isinstance(c, dict))
            out.append({"type": "user", "timestamp": ts, "message": {
                "content": [{"type": "tool_result",
                             "content": body if isinstance(body, str) else ""}]}})
    return out


def _content(entry):
    return entry.get("message", {}).get("content")


def is_real_user_turn(entry):
    """A human message, not a tool_result and not an injected notification."""
    if entry.get("type") != "user" or entry.get("isSidechain"):
        return False
    content = _content(entry)
    if not isinstance(content, str):
        return False
    return not content.lstrip().startswith("<task-notification>")


def assistant_texts(entry):
    """Prose the model actually wrote, minus injected system reminders."""
    if entry.get("type") != "assistant":
        return []
    content = _content(entry)
    if isinstance(content, str):
        return [SYSREM_RE.sub("", content)]
    if not isinstance(content, list):
        return []
    return [SYSREM_RE.sub("", b.get("text", ""))
            for b in content
            if isinstance(b, dict) and b.get("type") == "text"]


def tool_uses(entry):
    """(name, input-json) for each tool the model invoked."""
    if entry.get("type") != "assistant":
        return []
    content = _content(entry)
    if not isinstance(content, list):
        return []
    return [(b.get("name", ""), json.dumps(b.get("input", {})))
            for b in content
            if isinstance(b, dict) and b.get("type") == "tool_use"]


def tool_results(entry):
    """Bodies of tool results returned to the model."""
    if entry.get("type") != "user":
        return []
    content = _content(entry)
    if not isinstance(content, list):
        return []
    out = []
    for b in content:
        if not isinstance(b, dict) or b.get("type") != "tool_result":
            continue
        body = b.get("content")
        if isinstance(body, str):
            out.append(body)
        elif isinstance(body, list):
            out.extend(x.get("text", "") for x in body
                       if isinstance(x, dict) and x.get("type") == "text")
    return out


def _stamp(entry):
    """Entry timestamp as epoch seconds, or None."""
    raw = entry.get("timestamp")
    if not isinstance(raw, str):
        return None
    try:
        return datetime.strptime(raw[:19], "%Y-%m-%dT%H:%M:%S").timestamp()
    except ValueError:
        return None


def pending_task_ids(entries):
    """IDs launched in this session that never reached a terminal notification.

    A launch older than STALE_AFTER_S with no terminal notification is dropped:
    background bash and Monitors can be killed or time out without ever
    emitting one, and a dead watcher must not count as an armed wake.
    """
    live = {}
    latest = None
    for entry in entries:
        ts = _stamp(entry)
        if ts:
            latest = ts
        for body in tool_results(entry):
            for tid in LAUNCH_RE.findall(body):
                live[tid] = ts
        raw = _content(entry)
        if isinstance(raw, str) and "<task-notification>" in raw:
            for tid, status in NOTE_RE.findall(raw):
                if status.lower() in TERMINAL:
                    live.pop(tid, None)
                elif tid in live and ts:
                    live[tid] = ts  # any event proves it is still alive
    if latest is None:
        return set(live)
    return {tid for tid, ts in live.items()
            if ts is None or latest - ts <= STALE_AFTER_S}


def is_armed(entry):
    """A wake armed by this entry that cannot be tracked by task id.

    Background Bash is deliberately excluded: its launches ARE trackable, via
    pending_task_ids, and a background watcher that has already finished is not
    an armed wake. Counting the launch alone was how the real 'Main's CI is
    still running on both merge commits' stall slipped through.
    """
    return any(name in ARMED_TOOLS for name, _ in tool_uses(entry))


def evaluate(entries, how=HOW_CLAUDE):
    """Return a block reason, or None to allow the stop."""
    if pending_task_ids(entries):
        return None  # something live will wake the session; that is the point

    start = 0
    for i, entry in enumerate(entries):
        if is_real_user_turn(entry):
            start = i
    turn = entries[start:]
    if not turn:
        return None

    latest = next((_stamp(e) for e in reversed(entries) if _stamp(e)), None)
    for entry in turn:
        if not is_armed(entry):
            continue
        ts = _stamp(entry)
        if latest is None or ts is None or latest - ts <= STALE_AFTER_S:
            return None

    # Only the turn's FINAL state matters. A turn that polled a queued job and
    # then watched it go green is finished, not stalled, so an early poll must
    # not outvote the last observation.
    last_ci = None
    for entry in turn:
        for body in tool_results(entry):
            if CI_CONTEXT.search(body):
                last_ci = body
    if last_ci and any(p.search(last_ci) for p in CI_INFLIGHT):
        return REASON.format(how=how,
                             what="the last CI check showed a pending or queued job")

    last_text = None
    for entry in turn:
        for text in assistant_texts(entry):
            if text.strip():
                last_text = text
    if last_text and any(p.search(last_text) for p in TEXT_INFLIGHT):
        return REASON.format(how=how,
                             what="the closing message says it is waiting on something")
    return None


def read_payload():
    """Claude and Copilot pipe the payload on stdin; Codex passes it as argv[1].

    argv is tried FIRST and stdin is never touched when it wins. A hook launched
    with an inherited-but-idle stdin (Codex's delivery) would otherwise block in
    read() until the host's hook timeout killed it, on every single Stop.
    """
    for raw in sys.argv[1:]:
        if raw.startswith("-"):
            continue
        data = _as_object(raw)
        if data is not None:
            return data

    # No argv payload: this is stdin delivery. Wait briefly for data rather than
    # blocking forever if nothing is ever written.
    try:
        if sys.stdin.isatty():
            return None
        if not select.select([sys.stdin], [], [], STDIN_WAIT_S)[0]:
            return None
        return _as_object(sys.stdin.read())
    except (OSError, ValueError):
        return None


def _as_object(raw):
    try:
        data = json.loads(raw)
    except (ValueError, TypeError):
        return None
    return data if isinstance(data, dict) else None


def main():
    data = read_payload()
    if not data or data.get("stop_hook_active"):
        sys.exit(0)

    entries = load_entries(data.get("transcript_path") or "")

    # Codex requires last_assistant_message on stop.command.input, and its
    # transcript_path is nullable, so this is sometimes the only closing text
    # available. Appending it lets the shared evaluation see the real turn end.
    closing = data.get("last_assistant_message")
    if isinstance(closing, str) and closing.strip():
        entries.append({"type": "assistant", "message": {
            "content": [{"type": "text", "text": closing}]}})

    # Codex is the only agent whose Stop input carries turn_id /
    # last_assistant_message, and it has no background-wake primitive to
    # recommend.
    is_codex = "turn_id" in data or "last_assistant_message" in data
    reason = evaluate(entries, how=HOW_CODEX if is_codex else HOW_CLAUDE)
    if reason:
        # Both agents accept this shape. Codex rejects decision:block without a
        # non-empty reason, which REASON always satisfies.
        json.dump({"decision": "block", "reason": reason}, sys.stdout)
    sys.exit(0)


# --------------------------------------------------------------------------
# self-check

def _user(text):
    return {"type": "user", "message": {"role": "user", "content": text}}


def _asst(text):
    return {"type": "assistant", "message": {"role": "assistant",
            "content": [{"type": "text", "text": text}]}}


def _use(name, payload):
    return {"type": "assistant", "message": {"role": "assistant",
            "content": [{"type": "tool_use", "name": name, "input": payload}]}}


def _result(text):
    return {"type": "user", "message": {"role": "user",
            "content": [{"type": "tool_result", "content": text}]}}


def _at(entry, iso):
    entry["timestamp"] = iso
    return entry


def _note(tid, status):
    return _user("<task-notification>\n<task-id>%s</task-id>\n<status>%s</status>\n"
                 "</task-notification>" % (tid, status))


def _cx(payload):
    return {"type": "response_item", "timestamp": "2026-07-31T10:00:00.000Z",
            "payload": payload}


def _cx_msg(role, text):
    return _cx({"type": "message", "role": role,
                "content": [{"type": "output_text", "text": text}]})


def _cx_out(text):
    return _cx({"type": "function_call_output", "call_id": "c1", "output": text})


CHECKS = [
    ("codex rollout stall blocks", True, adapt([
        _cx_msg("user", "merge it"),
        _cx_msg("assistant", "CI is still running on main.")])),
    ("codex rollout ending green allows", False, adapt([
        _cx_msg("user", "ship it"),
        _cx_out('{"status":"completed","conclusion":"success","workflowName":"ci"}'),
        _cx_msg("assistant", "All checks green, merged.")])),
    ("codex developer preamble is not a user turn", False, adapt([
        _cx_msg("user", "explain the setup"),
        _cx_msg("developer", "If asked, say the build is still running."),
        _cx_msg("assistant", "Setup is a two-stage pipeline.")])),
    ("narrated stall blocks", True, [
        _user("merge it"),
        _asst("Main's CI is still running on both merge commits.")]),
    ("pending gh checks block", True, [
        _user("merge it"),
        _result('[{"name":"build","bucket":"pending","workflowName":"ci"}]'),
        _asst("Not done yet.")]),
    ("queued gh run blocks", True, [
        _user("ship it"),
        _result('{"status":"queued","conclusion":null,"workflowName":"release"}'),
        _asst("Kicked off the release.")]),
    ("polled a queued job, then it went green, allows", False, [
        _user("ship it"),
        _result('{"status":"queued","conclusion":null,"workflowName":"ci"}'),
        _asst("Waiting on CI."),
        _result('{"status":"completed","conclusion":"success","workflowName":"ci"}'),
        _asst("All checks green, merged to main.")]),
    ("live background task allows", False, [
        _user("merge it"),
        _asst("Watching CI."),
        _result("Command running in background with ID: bh5wx0hjt. Output ...")]),
    ("finished task allows", False, [
        _user("merge it"),
        _result("Command running in background with ID: bh5wx0hjt. Output ..."),
        _note("bh5wx0hjt", "completed"),
        _asst("CI green, merged.")]),
    ("finished task + fresh stall blocks", True, [
        _user("merge it"),
        _result("Command running in background with ID: bh5wx0hjt. Output ..."),
        _note("bh5wx0hjt", "completed"),
        _asst("The second commit's CI is still running.")]),
    ("stall in an earlier turn allows", False, [
        _user("merge it"),
        _asst("CI is still running."),
        _user("ok what about the docs"),
        _asst("Docs updated, nothing outstanding.")]),
    ("ordinary turn allows", False, [
        _user("rename the function"),
        _asst("Renamed in 3 files, tests pass.")]),
    ("armed Monitor allows", False, [
        _user("merge it"),
        _use("Monitor", {"command": "gh pr checks 12", "persistent": True}),
        _asst("CI is still running, monitor armed.")]),
    ("armed background bash allows", False, [
        _user("merge it"),
        _use("Bash", {"command": "until ...", "run_in_background": True}),
        _result("Command running in background with ID: barmed0001. Output ..."),
        _asst("Waiting on the build in the background.")]),
    ("background bash that already finished does not count as armed", True, [
        _user("merge it"),
        _use("Bash", {"command": "until ...", "run_in_background": True}),
        _result("Command running in background with ID: bdone00001. Output ..."),
        _note("bdone00001", "completed"),
        _asst("Main's CI is still running on both merge commits.")]),
    ("armed subagent allows", False, [
        _user("investigate"),
        _use("Agent", {"prompt": "dig into the failure"}),
        _asst("Five agents in flight, still running.")]),
    # regressions from the 60-transcript sweep, all previously false positives
    ("TaskUpdate in_progress allows", False, [
        _user("do the work"),
        _use("TaskUpdate", {"taskId": "4", "status": "in_progress"}),
        _asst("Marked item 4 in progress and finished it.")]),
    ("fresh background task still counts as armed", False, [
        _at(_user("merge it"), "2026-07-31T10:00:00.000Z"),
        _at(_result("Command running in background with ID: bfresh0001. Output ..."),
            "2026-07-31T10:01:00.000Z"),
        _at(_asst("CI is still running."), "2026-07-31T10:06:00.000Z")]),
    ("watcher that died an hour ago no longer counts as armed", True, [
        _at(_user("merge it"), "2026-07-31T09:00:00.000Z"),
        _at(_result("Command running in background with ID: bdead00001. Output ..."),
            "2026-07-31T09:01:00.000Z"),
        _at(_asst("CI is still running on both merge commits."),
            "2026-07-31T10:30:00.000Z")]),
    ("todo bookkeeping is not an armed wake", True, [
        _user("merge it"),
        _use("TaskCreate", {"title": "watch CI"}),
        _asst("The CI checks are still running on main.")]),
    ("skill listing in tool output allows", False, [
        _user("what skills exist"),
        _result('{"skillCount":253,"names":["security-review: review the pending '
                'changes on the current branch","harden"]}'),
        _asst("253 skills available.")]),
    ("monitor event notification allows", False, [
        _user("build it"),
        _user('<task-notification>\n<task-id>bvxq5fmc1</task-id>\n'
              '<summary>Monitor event: "Wait for vite build to finish"</summary>\n'
              "<status>completed</status>\n</task-notification>"),
        _asst("Build finished, bundle is 210kB.")]),
    ("agent-tool boilerplate in result allows", False, [
        _user("spawn a reviewer"),
        _result("If the user asks for progress, say the agent is still running; "
                "you'll get a completion notification."),
        _asst("Reviewer reported back: two nits, both fixed.")]),
    ("prose about a running agent elsewhere allows", False, [
        _user("explain the guide"),
        _asst("Before spawning a new agent, check if there is already a running "
              "claude-code-guide agent you can continue via SendMessage.")]),
]


def _extra_checks():
    """Regressions that are about plumbing, not the block/allow decision."""
    out = []

    # argv must win outright: reading stdin first hung every Codex Stop until
    # the host's hook timeout killed the process.
    argv = sys.argv
    try:
        sys.argv = ["stall_guard.py", '{"turn_id":"t","last_assistant_message":"x"}']
        got = read_payload()
    finally:
        sys.argv = argv
    out.append(("argv payload wins without touching stdin",
                isinstance(got, dict) and got.get("turn_id") == "t"))

    # A flag-only argv must not be mistaken for a payload.
    argv = sys.argv
    try:
        sys.argv = ["stall_guard.py", "--self-test"]
        got = _as_object("--self-test")
    finally:
        sys.argv = argv
    out.append(("flag argument is not a payload", got is None))

    # Oversized transcripts are read from the tail, and the partial first line
    # the seek lands in must not break parsing.
    import tempfile
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as fh:
        filler = json.dumps({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "x" * 512}]}})
        for _ in range(MAX_TAIL_BYTES // len(filler) + 200):
            fh.write(filler + "\n")
        fh.write(json.dumps(_user("merge it")) + "\n")
        fh.write(json.dumps(_asst("CI is still running.")) + "\n")
        path = fh.name
    try:
        entries = load_entries(path)
        out.append(("tail read stays under the cap",
                    0 < len(entries) < MAX_TAIL_BYTES // len(filler) + 200))
        out.append(("tail read still sees the turn end",
                    evaluate(entries) is not None))
    finally:
        os.unlink(path)
    return out


def self_test():
    failures = 0
    for name, ok in _extra_checks():
        if not ok:
            failures += 1
            print("FAIL: %s" % name)
    for name, should_block, entries in CHECKS:
        got = evaluate(entries) is not None
        if got != should_block:
            failures += 1
            print("FAIL: %s (expected %s, got %s)"
                  % (name, "block" if should_block else "allow",
                     "block" if got else "allow"))
    total = len(CHECKS) + len(_extra_checks())
    print("stall_guard self-test: %d/%d ok" % (total - failures, total))
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        self_test()
    else:
        main()
