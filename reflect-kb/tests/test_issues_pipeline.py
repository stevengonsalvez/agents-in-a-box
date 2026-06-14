"""End-to-end tests for the transcripts -> issues pipeline.

Everything network/model-shaped is injected: the analyzer returns canned
candidates, and ``gh`` is a fake that records calls. This proves the success
criteria without auth or a model:

* a run produces candidate issues from queued transcripts and files them;
* a second run files NO duplicates (idempotent via the local ledger);
* ``--dry-run`` prints the exact bodies and never invokes ``gh``;
* sanitization runs before anything leaves the pipeline.
"""

from __future__ import annotations

import json
import subprocess


from reflect_kb.issues.dedupe import CandidateIssue
from reflect_kb.issues.pipeline import run_issues


def _transcript(tmp_path, name: str, *, user: str, assistant: str) -> str:
    """Write a minimal Claude-style JSONL transcript and return its path."""
    p = tmp_path / name
    lines = [
        json.dumps(
            {
                "uuid": "u-1",
                "timestamp": "2026-06-14T10:00:00Z",
                "message": {"role": "user", "content": user},
            }
        ),
        json.dumps(
            {"message": {"role": "assistant", "content": [{"type": "text", "text": assistant}]}}
        ),
        json.dumps(
            {
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "name": "Bash", "input": {"command": "cargo test"}}
                    ],
                }
            }
        ),
    ]
    p.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return str(p)


def _queue(tmp_path, transcript_paths: list[str]):
    qf = tmp_path / "pending_reflections.jsonl"
    with open(qf, "w", encoding="utf-8") as fh:
        for i, tp in enumerate(transcript_paths):
            fh.write(
                json.dumps(
                    {
                        "ts": f"2026-06-14T10:0{i}:00",
                        "session_id": f"sess-{i}",
                        "transcript_path": tp,
                        "trigger": "stop",
                        "cwd": str(tmp_path),
                    }
                )
                + "\n"
            )
    return qf


class _FakeGh:
    """Fake gh runner: records create calls, returns a synthetic issue URL."""

    def __init__(self, existing_titles=None):
        self.calls: list[list[str]] = []
        self.created: list[str] = []
        self._n = 100
        self._existing = existing_titles or []

    def __call__(self, cmd):
        self.calls.append(cmd)
        if cmd[:3] == ["gh", "issue", "list"]:
            rows = [{"title": t} for t in self._existing]
            return subprocess.CompletedProcess(cmd, 0, stdout=json.dumps(rows), stderr="")
        if cmd[:3] == ["gh", "label", "list"]:
            return subprocess.CompletedProcess(
                cmd, 0, stdout='[{"name": "bug"}, {"name": "cli"}]', stderr=""
            )
        if cmd[:3] == ["gh", "issue", "create"]:
            title = cmd[cmd.index("--title") + 1]
            self.created.append(title)
            self._n += 1
            url = f"https://github.com/o/r/issues/{self._n}"
            return subprocess.CompletedProcess(cmd, 0, stdout=url + "\n", stderr="")
        return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")


def _analyzer(candidates):
    def fn(timelines):
        assert timelines, "analyzer should receive distilled timelines"
        return list(candidates), "ok"

    return fn


def test_dry_run_prints_bodies_and_never_calls_gh(tmp_path):
    tp = _transcript(tmp_path, "a.jsonl", user="cli crashes", assistant="reproduced")
    qf = _queue(tmp_path, [tp])
    fake_gh = _FakeGh()

    result = run_issues(
        dry_run=True,
        queue=qf,
        ledger_path=tmp_path / "filed.json",
        analyze_fn=_analyzer(
            [
                CandidateIssue(
                    title="CLI crashes on missing config",
                    body="## Summary\nIt crashes.",
                    labels=["bug", "cli"],
                ),
            ]
        ),
        gh_runner=fake_gh,
        fetch_titles=lambda: [],
    )

    assert result.dry_run is True
    assert len(result.previews) == 1
    assert "CLI crashes on missing config" in result.previews[0]
    # gh issue create was NEVER invoked.
    assert not any(c[:3] == ["gh", "issue", "create"] for c in fake_gh.calls)
    # Nothing written to the ledger on a dry run.
    assert not (tmp_path / "filed.json").exists()


