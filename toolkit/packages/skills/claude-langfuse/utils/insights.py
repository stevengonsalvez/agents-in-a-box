#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "requests>=2.28.0",
# ]
# ///
"""
Langfuse insights command - deep analysis of a specific session trace.

Usage:
    python insights.py <trace_id>
    python insights.py --latest
"""

import sys
import argparse
from datetime import datetime
from collections import Counter, defaultdict
from typing import Dict, Any, List

try:
    from api import LangfuseClient, format_timestamp, format_duration
except ImportError:
    from .api import LangfuseClient, format_timestamp, format_duration


def analyze_trace(client: LangfuseClient, trace_id: str) -> Dict[str, Any]:
    """Deep analysis of a single trace."""

    trace = client.get_trace(trace_id)
    observations = client.get_observations(trace_id, limit=200)

    # Sort observations by time
    sorted_obs = sorted(observations, key=lambda x: x.get('startTime', ''))

    # Categorize observations
    tool_obs = [o for o in sorted_obs if 'tool:' in o.get('name', '')]
    prompt_obs = [o for o in sorted_obs if o.get('name') == 'user-prompt']

    # Tool usage stats
    tool_counts = Counter(o.get('name', '').replace('tool:', '') for o in tool_obs)

    # Calculate timings
    durations = []
    for o in tool_obs:
        start = o.get('startTime')
        end = o.get('endTime')
        if start and end:
            try:
                start_dt = datetime.fromisoformat(start.replace('Z', '+00:00'))
                end_dt = datetime.fromisoformat(end.replace('Z', '+00:00'))
                dur_sec = (end_dt - start_dt).total_seconds()
                durations.append({
                    'tool': o.get('name', ''),
                    'duration': dur_sec,
                    'input': o.get('input', {}),
                })
            except Exception:
                pass

    # Find slowest operations
    slowest = sorted(durations, key=lambda x: x['duration'], reverse=True)[:5]

    # Analyze prompts for complexity
    prompt_texts = []
    for p in prompt_obs:
        input_data = p.get('input', {})
        if isinstance(input_data, dict):
            text = input_data.get('prompt', '')
        else:
            text = str(input_data)
        if text:
            prompt_texts.append(text)

    # Session timeline phases
    phases = []
    if sorted_obs:
        current_phase = {'start': sorted_obs[0].get('startTime'), 'tools': []}
        last_time = None

        for o in sorted_obs:
            obs_time = o.get('startTime')
            if last_time and obs_time:
                try:
                    last_dt = datetime.fromisoformat(last_time.replace('Z', '+00:00'))
                    obs_dt = datetime.fromisoformat(obs_time.replace('Z', '+00:00'))
                    gap = (obs_dt - last_dt).total_seconds()

                    # New phase if gap > 60 seconds
                    if gap > 60:
                        current_phase['end'] = last_time
                        phases.append(current_phase)
                        current_phase = {'start': obs_time, 'tools': []}
                except Exception:
                    pass

            current_phase['tools'].append(o.get('name', 'unknown'))
            last_time = obs_time

        current_phase['end'] = last_time
        phases.append(current_phase)

    return {
        'trace': trace,
        'observations': sorted_obs,
        'tool_counts': tool_counts,
        'prompt_count': len(prompt_obs),
        'prompt_texts': prompt_texts,
        'slowest': slowest,
        'phases': phases,
        'total_observations': len(observations),
    }


