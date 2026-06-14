"""CLI smoke tests for ``reflect issues`` via click's CliRunner.

These confirm the command is wired into the ``reflect`` group and that the
``--dry-run`` JSON contract is stable (the orchestration itself is covered in
depth by test_issues_pipeline.py).
"""

from __future__ import annotations

import json

import pytest
from click.testing import CliRunner

from reflect_kb import reflect_config
from reflect_kb.cli.learnings_cli import cli


@pytest.fixture(autouse=True)
def _clear_config_cache():
    # load_config is lru_cached; clear around every test so a config set by one
    # test (via REFLECT_CONFIG) never bleeds into the next.
    reflect_config.load_config.cache_clear()
    yield
    reflect_config.load_config.cache_clear()


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


def test_issues_config_limit_override_is_honored(monkeypatch, tmp_path):
    # A [issues].limit in reflect.toml must be used as the default when --limit
    # is unset, instead of the hard-coded 20.
    from reflect_kb import reflect_config
    from reflect_kb.cli import issues_cli

    cfg = tmp_path / "reflect.toml"
    cfg.write_text('[issues]\nlimit = 7\nmodel = "opus"\n', encoding="utf-8")
    monkeypatch.setenv("REFLECT_CONFIG", str(cfg))
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    reflect_config.load_config.cache_clear()

    captured: dict = {}

    def fake_run_issues(**kwargs):
        captured.update(kwargs)
        from reflect_kb.issues.pipeline import IssuesRunResult

        return IssuesRunResult(dry_run=True)

    monkeypatch.setattr(issues_cli, "run_issues", fake_run_issues)

    result = CliRunner().invoke(cli, ["issues", "run", "--dry-run", "-f", "json"])
    assert result.exit_code == 0, result.output
    assert captured["limit"] == 7
    assert captured["model"] == "opus"

    reflect_config.load_config.cache_clear()


def test_explicit_limit_flag_overrides_config(monkeypatch, tmp_path):
    from reflect_kb import reflect_config
    from reflect_kb.cli import issues_cli

    cfg = tmp_path / "reflect.toml"
    cfg.write_text("[issues]\nlimit = 7\n", encoding="utf-8")
    monkeypatch.setenv("REFLECT_CONFIG", str(cfg))
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    reflect_config.load_config.cache_clear()

    captured: dict = {}

    def fake_run_issues(**kwargs):
        captured.update(kwargs)
        from reflect_kb.issues.pipeline import IssuesRunResult

        return IssuesRunResult(dry_run=True)

    monkeypatch.setattr(issues_cli, "run_issues", fake_run_issues)

    result = CliRunner().invoke(cli, ["issues", "run", "--dry-run", "--limit", "3", "-f", "json"])
    assert result.exit_code == 0, result.output
    assert captured["limit"] == 3

    reflect_config.load_config.cache_clear()
