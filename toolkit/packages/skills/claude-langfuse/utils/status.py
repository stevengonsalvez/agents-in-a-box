#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "requests>=2.28.0",
# ]
# ///
"""
Langfuse status command - shows current session and recent traces.

Usage:
    python status.py
"""

import sys
from collections import Counter

try:
    from api import LangfuseClient, format_timestamp, format_duration
except ImportError:
    from .api import LangfuseClient, format_timestamp, format_duration


def print_box(title: str, width: int = 70):
    """Print a box header."""
    print('╔' + '═' * (width - 2) + '╗')
    print('║ ' + title.ljust(width - 4) + ' ║')
    print('╠' + '═' * (width - 2) + '╣')


def print_box_end(width: int = 70):
    """Print box footer."""
    print('╚' + '═' * (width - 2) + '╝')


def main():
    try:
        client = LangfuseClient()
    except ValueError as e:
        print(f"Error: {e}")
        print("Set LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY in ~/.secrets")
        sys.exit(1)

    # Get current session
    current_session = client.get_current_session_id()
    current_trace = client.get_current_trace_id()

    print()
    print_box('LANGFUSE STATUS')

    # Current session info
    if current_session and current_trace:
        try:
            trace = client.get_trace(current_trace)
            observations = client.get_observations(current_trace)

            tool_counts = Counter(o.get('name', 'unknown') for o in observations)

            print(f'║ Current Session: {current_session[:30]}...'.ljust(68) + ' ║')
            print(f'║ Trace ID: {current_trace[:40]}...'.ljust(68) + ' ║')
            print(f'║ Project: {trace.get("metadata", {}).get("project", "N/A")}'.ljust(68) + ' ║')
            print(f'║ Started: {format_timestamp(trace.get("timestamp", ""))}'.ljust(68) + ' ║')
            print(f'║ Observations: {len(observations)}'.ljust(68) + ' ║')
            print('║'.ljust(69) + '║')
            print('║ Tool breakdown:'.ljust(69) + '║')
            for tool, count in tool_counts.most_common(5):
                print(f'║   {tool}: {count}'.ljust(68) + ' ║')
        except Exception as e:
            print(f'║ Error getting current trace: {e}'.ljust(68) + ' ║')
    else:
        print('║ No active session detected'.ljust(68) + ' ║')

    print('╠' + '═' * 68 + '╣')

    # Recent traces
    print('║ RECENT SESSIONS'.ljust(69) + '║')
    print('╠' + '═' * 68 + '╣')

    try:
        traces = client.get_traces(limit=5)

        for i, trace in enumerate(traces, 1):
            session_id = trace.get('sessionId', 'N/A')[:20]
            timestamp = format_timestamp(trace.get('timestamp', ''))
            project = trace.get('metadata', {}).get('project', 'N/A')[:20]
            tags = trace.get('tags', [])

            # Get observation count
            try:
                obs = client.get_observations(trace.get('id'), limit=100)
                obs_count = len(obs)
                tool_count = len([o for o in obs if 'tool:' in o.get('name', '')])
            except Exception:
                obs_count = '?'
                tool_count = '?'

            print(f'║ [{i}] {session_id}...'.ljust(68) + ' ║')
            print(f'║     Project: {project}'.ljust(68) + ' ║')
            print(f'║     Time: {timestamp}'.ljust(68) + ' ║')
            print(f'║     Observations: {obs_count} ({tool_count} tools)'.ljust(68) + ' ║')
            print(f'║     Tags: {", ".join(tags)}'.ljust(68) + ' ║')

            if i < len(traces):
                print('║'.ljust(69) + '║')

    except Exception as e:
        print(f'║ Error fetching traces: {e}'.ljust(68) + ' ║')

    print_box_end()

    # Print Langfuse URL
    print()
    print(f'View in Langfuse: {client.host}/sessions')
    print()


if __name__ == '__main__':
    main()
