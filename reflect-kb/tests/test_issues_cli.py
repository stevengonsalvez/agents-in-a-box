"""CLI smoke tests for ``reflect issues`` via click's CliRunner.

These confirm the command is wired into the ``reflect`` group and that the
``--dry-run`` JSON contract is stable (the orchestration itself is covered in
depth by test_issues_pipeline.py).
"""

from __future__ import annotations

import json

from click.testing import CliRunner

from reflect_kb.cli.learnings_cli import cli


def test_issues_group_is_registered():
    result = CliRunner().invoke(cli, ["issues", "--help"])
    assert result.exit_code == 0
    assert "run" in result.output
    assert "ledger" in result.output
    assert "queue" in result.output


def test_map_flag_rejects_bad_syntax(monkeypatch, tmp_path):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    result = CliRunner().invoke(cli, ["issues", "run", "--dry-run", "--map", "no-equals"])
    assert result.exit_code != 0
    assert "KEY=VALUE" in result.output


def test_dry_run_empty_queue_json(monkeypatch, tmp_path):
    # Point the state dir at an empty tmp so there is no queue -> clean result.
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    result = CliRunner().invoke(cli, ["issues", "run", "--dry-run", "-f", "json"])
    assert result.exit_code == 0
    payload = json.loads(result.output)
    assert payload["dry_run"] is True
    assert payload["transcripts_seen"] == 0
    assert payload["filed"] == []


def test_ledger_empty_json(monkeypatch, tmp_path):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    result = CliRunner().invoke(cli, ["issues", "ledger", "-f", "json"])
    assert result.exit_code == 0
    payload = json.loads(result.output)
    assert payload["filed_issues"] == []
