#!/usr/bin/env python3
"""
Token usage analyzer for Claude Code sessions.
Parses ~/.claude/projects/**/*.jsonl for token consumption data.

Usage:
  python3 token_usage.py                    # All time
  python3 token_usage.py --days 7           # Last 7 days
  python3 token_usage.py --since 2026-04-01 # Since date
  python3 token_usage.py --top-sessions 10  # Top N costly sessions
  python3 token_usage.py --format json      # JSON output
"""

import json
import os
import sys
import argparse
from pathlib import Path
from collections import defaultdict
from datetime import datetime, timedelta, timezone


def parse_args():
    p = argparse.ArgumentParser(description="Claude Code token usage analyzer")
    p.add_argument("--days", type=int, help="Only include sessions from last N days")
    p.add_argument("--since", type=str, help="Only include sessions since date (YYYY-MM-DD)")
    p.add_argument("--top-sessions", type=int, default=10, help="Show top N costly sessions (default: 10)")
    p.add_argument("--format", choices=["text", "json", "markdown"], default="text", help="Output format")
    p.add_argument("--project", type=str, help="Filter to a specific project (substring match)")
    p.add_argument("--projects-dir", type=str, help="Override projects directory path")
    return p.parse_args()


def get_cutoff(args):
    if args.since:
        return datetime.fromisoformat(args.since).replace(tzinfo=timezone.utc)
    if args.days:
        return datetime.now(timezone.utc) - timedelta(days=args.days)
    return None


def clean_project_name(raw):
    username = os.environ.get("USER", os.environ.get("USERNAME", "unknown"))
    for prefix in [f"-Users-{username}-", f"Users-{username}-"]:
        if raw.startswith(prefix):
            raw = raw[len(prefix):]
            break
    if raw.startswith("-agents-in-a-box-worktrees-"):
        raw = raw.replace("-agents-in-a-box-worktrees-", "worktree/", 1)
    return raw or "unknown"


def parse_jsonl(path):
    """Parse a single JSONL file, yielding (date_str, session_id, usage_dict) tuples."""
    try:
        with open(path) as f:
            for line in f:
                try:
                    obj = json.loads(line)
                except (json.JSONDecodeError, ValueError):
                    continue
                if obj.get("type") != "assistant":
                    continue
                usage = obj.get("message", {}).get("usage")
                if not usage:
                    continue
                ts = obj.get("timestamp", "")
                if len(ts) < 10:
                    continue
                session_id = obj.get("sessionId", path.stem)
                yield ts[:10], session_id, usage
    except (OSError, PermissionError):
        pass


def scan_projects(projects_dir, cutoff=None, project_filter=None):
    """Scan all projects and return aggregated data."""
    daily = defaultdict(lambda: {"input": 0, "cache_create": 0, "cache_read": 0, "output": 0, "sessions": set(), "projects": set()})
    projects = defaultdict(lambda: {"input": 0, "cache_create": 0, "cache_read": 0, "output": 0, "sessions": set()})
    sessions = defaultdict(lambda: {"input": 0, "cache_create": 0, "cache_read": 0, "output": 0, "project": "", "first_ts": "", "first_prompt": ""})

    for project_dir in projects_dir.iterdir():
        if not project_dir.is_dir():
            continue
        proj_name = clean_project_name(project_dir.name)
        if project_filter and project_filter.lower() not in proj_name.lower():
            continue

        for jsonl_file in project_dir.glob("*.jsonl"):
            session_id = jsonl_file.stem
            for date_str, sid, usage in parse_jsonl(jsonl_file):
                if cutoff:
                    try:
                        dt = datetime.fromisoformat(date_str).replace(tzinfo=timezone.utc)
                        if dt < cutoff:
                            continue
                    except ValueError:
                        continue

                inp = usage.get("input_tokens", 0)
                cc = usage.get("cache_creation_input_tokens", 0)
                cr = usage.get("cache_read_input_tokens", 0)
                out = usage.get("output_tokens", 0)

                # Daily
                d = daily[date_str]
                d["input"] += inp
                d["cache_create"] += cc
                d["cache_read"] += cr
                d["output"] += out
                d["sessions"].add(sid)
                d["projects"].add(proj_name)

                # Project
                p = projects[proj_name]
                p["input"] += inp
                p["cache_create"] += cc
                p["cache_read"] += cr
                p["output"] += out
                p["sessions"].add(sid)

                # Session
                s = sessions[sid]
                s["input"] += inp
                s["cache_create"] += cc
                s["cache_read"] += cr
                s["output"] += out
                s["project"] = proj_name
                if not s["first_ts"] or date_str < s["first_ts"]:
                    s["first_ts"] = date_str

            # Grab first human prompt for the session
            if session_id in sessions and not sessions[session_id]["first_prompt"]:
                try:
                    with open(jsonl_file) as f:
                        for line in f:
                            try:
                                obj = json.loads(line)
                            except (json.JSONDecodeError, ValueError):
                                continue
                            if obj.get("type") == "user" and not obj.get("isSidechain"):
                                content = obj.get("message", {}).get("content", "")
                                if isinstance(content, str) and content.strip():
                                    sessions[session_id]["first_prompt"] = content[:100]
                                    break
                                elif isinstance(content, list):
                                    for item in content:
                                        if isinstance(item, dict) and item.get("type") == "text":
                                            sessions[session_id]["first_prompt"] = item.get("text", "")[:100]
                                            break
                                    if sessions[session_id]["first_prompt"]:
                                        break
                except (OSError, PermissionError):
                    pass

    return daily, projects, sessions