def test_run_files_then_second_run_files_no_duplicates(tmp_path):
    tp = _transcript(tmp_path, "a.jsonl", user="drain double files", assistant="confirmed")
    qf = _queue(tmp_path, [tp])
    ledger = tmp_path / "filed.json"
    cand = CandidateIssue(
        title="Reflect drain double-files issues",
        body="## Summary\nDuplicates filed.",
        labels=["bug"],
    )

    fake_gh1 = _FakeGh()
    first = run_issues(
        dry_run=False,
        queue=qf,
        ledger_path=ledger,
        analyze_fn=_analyzer([cand]),
        gh_runner=fake_gh1,
        fetch_titles=lambda: [],
    )
    assert first.filed_count == 1
    # Filed with the default "reflect: " provenance prefix.
    assert fake_gh1.created == ["reflect: Reflect drain double-files issues"]
    assert ledger.exists()

    # Second run: same candidate. The local ledger must suppress it.
    fake_gh2 = _FakeGh()
    second = run_issues(
        dry_run=False,
        queue=qf,
        ledger_path=ledger,
        analyze_fn=_analyzer([cand]),
        gh_runner=fake_gh2,
        fetch_titles=lambda: [],
    )
    assert second.filed_count == 0
    assert fake_gh2.created == []
    assert any(d.reason == "dup-in-ledger" for d in second.skipped)


def test_existing_github_issue_suppresses_filing(tmp_path):
    tp = _transcript(tmp_path, "a.jsonl", user="x", assistant="y")
    qf = _queue(tmp_path, [tp])
    cand = CandidateIssue(title="Sanitizer drops slack tokens", body="b", labels=["bug"])

    fake_gh = _FakeGh()
    result = run_issues(
        dry_run=False,
        queue=qf,
        ledger_path=tmp_path / "filed.json",
        analyze_fn=_analyzer([cand]),
        gh_runner=fake_gh,
        # An existing issue reflect filed earlier carries the "reflect: " prefix;
        # the decorated candidate fingerprints to the same slug and is suppressed.
        fetch_titles=lambda: ["reflect: Sanitizer drops slack tokens"],
    )
    assert result.filed_count == 0
    assert any(d.reason == "dup-on-github" for d in result.skipped)


def test_candidate_is_sanitized_before_filing(tmp_path):
    tp = _transcript(tmp_path, "a.jsonl", user="x", assistant="y")
    qf = _queue(tmp_path, [tp])
    # Token-shaped strings are assembled at runtime so this source file holds no
    # verbatim secret literal (GitHub push-protection trips on those).
    anthropic_prefix = "sk-ant-"
    github_prefix = "gh" + "p_"
    leaky = CandidateIssue(
        title=f"Crash at /Users/stevie/proj when token {anthropic_prefix}abcdef0123456789abcdef0123 used",
        body=f"contact jonny@shotclubhouse.com and server 10.1.2.3 with {github_prefix}" + "a" * 36,
        labels=["bug"],
    )
    fake_gh = _FakeGh()
    result = run_issues(
        dry_run=False,
        queue=qf,
        ledger_path=tmp_path / "filed.json",
        analyze_fn=_analyzer([leaky]),
        gh_runner=fake_gh,
        fetch_titles=lambda: [],
    )
    assert result.filed_count == 1
    create_call = [c for c in fake_gh.calls if c[:3] == ["gh", "issue", "create"]][0]
    blob = " ".join(create_call)
    # None of the secrets/PII survive into the gh invocation.
    assert "/Users/stevie" not in blob
    assert anthropic_prefix not in blob
    assert "jonny@shotclubhouse.com" not in blob
    assert "10.1.2.3" not in blob
    assert github_prefix not in blob


def test_residual_audit_flags_are_surfaced_on_result(tmp_path):
    # A base64-ish blob the sanitizer can't redact must still reach the run
    # result's audit so a reviewer (or the CLI) sees it — the audit must NOT be
    # discarded the way it was before _sanitize_candidate dropped it.
    tp = _transcript(tmp_path, "a.jsonl", user="x", assistant="y")
    qf = _queue(tmp_path, [tp])
    blob = "Zm9vYmFyYmF6" * 5  # 60 chars base64-ish, no known secret shape
    cand = CandidateIssue(
        title="New finding with a suspicious blob",
        body=f"## Summary\nresidual data: {blob}",
        labels=["bug"],
    )
    result = run_issues(
        dry_run=True,
        queue=qf,
        ledger_path=tmp_path / "filed.json",
        analyze_fn=_analyzer([cand]),
        gh_runner=_FakeGh(),
        fetch_titles=lambda: [],
    )
    kinds = {f["kind"] for f in result.audit}
    assert "possible_base64_blob" in kinds
    # Each finding is attributed to the candidate it came from.
    assert all("candidate" in f for f in result.audit)


