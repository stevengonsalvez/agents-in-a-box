# ABOUTME: Install / teardown the bridge as a launchd (macOS) or systemd (Linux)
# service. Idempotent: install overwrites cleanly, teardown removes everything.
#
# The daemon is launched as `<venv>/bin/python3 -m ainb_phone_bridge run`. The
# venv path uses `venv/bin/python3` (NO leading dot — agent-deck's daemon python
# resolver only ever stats `venv`, never `.venv`). The bot token is NEVER on the
# command line; the daemon reads it from config.toml at startup. The unit env
# carries only PATH and HOME.

from __future__ import annotations

import os
import plistlib
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

LABEL = "com.agentsinabox.phone-bridge"
_SYSTEMD_UNIT = "ainb-phone-bridge.service"


@dataclass
class ServicePaths:
    package_dir: Path  # the `bridge/` dir that holds ainb_phone_bridge/
    venv_python: Path  # <bridge>/venv/bin/python3
    log_path: Path  # ~/.agents-in-a-box/phone-bridge.log


def _bridge_root() -> Path:
    # ainb_phone_bridge/service.py -> ainb_phone_bridge -> bridge/
    return Path(__file__).resolve().parent.parent


def resolve_paths() -> ServicePaths:
    root = _bridge_root()
    venv_python = root / "venv" / "bin" / "python3"
    log_path = Path.home() / ".agents-in-a-box" / "phone-bridge.log"
    return ServicePaths(package_dir=root, venv_python=venv_python, log_path=log_path)


def _python_for_daemon(paths: ServicePaths) -> str:
    """Prefer the bridge venv python; fall back to the current interpreter."""
    if paths.venv_python.exists():
        return str(paths.venv_python)
    return sys.executable or "python3"


def _daemon_argv(paths: ServicePaths) -> list[str]:
    # `-m ainb_phone_bridge run` — no token, ever, on argv.
    return [_python_for_daemon(paths), "-m", "ainb_phone_bridge", "run"]


# --- macOS launchd ----------------------------------------------------------


def _launchd_plist_path() -> Path:
    return Path.home() / "Library" / "LaunchAgents" / f"{LABEL}.plist"


def _build_plist(paths: ServicePaths) -> dict:
    env_path = os.environ.get("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
    return {
        "Label": LABEL,
        "ProgramArguments": _daemon_argv(paths),
        "WorkingDirectory": str(paths.package_dir),
        "RunAtLoad": True,
        "KeepAlive": True,
        "ThrottleInterval": 10,
        "LowPriorityIO": True,
        "StandardOutPath": str(paths.log_path),
        "StandardErrorPath": str(paths.log_path),
        "EnvironmentVariables": {
            "PATH": env_path,
            "HOME": str(Path.home()),
            # Make the package importable without an install step.
            "PYTHONPATH": str(paths.package_dir),
        },
    }


def _install_launchd(paths: ServicePaths) -> Path:
    plist_path = _launchd_plist_path()
    plist_path.parent.mkdir(parents=True, exist_ok=True)
    paths.log_path.parent.mkdir(parents=True, exist_ok=True)
    with plist_path.open("wb") as fh:
        plistlib.dump(_build_plist(paths), fh)
    # Idempotent reload: unload (ignore errors) then load.
    subprocess.run(
        ["launchctl", "unload", str(plist_path)],
        capture_output=True,
        check=False,
    )
    subprocess.run(["launchctl", "load", str(plist_path)], capture_output=True, check=False)
    return plist_path


def _teardown_launchd() -> Path | None:
    plist_path = _launchd_plist_path()
    if plist_path.exists():
        subprocess.run(
            ["launchctl", "unload", str(plist_path)],
            capture_output=True,
            check=False,
        )
        plist_path.unlink(missing_ok=True)
        return plist_path
    return None


# --- Linux systemd (user) ---------------------------------------------------


def _systemd_unit_path() -> Path:
    return Path.home() / ".config" / "systemd" / "user" / _SYSTEMD_UNIT


def build_systemd_unit(paths: ServicePaths) -> str:
    exec_start = " ".join(_daemon_argv(paths))
    return (
        "[Unit]\n"
        "Description=ainb Telegram phone bridge\n"
        "After=network-online.target\n"
        "Wants=network-online.target\n"
        "\n"
        "[Service]\n"
        "Type=simple\n"
        f"WorkingDirectory={paths.package_dir}\n"
        f"Environment=PYTHONPATH={paths.package_dir}\n"
        f"ExecStart={exec_start}\n"
        "Restart=always\n"
        "RestartSec=10\n"
        f"StandardOutput=append:{paths.log_path}\n"
        f"StandardError=append:{paths.log_path}\n"
        "\n"
        "[Install]\n"
        "WantedBy=default.target\n"
    )


def _install_systemd(paths: ServicePaths) -> Path:
    unit_path = _systemd_unit_path()
    unit_path.parent.mkdir(parents=True, exist_ok=True)
    paths.log_path.parent.mkdir(parents=True, exist_ok=True)
    unit_path.write_text(build_systemd_unit(paths), encoding="utf-8")
    subprocess.run(["systemctl", "--user", "daemon-reload"], capture_output=True, check=False)
    subprocess.run(
        ["systemctl", "--user", "enable", "--now", _SYSTEMD_UNIT],
        capture_output=True,
        check=False,
    )
    return unit_path


def _teardown_systemd() -> Path | None:
    unit_path = _systemd_unit_path()
    if unit_path.exists():
        subprocess.run(
            ["systemctl", "--user", "disable", "--now", _SYSTEMD_UNIT],
            capture_output=True,
            check=False,
        )
        unit_path.unlink(missing_ok=True)
        subprocess.run(["systemctl", "--user", "daemon-reload"], capture_output=True, check=False)
        return unit_path
    return None


# --- public API -------------------------------------------------------------


def install() -> Path:
    """Provision the daemon service for the current platform. Idempotent."""
    paths = resolve_paths()
    if sys.platform == "darwin":
        return _install_launchd(paths)
    return _install_systemd(paths)


def teardown() -> Path | None:
    """Remove the daemon service. Safe to call when nothing is installed."""
    if sys.platform == "darwin":
        return _teardown_launchd()
    return _teardown_systemd()


def status() -> str:
    """Human-readable install status for the current platform."""
    if sys.platform == "darwin":
        p = _launchd_plist_path()
        return f"launchd: {'installed' if p.exists() else 'not installed'} ({p})"
    p = _systemd_unit_path()
    return f"systemd: {'installed' if p.exists() else 'not installed'} ({p})"