def total_tokens(d):
    return d["input"] + d["cache_create"] + d["cache_read"] + d["output"]


def fmt(n):
    if n >= 1_000_000_000:
        return f"{n / 1_000_000_000:.1f}B"
    elif n >= 1_000_000:
        return f"{n / 1_000_000:.1f}M"
    elif n >= 1_000:
        return f"{n / 1_000:.0f}K"
    return str(n)


def fmt_commas(n):
    return f"{n:,}"


def print_text(daily, projects, sessions, top_n):
    # Grand totals
    grand = {"input": 0, "cache_create": 0, "cache_read": 0, "output": 0}
    total_sessions = set()
    for d in daily.values():
        grand["input"] += d["input"]
        grand["cache_create"] += d["cache_create"]
        grand["cache_read"] += d["cache_read"]
        grand["output"] += d["output"]
        total_sessions.update(d["sessions"])

    gt = grand["input"] + grand["cache_create"] + grand["cache_read"] + grand["output"]

    print(f"\n{'='*70}")
    print(f" Token Usage Summary")
    print(f"{'='*70}")
    print(f" Total: {fmt(gt)} tokens across {len(total_sessions)} sessions in {len(projects)} projects")
    print(f" Input: {fmt(grand['input'])}  Cache Create: {fmt(grand['cache_create'])}  Cache Read: {fmt(grand['cache_read'])}  Output: {fmt(grand['output'])}")
    print(f"{'='*70}\n")

    # Daily
    print(f"{'Date':<12} {'Total':>12} {'Input':>10} {'Cache':>12} {'Output':>10} {'Sessions':>8}")
    print("-" * 68)
    for date in sorted(daily.keys()):
        d = daily[date]
        t = total_tokens(d)
        cache = d["cache_create"] + d["cache_read"]
        print(f"{date:<12} {fmt(t):>12} {fmt(d['input']):>10} {fmt(cache):>12} {fmt(d['output']):>10} {len(d['sessions']):>8}")

    # Weekly
    print(f"\n{'Week Start':<12} {'Total':>12} {'Sessions':>8} {'Projects':>8} {'Avg/Day':>12}")
    print("-" * 56)
    weekly = defaultdict(lambda: {"total": 0, "sessions": set(), "projects": set(), "days": 0})
    for date, d in daily.items():
        try:
            dt = datetime.strptime(date, "%Y-%m-%d")
            ws = (dt - timedelta(days=dt.weekday())).strftime("%Y-%m-%d")
        except ValueError:
            continue
        w = weekly[ws]
        w["total"] += total_tokens(d)
        w["sessions"].update(d["sessions"])
        w["projects"].update(d["projects"])
        w["days"] += 1
    for ws in sorted(weekly.keys()):
        w = weekly[ws]
        avg = w["total"] // w["days"] if w["days"] else 0
        print(f"{ws:<12} {fmt(w['total']):>12} {len(w['sessions']):>8} {len(w['projects']):>8} {fmt(avg):>12}")

    # Projects
    print(f"\n{'#':>3} {'Project':<45} {'Total':>12} {'Sessions':>8}")
    print("-" * 72)
    sorted_projects = sorted(projects.items(), key=lambda x: total_tokens(x[1]), reverse=True)
    for i, (name, p) in enumerate(sorted_projects[:20], 1):
        display = name[:44]
        print(f"{i:>3} {display:<45} {fmt(total_tokens(p)):>12} {len(p['sessions']):>8}")

    # Top sessions
    print(f"\nTop {top_n} Costliest Sessions:")
    print("-" * 72)
    sorted_sessions = sorted(sessions.items(), key=lambda x: total_tokens(x[1]), reverse=True)
    for sid, s in sorted_sessions[:top_n]:
        t = total_tokens(s)
        prompt = s["first_prompt"].replace("\n", " ")[:60] if s["first_prompt"] else ""
        print(f" [{s['first_ts']}] {s['project'][:30]}: {fmt(t):>10} — {prompt}")


