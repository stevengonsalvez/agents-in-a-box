"""Entry point for the ``reflect`` console script.

Re-exports the click group from ``learnings_cli`` so the pyproject ``project.scripts``
entry (``reflect = "reflect_kb.cli.main:main"``) resolves to a single callable.
"""

from reflect_kb.cli.learnings_cli import cli as main

__all__ = ["main"]


if __name__ == "__main__":
    main()
