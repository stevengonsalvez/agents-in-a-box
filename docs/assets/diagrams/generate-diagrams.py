#!/usr/bin/env python3
"""Generate the architecture diagrams under docs/assets/diagrams/.

The diagram used to be hand-authored, which is how it ended up advertising
"11 crates" and "v1.5" long after the workspace passed 34 crates and v1.22,
and how it kept listing notifyd as a plugin when notifyd is a daemon.

Everything countable is read from the repo at generation time, so regenerating
picks up drift instead of preserving it:

    python3 docs/assets/diagrams/generate-diagrams.py

Emits ecosystem-architecture.svg and plugin-architecture.svg, rewriting them in
place, and prints the facts it used so a reviewer can check them.

Colours: every element carries its light value as a presentation attribute and
a class. The embedded stylesheet only repaints for dark mode. GitHub sanitises
SVG CSS, so the README still gets a correct light diagram while the docsite
follows the reader's colour scheme.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path
from xml.sax.saxutils import escape

ROOT = Path(__file__).resolve().parents[3]
DIAGRAMS = ROOT / "docs/assets/diagrams"
OUT_ECO = DIAGRAMS / "ecosystem-architecture.svg"
OUT_PLUGIN = DIAGRAMS / "plugin-architecture.svg"

# ---------------------------------------------------------------- live facts


def crate_count() -> int:
    """Workspace members, from cargo, so `default-members` cannot inflate it
    the way a naive grep over Cargo.toml does."""
    try:
        out = subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            cwd=ROOT / "ainb-tui", capture_output=True, text=True,
            timeout=180, check=True,
        ).stdout
        return len(json.loads(out)["packages"])
    except Exception:  # noqa: BLE001 - fall back to the members array
        txt = (ROOT / "ainb-tui/Cargo.toml").read_text()
        m = re.search(r"^members\s*=\s*\[(.*?)\]", txt, re.S | re.M)
        return len(re.findall(r'"[^"]+"', m.group(1))) if m else 0


def workspace_version() -> str:
    txt = (ROOT / "ainb-tui/Cargo.toml").read_text()
    m = re.search(r'^version\s*=\s*"([^"]+)"', txt, re.M)
    return m.group(1) if m else "?"


def staged_plugins() -> list[str]:
    """Plugin ids the build script stages into dist/plugins/<id>/."""
    txt = (ROOT / "ainb-tui/scripts/build-plugins.sh").read_text()
    return re.findall(r"^build_plugin\s+\S+\s+(\S+)", txt, re.M)


def daemon_kinds() -> list[str]:
    """Stable lowercase daemon ids, from DaemonKind::id()."""
    txt = (ROOT / "ainb-tui/crates/ainb-core/src/fleet/daemons/probe.rs").read_text()
    block = re.search(r"pub fn id\(self\).*?\{(.*?)\n    \}", txt, re.S)
    return re.findall(r'=>\s*"([a-z-]+)"', block.group(1)) if block else []


def screen_count() -> int:
    txt = (ROOT / "ainb-tui/crates/ainb-core/src/app/screens/mod.rs").read_text()
    return len(re.findall(r"^\s*pub const ", txt, re.M))


def cli_command_count() -> int:
    """Registry built-ins, plus `tui` and `diff-review` wired up inline in
    main.rs, plus the commands routed to the separate ainb-cli crate."""
    reg = (ROOT / "ainb-tui/crates/ainb-core/src/cli/registry.rs").read_text()
    m = re.search(r"pub fn built_ins\(\) -> Self \{(.*?)\n    \}", reg, re.S)
    registry = len(re.findall(r"^\s*r\.register\(", m.group(1), re.M)) if m else 0
    main = (ROOT / "ainb-tui/crates/ainb-core/src/main.rs").read_text()
    routed = re.search(r"SKILL_MANAGER_CLI_COMMANDS[^=]*=\s*&?\[([^\]]*)\]", main, re.S)
    routed_n = len(re.findall(r'"[^"]+"', routed.group(1))) if routed else 0
    return registry + 2 + routed_n


def tool_adapters() -> list[str]:
    txt = (ROOT / "ainb-tui/crates/ainb-adapters-tool/src/lib.rs").read_text()
    m = re.search(r"fn adapter_by_name.*?match name \{(.*?)\n    \}", txt, re.S)
    return re.findall(r'"([a-z-]+)"\s*=>', m.group(1)) if m else []


def rpc_method_count() -> int:
    txt = (ROOT / "ainb-tui/crates/ainb-hangar-proto/src/methods.rs").read_text()
    m = re.search(r"ALL_METHODS[^=]*=\s*&?\[(.*?)\];", txt, re.S)
    if not m:
        return 0
    return len([
        x for x in m.group(1).split(",")
        if x.strip() and not x.strip().startswith("//")
    ])


def toolkit_counts() -> tuple[int, int]:
    """Skills and agents from the toolkit's generated catalog, if reachable."""
    try:
        import yaml  # noqa: PLC0415
    except ImportError:
        return (0, 0)
    try:
        url = "repos/stevengonsalvez/ainb-toolkit/contents/catalog.yaml"
        raw = subprocess.run(
            ["gh", "api", url, "--jq", ".download_url"],
            capture_output=True, text=True, timeout=25, check=True,
        ).stdout.strip()
        body = subprocess.run(
            ["curl", "-sL", "--max-time", "25", raw],
            capture_output=True, text=True, timeout=30, check=True,
        ).stdout
        cat = yaml.safe_load(body)["components"]
        return (len(cat["skills"]), sum(len(v) for v in cat["agents"].values()))
    except Exception:  # noqa: BLE001 - offline is fine, we just omit the numbers
        return (0, 0)


