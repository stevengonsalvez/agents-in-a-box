#!/usr/bin/env python3
"""Inject the Claude-theme overlay CSS into a copied template.

Usage:
    inject_theme.py <html-file> [<theme-css>]

Writes back to <html-file> in place. If <theme-css> is omitted, uses
../assets/claude-theme.css relative to this script.
"""
from __future__ import annotations

import pathlib
import sys


def inject(html_path: pathlib.Path, theme_path: pathlib.Path) -> None:
    html = html_path.read_text(encoding="utf-8")
    if 'data-claude-theme="injected"' in html:
        print(f"already injected: {html_path}", file=sys.stderr)
        return
    theme = theme_path.read_text(encoding="utf-8")
    marker = "</style>"
    i = html.find(marker)
    if i < 0:
        raise SystemExit(f"no </style> found in {html_path}")
    block = (
        f"{marker}\n"
        '<style data-claude-theme="injected">\n'
        f"{theme}\n"
        "</style>"
    )
    new_html = html[:i] + block + html[i + len(marker):]
    html_path.write_text(new_html, encoding="utf-8")
    print(f"injected {len(theme)} chars into {html_path}")


def main() -> None:
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    html_path = pathlib.Path(sys.argv[1]).resolve()
    if len(sys.argv) >= 3:
        theme_path = pathlib.Path(sys.argv[2]).resolve()
    else:
        theme_path = (
            pathlib.Path(__file__).resolve().parent.parent
            / "assets"
            / "claude-theme.css"
        )
    inject(html_path, theme_path)


if __name__ == "__main__":
    main()