def test_ledger_saved_incrementally_after_each_file(tmp_path):
    # The ledger must be persisted after EACH successful file, so a crash
    # mid-loop still records the issues already filed. Simulate a crash on the
    # 2nd create: the 1st must already be on disk.
    tp = _transcript(tmp_path, "a.jsonl", user="x", assistant="y")
    qf = _queue(tmp_path, [tp])
    ledger = tmp_path / "filed.json"
    cands = [
        CandidateIssue(title="First brand new finding", body="b", labels=["bug"]),
        CandidateIssue(title="Second brand new finding", body="b", labels=["bug"]),
    ]

    from reflect_kb.issues.dedupe import load_ledger

    class _CrashOnSecondCreate(_FakeGh):
        def __call__(self, cmd):
            if cmd[:3] == ["gh", "issue", "create"] and len(self.created) >= 1:
                raise subprocess.CalledProcessError(1, cmd, stderr="boom")
            return super().__call__(cmd)

    fake_gh = _CrashOnSecondCreate()
    result = run_issues(
        dry_run=False,
        queue=qf,
        ledger_path=ledger,
        analyze_fn=_analyzer(cands),
        gh_runner=fake_gh,
        fetch_titles=lambda: [],
    )
    # First filed, second failed — but the first is already persisted.
    assert result.filed_count == 1
    assert ledger.exists()
    saved = load_ledger(ledger)
    fps = {e["fingerprint"] for e in saved["filed_issues"]}
    from reflect_kb.issues.dedupe import fingerprint

    # Ledger records the fingerprint of the decorated (prefixed) title.
    assert fingerprint("reflect: First brand new finding") in fps


def test_empty_queue_returns_clean_result(tmp_path):
    result = run_issues(
        dry_run=True, queue=tmp_path / "nonexistent.jsonl", ledger_path=tmp_path / "filed.json"
    )
    assert result.transcripts_seen == 0
    assert result.filed_count == 0
    assert any("no transcripts" in n for n in result.notes)


def test_no_candidates_records_reason(tmp_path):
    tp = _transcript(tmp_path, "a.jsonl", user="x", assistant="y")
    qf = _queue(tmp_path, [tp])
    result = run_issues(
        dry_run=True,
        queue=qf,
        ledger_path=tmp_path / "filed.json",
        analyze_fn=lambda timelines: ([], "no-signal"),
        gh_runner=_FakeGh(),
        fetch_titles=lambda: [],
    )
    assert result.candidates == 0
    assert result.analyze_reason == "no-signal"


def test_unknown_labels_are_filtered_out(tmp_path):
    tp = _transcript(tmp_path, "a.jsonl", user="x", assistant="y")
    qf = _queue(tmp_path, [tp])
    # "frobnicate" is not a real repo label; _FakeGh only knows bug/cli.
    cand = CandidateIssue(title="Some new finding", body="b", labels=["bug", "frobnicate"])
    fake_gh = _FakeGh()
    run_issues(
        dry_run=False,
        queue=qf,
        ledger_path=tmp_path / "filed.json",
        analyze_fn=_analyzer([cand]),
        gh_runner=fake_gh,
        fetch_titles=lambda: [],
    )
    create_call = [c for c in fake_gh.calls if c[:3] == ["gh", "issue", "create"]][0]
    label_idx = create_call.index("--label") + 1
    assert "frobnicate" not in create_call[label_idx]
    assert "bug" in create_call[label_idx]