# ------------------------------------------------------------------ drawing

W, H = 1660, 930
PAD = 36

LIGHT = dict(bg="#FAF9F5", card="#FFFFFF", edge="#E4E0D8", ink="#141413",
             mute="#6B655B", chip="#F4F1EA", accent="#C25A38", alt="#5F7048",
             line="#8A857C")
DARK = dict(bg="#0F0F18", card="#191923", edge="#2E2E3E", ink="#DCDCE6",
            mute="#9A9AB0", chip="#20202C", accent="#E08A63", alt="#8FA86B",
            line="#7A7A92")

parts: list[str] = []


def esc(t: object) -> str:
    return escape(str(t))


def text(x, y, s, size=13, cls="ink", weight=None, anchor="start"):
    w = f' font-weight="{weight}"' if weight else ""
    parts.append(
        f'<text x="{x}" y="{y}" font-size="{size}" class="{cls}" fill="{LIGHT[cls]}"'
        f' text-anchor="{anchor}"{w}>{esc(s)}</text>'
    )


def box(x, y, w, h, cls="card", rx=10):
    parts.append(
        f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" class="{cls}"'
        f' fill="{LIGHT[cls]}" stroke="{LIGHT["edge"]}" stroke-width="1"/>'
    )


def section(x, y, w, h, num, title, subtitle):
    box(x, y, w, h, "card")
    parts.append(
        f'<rect x="{x}" y="{y}" width="4" height="{h}" rx="2"'
        f' class="accentfill" fill="{LIGHT["accent"]}"/>'
    )
    text(x + 18, y + 28, num, 15, "accent", 800)
    text(x + 38, y + 28, title, 15, "ink", 700)
    text(x + 18, y + 48, subtitle, 11.5, "mute")


def chip(x, y, w, h, label, sub=None):
    box(x, y, w, h, "chip", 7)
    text(x + 11, y + 20, label, 12, "ink", 600)
    if sub:
        text(x + 11, y + 35, sub, 10, "mute")