def print_markdown(daily, projects, sessions, top_n):
    grand = {"input": 0, "cache_create": 0, "cache_read": 0, "output": 0}
    total_sessions_set = set()
    for d in daily.values():
        grand["input"] += d["input"]
        grand["cache_create"] += d["cache_create"]
        grand["cache_read"] += d["cache_read"]
        grand["output"] += d["output"]
        total_sessions_set.update(d["sessions"])

    gt = grand["input"] + grand["cache_create"] + grand["cache_read"] + grand["output"]

    print(f"# Token Usage Report\n")
    print(f"**Total**: {fmt(gt)} tokens | {len(total_sessions_set)} sessions | {len(projects)} projects\n")
    print(f"| Metric | Value |")
    print(f"|--------|-------|")
    print(f"| Input | {fmt(grand['input'])} |")
    print(f"| Cache Create | {fmt(grand['cache_create'])} |")
    print(f"| Cache Read | {fmt(grand['cache_read'])} |")
    print(f"| Output | {fmt(grand['output'])} |")

    print(f"\n## Daily\n")
    print(f"| Date | Total | Input | Cache | Output | Sessions |")
    print(f"|------|-------|-------|-------|--------|----------|")
    for date in sorted(daily.keys()):
        d = daily[date]
        t = total_tokens(d)
        cache = d["cache_create"] + d["cache_read"]
        print(f"| {date} | {fmt(t)} | {fmt(d['input'])} | {fmt(cache)} | {fmt(d['output'])} | {len(d['sessions'])} |")

    print(f"\n## By Project\n")
    print(f"| # | Project | Total | Sessions |")
    print(f"|---|---------|-------|----------|")
    sorted_projects = sorted(projects.items(), key=lambda x: total_tokens(x[1]), reverse=True)
    for i, (name, p) in enumerate(sorted_projects[:20], 1):
        print(f"| {i} | {name[:40]} | {fmt(total_tokens(p))} | {len(p['sessions'])} |")

    print(f"\n## Top {top_n} Costliest Sessions\n")
    sorted_sessions = sorted(sessions.items(), key=lambda x: total_tokens(x[1]), reverse=True)
    for i, (sid, s) in enumerate(sorted_sessions[:top_n], 1):
        t = total_tokens(s)
        prompt = s["first_prompt"].replace("\n", " ")[:60] if s["first_prompt"] else ""
        print(f"{i}. **{s['project'][:30]}** — {fmt(t)} [{s['first_ts']}]")
        if prompt:
            print(f"   > {prompt}")


def print_json(daily, projects, sessions, top_n):
    result = {
        "daily": {date: {"total": total_tokens(d), **{k: v for k, v in d.items() if k not in ("sessions", "projects")}, "sessions": len(d["sessions"]), "projects": len(d["projects"])} for date, d in sorted(daily.items())},
        "projects": {name: {"total": total_tokens(p), **{k: v for k, v in p.items() if k != "sessions"}, "sessions": len(p["sessions"])} for name, p in sorted(projects.items(), key=lambda x: total_tokens(x[1]), reverse=True)},
        "top_sessions": [{
            "id": sid[:12],
            "project": s["project"],
            "total": total_tokens(s),
            "date": s["first_ts"],
            "prompt": s["first_prompt"][:100]
        } for sid, s in sorted(sessions.items(), key=lambda x: total_tokens(x[1]), reverse=True)[:top_n]]
    }
    grand = {"input": 0, "cache_create": 0, "cache_read": 0, "output": 0}
    for d in daily.values():
        for k in grand:
            grand[k] += d.get(k, 0)
    result["grand_total"] = {**grand, "total": sum(grand.values())}
    print(json.dumps(result, indent=2))


def main():
    args = parse_args()
    projects_dir = Path(args.projects_dir) if args.projects_dir else Path.home() / ".claude" / "projects"

    if not projects_dir.is_dir():
        print(f"Projects directory not found: {projects_dir}", file=sys.stderr)
        sys.exit(1)

    cutoff = get_cutoff(args)
    daily, projects, sessions = scan_projects(projects_dir, cutoff, args.project)

    if not daily:
        print("No usage data found.")
        sys.exit(0)

    if args.format == "json":
        print_json(daily, projects, sessions, args.top_sessions)
    elif args.format == "markdown":
        print_markdown(daily, projects, sessions, args.top_sessions)
    else:
        print_text(daily, projects, sessions, args.top_sessions)


if __name__ == "__main__":
    main()