class _LabelTrackingGh:
    """gh fake that learns labels created via ``gh label create`` so a later
    ``gh label list`` reports them (mirrors real gh behaviour)."""

    def __init__(self, initial_labels=None, existing_titles=None):
        self.calls: list[list[str]] = []
        self.created: list[str] = []
        self._labels = set(initial_labels or [])
        self._existing = existing_titles or []
        self._n = 200

    def __call__(self, cmd):
        self.calls.append(cmd)
        if cmd[:3] == ["gh", "issue", "list"]:
            rows = [{"title": t} for t in self._existing]
            return subprocess.CompletedProcess(cmd, 0, stdout=json.dumps(rows), stderr="")
        if cmd[:3] == ["gh", "label", "create"]:
            self._labels.add(cmd[3])
            return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")
        if cmd[:3] == ["gh", "label", "list"]:
            rows = [{"name": n} for n in sorted(self._labels)]
            return subprocess.CompletedProcess(cmd, 0, stdout=json.dumps(rows), stderr="")
        if cmd[:3] == ["gh", "issue", "create"]:
            self.created.append(cmd[cmd.index("--title") + 1])
            self._n += 1
            url = f"https://github.com/o/r/issues/{self._n}"
            return subprocess.CompletedProcess(cmd, 0, stdout=url + "\n", stderr="")
        return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")


def test_filed_issue_gets_reflect_prefix_and_label(tmp_path):
    # Default provenance: every filed issue is titled "reflect: ..." and the
    # reflect label is auto-created (so it survives _filter_labels) and applied.
    tp = _transcript(tmp_path, "a.jsonl", user="x", assistant="y")
    qf = _queue(tmp_path, [tp])
    cand = CandidateIssue(title="CLI crashes on missing config", body="b", labels=["bug"])
    gh = _LabelTrackingGh(initial_labels={"bug"})
    run_issues(
        dry_run=False,
        queue=qf,
        ledger_path=tmp_path / "filed.json",
        analyze_fn=_analyzer([cand]),
        gh_runner=gh,
        fetch_titles=lambda: [],
    )
    # The reflect label was auto-created.
    assert any(c[:3] == ["gh", "label", "create"] and c[3] == "reflect" for c in gh.calls)
    create = [c for c in gh.calls if c[:3] == ["gh", "issue", "create"]][0]
    title = create[create.index("--title") + 1]
    labels = create[create.index("--label") + 1]
    assert title == "reflect: CLI crashes on missing config"
    assert "reflect" in labels.split(",")
    assert "bug" in labels.split(",")  # analyzer labels preserved alongside


def test_reflect_prefix_is_idempotent(tmp_path):
    # An analyzer that already emits a "reflect: " title must not be double-prefixed.
    tp = _transcript(tmp_path, "a.jsonl", user="x", assistant="y")
    qf = _queue(tmp_path, [tp])
    cand = CandidateIssue(title="reflect: already prefixed", body="b", labels=[])
    gh = _LabelTrackingGh()
    run_issues(
        dry_run=False,
        queue=qf,
        ledger_path=tmp_path / "filed.json",
        analyze_fn=_analyzer([cand]),
        gh_runner=gh,
        fetch_titles=lambda: [],
    )
    title = gh.created[0]
    assert title == "reflect: already prefixed"
    assert not title.startswith("reflect: reflect:")


def test_provenance_can_be_disabled(tmp_path):
    # Empty title_prefix + label opt out entirely (no prefix, no reflect label).
    tp = _transcript(tmp_path, "a.jsonl", user="x", assistant="y")
    qf = _queue(tmp_path, [tp])
    cand = CandidateIssue(title="Plain finding", body="b", labels=["bug"])
    gh = _LabelTrackingGh(initial_labels={"bug"})
    run_issues(
        dry_run=False,
        queue=qf,
        ledger_path=tmp_path / "filed.json",
        analyze_fn=_analyzer([cand]),
        gh_runner=gh,
        fetch_titles=lambda: [],
        title_prefix="",
        label="",
    )
    assert gh.created[0] == "Plain finding"
    assert not any(c[:3] == ["gh", "label", "create"] for c in gh.calls)


def test_reflect_prefix_shows_in_dry_run_preview(tmp_path):
    # The dry-run preview must show exactly what would be filed — prefix included.
    tp = _transcript(tmp_path, "a.jsonl", user="x", assistant="y")
    qf = _queue(tmp_path, [tp])
    cand = CandidateIssue(title="Preview finding", body="b", labels=[])
    gh = _LabelTrackingGh()
    result = run_issues(
        dry_run=True,
        queue=qf,
        ledger_path=tmp_path / "filed.json",
        analyze_fn=_analyzer([cand]),
        gh_runner=gh,
        fetch_titles=lambda: [],
    )
    assert "reflect: Preview finding" in result.previews[0]
    assert "reflect" in result.previews[0]  # label shown in the preview bracket
