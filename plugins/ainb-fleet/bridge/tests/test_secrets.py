"""Secret resolver: env refs, keychain refs, plain strings."""

from __future__ import annotations

import subprocess

from ainb_phone_bridge.secrets import resolve_secret


def test_plain_string_passthrough():
    assert resolve_secret("1234:ABCDEF") == "1234:ABCDEF"


def test_empty_and_whitespace():
    assert resolve_secret("") == ""
    assert resolve_secret("   ") == ""


def test_env_dollar_form(monkeypatch):
    monkeypatch.setenv("MY_TOKEN", "secret-value")
    assert resolve_secret("$MY_TOKEN") == "secret-value"


def test_env_brace_form(monkeypatch):
    monkeypatch.setenv("MY_TOKEN", "brace-value")
    assert resolve_secret("${MY_TOKEN}") == "brace-value"


def test_env_unset_returns_empty(monkeypatch):
    monkeypatch.delenv("DEFINITELY_UNSET_XYZ", raising=False)
    assert resolve_secret("$DEFINITELY_UNSET_XYZ") == ""


def test_non_string_returns_empty():
    assert resolve_secret(None) == ""  # type: ignore[arg-type]
    assert resolve_secret(12345) == ""  # type: ignore[arg-type]


def test_keychain_success(monkeypatch):
    def fake_run(cmd, **kwargs):
        assert cmd[:3] == ["/usr/bin/security", "find-generic-password", "-s"]
        assert cmd[3] == "tg-bot"
        return subprocess.CompletedProcess(cmd, 0, stdout="kc-token\n", stderr="")

    monkeypatch.setattr(subprocess, "run", fake_run)
    assert resolve_secret("keychain:tg-bot") == "kc-token"


def test_keychain_miss_returns_empty(monkeypatch):
    def fake_run(cmd, **kwargs):
        return subprocess.CompletedProcess(cmd, 44, stdout="", stderr="not found")

    monkeypatch.setattr(subprocess, "run", fake_run)
    assert resolve_secret("keychain:nope") == ""


def test_keychain_empty_service():
    assert resolve_secret("keychain:") == ""


def test_keychain_subprocess_error(monkeypatch):
    def boom(cmd, **kwargs):
        raise OSError("security binary missing")

    monkeypatch.setattr(subprocess, "run", boom)
    assert resolve_secret("keychain:svc") == ""