def print_insights(analysis: Dict[str, Any]):
    """Print detailed insights."""
    trace = analysis['trace']
    metadata = trace.get('metadata', {})

    print()
    print('═' * 70)
    print('  LANGFUSE INSIGHTS - Session Deep Dive')
    print('═' * 70)

    # Overview
    print()
    print('┌' + '─' * 68 + '┐')
    print('│ SESSION OVERVIEW'.ljust(69) + '│')
    print('├' + '─' * 68 + '┤')
    print(f"│ Session ID: {trace.get('sessionId', 'N/A')[:45]}".ljust(69) + '│')
    print(f"│ Trace ID: {trace.get('id', 'N/A')[:48]}".ljust(69) + '│')
    print(f"│ User: {trace.get('userId', 'N/A')}".ljust(69) + '│')
    print(f"│ Project: {metadata.get('project', 'N/A')}".ljust(69) + '│')
    print(f"│ Branch: {metadata.get('git_branch', 'N/A')}".ljust(69) + '│')
    print(f"│ Started: {format_timestamp(trace.get('timestamp', ''))}".ljust(69) + '│')
    print(f"│ Ended: {format_timestamp(metadata.get('ended_at', ''))}".ljust(69) + '│')
    print(f"│ Tags: {', '.join(trace.get('tags', []))}".ljust(69) + '│')
    print('└' + '─' * 68 + '┘')

    # Stats
    print()
    print('┌' + '─' * 68 + '┐')
    print('│ STATISTICS'.ljust(69) + '│')
    print('├' + '─' * 68 + '┤')
    print(f"│ Total Observations: {analysis['total_observations']}".ljust(69) + '│')
    print(f"│ User Prompts: {analysis['prompt_count']}".ljust(69) + '│')
    print(f"│ Tool Invocations: {sum(analysis['tool_counts'].values())}".ljust(69) + '│')
    print(f"│ Activity Phases: {len(analysis['phases'])}".ljust(69) + '│')
    print('└' + '─' * 68 + '┘')

    # Tool breakdown
    print()
    print('┌' + '─' * 68 + '┐')
    print('│ TOOL USAGE'.ljust(69) + '│')
    print('├' + '─' * 68 + '┤')
    for tool, count in analysis['tool_counts'].most_common(10):
        bar_len = min(count, 30)
        bar = '█' * bar_len
        print(f"│ {tool:15} {count:4} {bar}".ljust(69) + '│')
    print('└' + '─' * 68 + '┘')

    # Slowest operations
    if analysis['slowest']:
        print()
        print('┌' + '─' * 68 + '┐')
        print('│ SLOWEST OPERATIONS'.ljust(69) + '│')
        print('├' + '─' * 68 + '┤')
        for i, op in enumerate(analysis['slowest'], 1):
            dur = op['duration']
            if dur < 60:
                dur_str = f"{dur:.1f}s"
            else:
                dur_str = f"{dur/60:.1f}m"

            tool_name = op['tool'].replace('tool:', '')
            input_preview = ''
            if isinstance(op['input'], dict):
                if 'command' in op['input']:
                    input_preview = str(op['input']['command'])[:30]
                elif 'file_path' in op['input']:
                    input_preview = str(op['input']['file_path'])[-30:]

            print(f"│ [{i}] {tool_name}: {dur_str}".ljust(69) + '│')
            if input_preview:
                print(f"│     {input_preview}...".ljust(69) + '│')
        print('└' + '─' * 68 + '┘')

    # Phases
    if analysis['phases']:
        print()
        print('┌' + '─' * 68 + '┐')
        print('│ SESSION PHASES'.ljust(69) + '│')
        print('├' + '─' * 68 + '┤')
        for i, phase in enumerate(analysis['phases'], 1):
            start = format_timestamp(phase.get('start', ''))
            tool_summary = Counter(phase['tools'])
            top_tools = ', '.join(f"{t}({c})" for t, c in tool_summary.most_common(3))
            print(f"│ Phase {i} @ {start}".ljust(69) + '│')
            print(f"│   Tools: {top_tools[:55]}".ljust(69) + '│')
        print('└' + '─' * 68 + '┘')

    # User prompt excerpts
    if analysis['prompt_texts']:
        print()
        print('┌' + '─' * 68 + '┐')
        print('│ USER PROMPTS (excerpts)'.ljust(69) + '│')
        print('├' + '─' * 68 + '┤')
        for i, text in enumerate(analysis['prompt_texts'][:5], 1):
            excerpt = text[:60].replace('\n', ' ')
            print(f"│ [{i}] \"{excerpt}...\"".ljust(69) + '│')
        if len(analysis['prompt_texts']) > 5:
            print(f"│ ... and {len(analysis['prompt_texts']) - 5} more".ljust(69) + '│')
        print('└' + '─' * 68 + '┘')

    print()


def main():
    parser = argparse.ArgumentParser(description='Deep analysis of a Langfuse trace')
    parser.add_argument('trace_id', nargs='?', help='Trace ID to analyze')
    parser.add_argument('--latest', action='store_true', help='Analyze the most recent trace')
    args = parser.parse_args()

    try:
        client = LangfuseClient()
    except ValueError as e:
        print(f"Error: {e}")
        sys.exit(1)

    trace_id = args.trace_id

    if args.latest or not trace_id:
        traces = client.get_traces(limit=1)
        if not traces:
            print("No traces found")
            sys.exit(1)
        trace_id = traces[0].get('id')
        print(f"Analyzing latest trace: {trace_id[:20]}...")

    try:
        analysis = analyze_trace(client, trace_id)
        print_insights(analysis)
    except Exception as e:
        print(f"Error analyzing trace: {e}")
        sys.exit(1)


if __name__ == '__main__':
    main()
