"""Service unit generation: venv python path, no token on argv, idempotency."""

from __future__ import annotations

from pathlib import Path

from ainb_phone_bridge import service
from ainb_phone_bridge.service import (
    ServicePaths,
    _build_plist,
    _daemon_argv,
    build_systemd_unit,
)


def _paths(tmp_path: Path, *, with_venv: bool) -> ServicePaths:
    venv_python = tmp_path / "venv" / "bin" / "python3"
    if with_venv:
        venv_python.parent.mkdir(parents=True, exist_ok=True)
        venv_python.write_text("#!/bin/sh\n")
    return ServicePaths(
        package_dir=tmp_path,
        venv_python=venv_python,
        log_path=tmp_path / "phone-bridge.log",
    )


def test_daemon_argv_prefers_venv_python(tmp_path):
    paths = _paths(tmp_path, with_venv=True)
    argv = _daemon_argv(paths)
    assert argv[0] == str(paths.venv_python)
    assert argv[0].endswith("venv/bin/python3")  # NO leading dot
    assert ".venv" not in argv[0]
    assert argv[1:] == ["-m", "ainb_phone_bridge", "run"]


def test_daemon_argv_falls_back_when_no_venv(tmp_path):
    paths = _paths(tmp_path, with_venv=False)
    argv = _daemon_argv(paths)
    # Falls back to a real interpreter, still no token.
    assert argv[1:] == ["-m", "ainb_phone_bridge", "run"]


def test_no_token_anywhere_in_argv(tmp_path):
    paths = _paths(tmp_path, with_venv=True)
    argv = _daemon_argv(paths)
    # The interpreter path is environment-controlled (tmp dirs can contain the
    # substring "token"); the token-leak guard is about the bridge's own args.
    bridge_args = " ".join(argv[1:]).lower()
    assert "token" not in bridge_args
    assert "--token" not in bridge_args


def test_systemd_unit_contents(tmp_path):
    paths = _paths(tmp_path, with_venv=True)
    unit = build_systemd_unit(paths)
    assert "ExecStart=" in unit
    assert "-m ainb_phone_bridge run" in unit
    assert "Restart=always" in unit
    assert "RestartSec=10" in unit
    assert "venv/bin/python3" in unit
    assert "token" not in unit.lower()


def test_plist_contents(tmp_path):
    paths = _paths(tmp_path, with_venv=True)
    plist = _build_plist(paths)
    assert plist["Label"] == "com.agentsinabox.phone-bridge"
    assert plist["KeepAlive"] is True
    assert plist["RunAtLoad"] is True
    assert plist["ThrottleInterval"] == 10
    assert plist["ProgramArguments"][1:] == ["-m", "ainb_phone_bridge", "run"]
    assert "TOKEN" not in plist["EnvironmentVariables"]
    # PATH + HOME only (plus PYTHONPATH for importability) — no secrets.
    assert set(plist["EnvironmentVariables"]) == {"PATH", "HOME", "PYTHONPATH"}


def test_install_teardown_idempotent_linux(tmp_path, monkeypatch):
    # Force the systemd path and redirect HOME so we don't touch the real one.
    monkeypatch.setattr(service.sys, "platform", "linux")
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path))

    calls: list[list[str]] = []

    def fake_run(cmd, **kwargs):
        calls.append(cmd)

        class R:
            returncode = 0

        return R()

    monkeypatch.setattr(service.subprocess, "run", fake_run)

    unit_path = service.install()
    assert unit_path.exists()
    # Second install is a clean overwrite (idempotent).
    service.install()
    assert unit_path.exists()

    removed = service.teardown()
    assert removed == unit_path
    assert not unit_path.exists()
    # Teardown when nothing installed is a no-op.
    assert service.teardown() is None
