# ABOUTME: CLI entry point for the phone bridge package.
#
#   python3 -m ainb_phone_bridge run        # run the daemon (foreground)
#   python3 -m ainb_phone_bridge install    # provision launchd/systemd service
#   python3 -m ainb_phone_bridge teardown   # remove the service
#   python3 -m ainb_phone_bridge status     # config + service status
#
# `run` reads the bot token from config.toml — it is NEVER accepted on argv.

from __future__ import annotations

import argparse
import logging
import sys

from . import __version__


def _configure_logging() -> None:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )


def _cmd_run(_args: argparse.Namespace) -> int:
    from .bridge import run_bridge

    return run_bridge()


def _cmd_install(_args: argparse.Namespace) -> int:
    from .service import install

    path = install()
    print(f"installed phone-bridge service: {path}")
    return 0


def _cmd_teardown(_args: argparse.Namespace) -> int:
    from .service import teardown

    path = teardown()
    if path is None:
        print("phone-bridge service not installed — nothing to remove")
    else:
        print(f"removed phone-bridge service: {path}")
    return 0


def _cmd_status(_args: argparse.Namespace) -> int:
    from .config import ConfigError, load_config
    from .service import status

    print(status())
    try:
        cfg = load_config()
        print(
            f"config: ok (user_id={cfg.authorized_user_id}, "
            f"default_target={cfg.default_target or '(conductor/auto)'}, "
            f"response_timeout={cfg.response_timeout}s)"
        )
    except ConfigError as exc:
        print(f"config: ERROR — {exc}")
    return 0


def main(argv: list[str] | None = None) -> int:
    _configure_logging()
    parser = argparse.ArgumentParser(
        prog="ainb_phone_bridge",
        description="ainb Telegram phone bridge daemon + service installer.",
    )
    parser.add_argument("--version", action="version", version=__version__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("run", help="run the bridge daemon (foreground)")
    sub.add_parser("install", help="install the launchd/systemd service")
    sub.add_parser("teardown", help="remove the launchd/systemd service")
    sub.add_parser("status", help="show config + service status")

    args = parser.parse_args(argv)
    handlers = {
        "run": _cmd_run,
        "install": _cmd_install,
        "teardown": _cmd_teardown,
        "status": _cmd_status,
    }
    return handlers[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