def grid(x, y, items, cols, cw, ch, gap=9):
    """Lay chips out in a grid; return the y just below the last row."""
    for i, it in enumerate(items):
        chip(x + (i % cols) * (cw + gap), y + (i // cols) * (ch + gap),
             cw, ch, it[0], it[1] if len(it) > 1 else None)
    return y + ((len(items) + cols - 1) // cols) * (ch + gap)


def arrow(x1, y1, x2, y2, cls="line", dash=False):
    d = ' stroke-dasharray="5 4"' if dash else ""
    marker = "arrAlt" if cls == "alt" else "arr"
    parts.append(
        f'<path d="M{x1} {y1} L{x2} {y2}" class="{cls}" fill="none"'
        f' stroke="{LIGHT[cls]}" stroke-width="1.6"{d} marker-end="url(#{marker})"/>'
    )


def build() -> str:
    crates, ver = crate_count(), workspace_version()
    plugins, daemons = staged_plugins(), daemon_kinds()
    screens, cmds = screen_count(), cli_command_count()
    adapters, rpcs = tool_adapters(), rpc_method_count()
    skills, agents = toolkit_counts()

    parts.clear()
    colw = (W - PAD * 2 - 28) // 2
    left, right = PAD, PAD + colw + 28

    text(PAD, 44, "agents-in-a-box", 26, "ink", 800)
    parts.append(
        f'<rect x="{PAD}" y="54" width="238" height="4" rx="2"'
        f' class="accentfill" fill="{LIGHT["accent"]}"/>'
    )
    text(PAD + 258, 44, "Ecosystem architecture", 15, "mute")
    text(W - PAD, 44, f"ainb v{ver} · {crates}-crate Rust workspace",
         12, "mute", anchor="end")

    # 1 - TUI host
    y = 84
    section(left, y, colw, 292, "1", "ainb TUI host",
            f"one binary · {screens} screens · {cmds} CLI commands · Unix only")
    yy = grid(left + 18, y + 64, [
        ("git worktrees", "a branch and a directory per session"),
        ("tmux / PTY", "attach · send-keys · survives sleep"),
        ("provider adapters", "claude · codex · gemini · copilot"),
        ("session store", "~/.agents-in-a-box/"),
    ], 2, (colw - 45) // 2, 48)
    chip(left + 18, yy + 4, colw - 36, 44, "screens",
         "home · sessions · fleet · hangar · inbox · stats · review · daemons · skills")
    chip(left + 18, yy + 56, colw - 36, 44, "config",
         "config.toml · favorites · presets · plugin root")

    # 2 - plugin host
    section(right, y, colw, 292, "2", "Plugin host (v2 ABI)",
            "subprocess · JSON-RPC 2.0 over stdio · capabilities default-deny")
    yy = grid(right + 18, y + 64, [(p,) for p in plugins], 3,
              (colw - 54) // 3, 34)
    chip(right + 18, yy + 8, colw - 36, 44, "runtime",
         "spawn · lifecycle FSM · snapshot bus · render frames · -32001 on denial")
    chip(right + 18, yy + 60, colw - 36, 44, "SDK + conformance",
         "protocol · sdk-rust · testkit · cts-v2 (21 axes)")

    # 3 - daemons. These are NOT plugins; the old diagram put notifyd in the
    # plugin box, which was the most misleading thing on it.
    y2 = y + 312
    section(left, y2, W - PAD * 2, 164, "3", "Daemons",
            "long-lived processes the TUI supervises · started, stopped and "
            "health-checked from the Daemons screen")
    blurb = {
        "notifyd": "notifications into the Inbox",
        "hangar-daemon": f"boards · tasks · {rpcs} RPC methods",
        "mcp-pool": "one MCP server, shared",
        "headroom-proxy": "context compression",
        "atc": "always-on watcher",
        "bridge": "Telegram · Slack · Discord",
        "approve-broker": "permission prompts",
        "fleet-daemon": "API-error auto-continue",
        "release-checker": "signed update check",
    }
    grid(left + 18, y2 + 64, [(d, blurb.get(d, "")) for d in daemons], 5,
         (W - PAD * 2 - 36 - 4 * 9) // 5, 44)

    # 4 - toolkit
    y3 = y2 + 184
    section(left, y3, colw, 168, "4", "Toolkit (separate repo)",
            "stevengonsalvez/ainb-toolkit · consumed as a pinned source")
    chip(left + 18, y3 + 64, colw - 36, 44,
         f"{skills} skills · {agents} agents" if skills else "skills · agents · workflows",
         "written once, deployed everywhere")
    chip(left + 18, y3 + 114, colw - 36, 44,
         f"{len(adapters)} tool homes", " · ".join(adapters))

    # 5 - memory
    section(right, y3, colw, 168, "5", "Memory (separate repo)",
            "stevengonsalvez/ainb-reflect-memory · CLI only, no library API")
    chip(right + 18, y3 + 64, colw - 36, 44, "reflect",
         "/reflect captures · /recall retrieves")
    chip(right + 18, y3 + 114, colw - 36, 44, "two-tier index",
         "GraphRAG across projects · vector search for fast hits")

    # 6 - distribution
    y4 = y3 + 188
    section(left, y4, W - PAD * 2, 100, "6", "Distribution",
            "how it reaches a machine, and how it is documented")
    grid(left + 18, y4 + 62, [
        ("Homebrew", "brew install ainb"),
        ("install.sh", "one-liner elsewhere"),
        ("docs/", "single source of truth"),
        ("ainb.app", "Astro Starlight on Pages"),
    ], 4, (W - PAD * 2 - 36 - 3 * 9) // 4, 32)

    arrow(left + colw // 2, y + 292, left + colw // 2, y2 - 4)
    arrow(right + colw // 2, y + 292, right + colw // 2, y2 - 4)
    arrow(left + colw // 2, y2 + 164, left + colw // 2, y3 - 4, "alt", True)
    arrow(right + colw // 2, y2 + 164, right + colw // 2, y3 - 4, "alt", True)

    ly = y4 + 124
    text(PAD, ly, "Solid: supervises.   Dashed: reads and writes.", 11.5, "mute")
    text(W - PAD, ly,
         "Regenerate: python3 docs/assets/diagrams/generate-diagrams.py",
         11.5, "mute", anchor="end")

    d = DARK
    style = (
        'text{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",'
        '"Helvetica Neue",Arial,sans-serif;}'
        "@media (prefers-color-scheme: dark){"
        f".bg{{fill:{d['bg']};}}"
        f".ink{{fill:{d['ink']};}}.mute{{fill:{d['mute']};}}"
        f".accent{{fill:{d['accent']};}}.accentfill{{fill:{d['accent']};}}"
        f".card{{fill:{d['card']};stroke:{d['edge']};}}"
        f".chip{{fill:{d['chip']};stroke:{d['edge']};}}"
        f".line{{stroke:{d['line']};}}.alt{{stroke:{d['alt']};}}"
        f".linefill{{fill:{d['line']};}}.altfill{{fill:{d['alt']};}}"
        "}"
    )

    return wrap(W, H, "agents-in-a-box ecosystem architecture: the TUI host, "
                "the v2 plugin host, the daemons it supervises, the separate "
                "toolkit and memory repos, and distribution"), dict(
        crates=crates, ver=ver, screens=screens, cmds=cmds, plugins=plugins,
        daemons=daemons, adapters=adapters, rpcs=rpcs, skills=skills,
        agents=agents,
    )


def wrap(w: int, h: int, label: str) -> str:
    """Close the current `parts` buffer into a finished SVG document."""
    d = DARK
    style = (
        'text{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",'
        '"Helvetica Neue",Arial,sans-serif;}'
        "@media (prefers-color-scheme: dark){"
        f".bg{{fill:{d['bg']};}}"
        f".ink{{fill:{d['ink']};}}.mute{{fill:{d['mute']};}}"
        f".accent{{fill:{d['accent']};}}.accentfill{{fill:{d['accent']};}}"
        f".card{{fill:{d['card']};stroke:{d['edge']};}}"
        f".chip{{fill:{d['chip']};stroke:{d['edge']};}}"
        f".line{{stroke:{d['line']};}}.alt{{stroke:{d['alt']};}}"
        f".linefill{{fill:{d['line']};}}.altfill{{fill:{d['alt']};}}"
        "}"
    )
    body = "\n  ".join(parts)
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}"'
        f' width="{w}" height="{h}" role="img" aria-label="{esc(label)}">\n'
        f"  <style>{style}</style>\n"
        f"  <defs>\n"
        f'    <marker id="arr" markerWidth="9" markerHeight="8" refX="7.5"'
        f' refY="4" orient="auto"><polygon points="0 0, 8.5 4, 0 8"'
        f' class="linefill" fill="{LIGHT["line"]}"/></marker>\n'
        f'    <marker id="arrAlt" markerWidth="9" markerHeight="8" refX="7.5"'
        f' refY="4" orient="auto"><polygon points="0 0, 8.5 4, 0 8"'
        f' class="altfill" fill="{LIGHT["alt"]}"/></marker>\n'
        f"  </defs>\n"
        f'  <rect width="{w}" height="{h}" class="bg" fill="{LIGHT["bg"]}"/>\n'
        f"  {body}\n</svg>\n"
    )


def build_plugin_diagram() -> tuple[str, dict]:
    """The v2 plugin ABI: what the host owns, what a plugin owns, and the two
    ways a plugin can put pixels on screen."""
    plugins = staged_plugins()
    pw, ph = 1500, 800
    parts.clear()

    text(PAD, 44, "ainb v2 plugin architecture", 24, "ink", 800)
    parts.append(
        f'<rect x="{PAD}" y="54" width="238" height="4" rx="2"'
        f' class="accentfill" fill="{LIGHT["accent"]}"/>'
    )
    text(PAD, 74, "native subprocess · JSON-RPC 2.0 over Content-Length stdio",
         13, "mute")

    inner = pw - PAD * 2 - 36

    # host
    section(PAD, 98, pw - PAD * 2, 134, "1", "Host (binary: ainb)",
            "owns the terminal · discovers, spawns and supervises every plugin")
    grid(PAD + 18, 98 + 64, [
        ("ainb-core", "ratatui TUI, owns the terminal"),
        ("ainb-plugin-runtime", "discover · spawn · route · restart"),
        ("capability gate", "default-deny, refuses with -32001"),
    ], 3, (inner - 2 * 9) // 3, 44)

    # the wire
    section(PAD, 252, pw - PAD * 2, 128, "2", "The wire",
            "one process per plugin, framed stdio, no shared memory")
    grid(PAD + 18, 252 + 62, [
        ("plugin/*", "init · render · handle_key · handle_event · cli_dispatch · shutdown"),
        ("host/*", "snapshot get/publish/subscribe · action invoke · log · fs · fetch"),
    ], 2, (inner - 9) // 2, 44)

    # plugins
    section(PAD, 400, pw - PAD * 2, 178, "3", f"Plugins ({len(plugins)} ship in-tree)",
            "each declares its capabilities in manifest.toml; the host enforces them")
    blurb = {
        "burndown": "Stats screen · ainb usage",
        "session-reader": "publisher, no screen",
        "witr": "ainb witr · foreign TTY",
        "learnings": "Memory screen · search",
        "abtop": "agent monitor · foreign TTY",
        "hangar-tui": "Hangar screen · daemon client",
    }
    grid(PAD + 18, 400 + 62, [(p, blurb.get(p, "")) for p in plugins], 3,
         (inner - 2 * 9) // 3, 44)

    # render modes
    section(PAD, 598, pw - PAD * 2, 122, "4", "Two ways to draw",
            "in-process frames, or hand the terminal over entirely")
    grid(PAD + 18, 598 + 62, [
        ("WireBuffer", "plugin returns frames, host composites them"),
        ("foreign TTY", "host suspends, tmux attaches the real binary, host resumes"),
    ], 2, (inner - 9) // 2, 44)

    arrow(pw // 2, 232, pw // 2, 248)
    arrow(pw // 2, 380, pw // 2, 396)

    text(PAD, 762, "Capabilities are default-deny: a plugin reaches the network, "
                   "the filesystem or your secrets only if its manifest asks and you agree.",
         11.5, "mute")
    text(pw - PAD, 762,
         "Regenerate: python3 docs/assets/diagrams/generate-diagrams.py",
         11.5, "mute", anchor="end")

    return wrap(pw, ph, "ainb v2 plugin architecture: host, wire protocol, "
                        "in-tree plugins and the two render modes"), {"plugins": plugins}


if __name__ == "__main__":
    svg, facts = build()
    OUT_ECO.write_text(svg)
    for k, v in facts.items():
        print(f"  {k}: {v}")
    psvg, pfacts = build_plugin_diagram()
    OUT_PLUGIN.write_text(psvg)
    print(f"  plugin diagram: {pfacts['plugins']}")
    for f in (OUT_ECO, OUT_PLUGIN):
        print(f"wrote {f.relative_to(ROOT)} ({f.stat().st_size} bytes)", file=sys.stderr)
