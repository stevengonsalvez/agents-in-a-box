#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "requests>=2.28.0",
# ]
# ///
"""
Langfuse reflect command - analyzes session traces to extract learnings.

Scans user prompts for correction signals and success patterns,
then proposes updates to agent files.

Usage:
    python reflect.py [--sessions N] [--since YYYY-MM-DD]
"""

import sys
import re
import argparse
from datetime import datetime, timedelta
from collections import defaultdict
from typing import List, Dict, Any, Tuple

try:
    from api import LangfuseClient, format_timestamp
except ImportError:
    from .api import LangfuseClient, format_timestamp


# Signal detection patterns
HIGH_CONFIDENCE_PATTERNS = [
    (r'\b(never|don\'t ever|do not ever)\b.*\b(do|use|create|make|add|write)\b', 'prohibition'),
    (r'\b(always|must always|should always)\b.*\b(check|verify|ensure|use|add)\b', 'requirement'),
    (r'\b(stop|quit|cease)\b.*\b(doing|using|creating)\b', 'prohibition'),
    (r'\bwrong\b.*\b(approach|way|method)\b', 'correction'),
    (r'\bthat\'s not (right|correct|what I)\b', 'correction'),
    (r'\b(fix|correct|change)\b.*\bthis\b', 'correction'),
    (r'\bI (said|told you|asked)\b.*\bnot\b', 'reminder'),
]

MEDIUM_CONFIDENCE_PATTERNS = [
    (r'\b(perfect|exactly|great job|well done)\b', 'approval'),
    (r'\bthis (works|is correct|looks good)\b', 'approval'),
    (r'\bgood (approach|pattern|solution)\b', 'approval'),
    (r'\bkeep (doing|using) this\b', 'approval'),
    (r'\bI (like|prefer|want)\b.*\bthis (way|approach|pattern)\b', 'preference'),
]

LOW_CONFIDENCE_PATTERNS = [
    (r'\bmaybe (we should|try|consider)\b', 'suggestion'),
    (r'\bit would be (nice|good|better)\b', 'suggestion'),
    (r'\bin this (case|project|context)\b', 'context'),
]

# Agent file mapping based on learning category
AGENT_MAPPINGS = {
    'code_style': ['code-reviewer', 'superstar-engineer'],
    'architecture': ['solution-architect', 'architecture-reviewer'],
    'testing': ['test-writer-fixer', 'integration-tests'],
    'security': ['security-agent', 'code-reviewer'],
    'performance': ['performance-optimizer'],
    'documentation': ['documentation-specialist'],
    'git': ['CLAUDE.md'],
    'tools': ['superstar-engineer', 'CLAUDE.md'],
    'process': ['CLAUDE.md'],
}

# Keywords for category detection
CATEGORY_KEYWORDS = {
    'code_style': ['format', 'naming', 'style', 'convention', 'indent', 'lint'],
    'architecture': ['pattern', 'design', 'structure', 'boundary', 'interface', 'module'],
    'testing': ['test', 'coverage', 'mock', 'assert', 'spec'],
    'security': ['security', 'auth', 'permission', 'inject', 'xss', 'csrf'],
    'performance': ['performance', 'slow', 'optimize', 'cache', 'memory'],
    'documentation': ['document', 'readme', 'comment', 'jsdoc', 'docstring'],
    'git': ['commit', 'branch', 'merge', 'push', 'git'],
    'tools': ['tool', 'command', 'cli', 'bash', 'script'],
}


def detect_signals(text: str) -> List[Dict[str, Any]]:
    """Detect correction and success signals in text."""
    signals = []
    text_lower = text.lower()

    # High confidence
    for pattern, signal_type in HIGH_CONFIDENCE_PATTERNS:
        matches = re.findall(pattern, text_lower, re.IGNORECASE)
        if matches:
            signals.append({
                'confidence': 'high',
                'type': signal_type,
                'pattern': pattern,
                'text': text[:200],
            })

    # Medium confidence
    for pattern, signal_type in MEDIUM_CONFIDENCE_PATTERNS:
        matches = re.findall(pattern, text_lower, re.IGNORECASE)
        if matches:
            signals.append({
                'confidence': 'medium',
                'type': signal_type,
                'pattern': pattern,
                'text': text[:200],
            })

    # Low confidence
    for pattern, signal_type in LOW_CONFIDENCE_PATTERNS:
        matches = re.findall(pattern, text_lower, re.IGNORECASE)
        if matches:
            signals.append({
                'confidence': 'low',
                'type': signal_type,
                'pattern': pattern,
                'text': text[:200],
            })

    return signals


def detect_category(text: str) -> str:
    """Detect learning category from text."""
    text_lower = text.lower()

    scores = defaultdict(int)
    for category, keywords in CATEGORY_KEYWORDS.items():
        for keyword in keywords:
            if keyword in text_lower:
                scores[category] += 1

    if scores:
        return max(scores, key=scores.get)

    return 'process'  # default


def get_target_files(category: str) -> List[str]:
    """Get target agent files for a category."""
    return AGENT_MAPPINGS.get(category, ['CLAUDE.md'])


def analyze_traces(client: LangfuseClient, limit: int = 10, since: str = None) -> Dict[str, Any]:
    """Analyze traces for learnings."""

    traces = client.get_traces(limit=limit)

    # Filter by date if specified
    if since:
        since_dt = datetime.fromisoformat(since)
        traces = [t for t in traces if t.get('timestamp', '')[:10] >= since]

    all_signals = {
        'high': [],
        'medium': [],
        'low': [],
    }

    session_count = 0
    prompt_count = 0

    for trace in traces:
        trace_id = trace.get('id')
        session_id = trace.get('sessionId', 'unknown')
        timestamp = trace.get('timestamp', '')[:19]

        try:
            observations = client.get_observations(trace_id)

            # Find user prompts
            prompts = [o for o in observations if o.get('name') == 'user-prompt']

            for prompt in prompts:
                prompt_count += 1
                input_data = prompt.get('input', {})

                if isinstance(input_data, dict):
                    text = input_data.get('prompt', '')
                else:
                    text = str(input_data)

                if not text:
                    continue

                signals = detect_signals(text)

                for signal in signals:
                    signal['session_id'] = session_id
                    signal['trace_id'] = trace_id
                    signal['timestamp'] = timestamp
                    signal['category'] = detect_category(text)
                    signal['target_files'] = get_target_files(signal['category'])

                    all_signals[signal['confidence']].append(signal)

            session_count += 1

        except Exception as e:
            print(f"Warning: Failed to analyze trace {trace_id}: {e}", file=sys.stderr)

    return {
        'session_count': session_count,
        'prompt_count': prompt_count,
        'signals': all_signals,
        'time_range': {
            'start': traces[-1].get('timestamp', '')[:10] if traces else 'N/A',
            'end': traces[0].get('timestamp', '')[:10] if traces else 'N/A',
        }
    }


def print_results(results: Dict[str, Any]):
    """Print analysis results."""
    print()
    print('═' * 70)
    print('  LANGFUSE REFLECT - Session Analysis')
    print('═' * 70)
    print()
    print(f"Sessions Analyzed: {results['session_count']}")
    print(f"User Prompts Scanned: {results['prompt_count']}")
    print(f"Time Range: {results['time_range']['start']} to {results['time_range']['end']}")
    print()

    signals = results['signals']

    # High confidence
    high = signals['high']
    print('┌' + '─' * 68 + '┐')
    print(f"│ HIGH CONFIDENCE SIGNALS ({len(high)} found)".ljust(69) + '│')
    print('├' + '─' * 68 + '┤')

    if high:
        for i, s in enumerate(high[:10], 1):
            text_preview = s['text'][:50].replace('\n', ' ')
            print(f"│ [{i}] \"{text_preview}...\"".ljust(69) + '│')
            print(f"│     Session: {s['session_id'][:20]}... @ {s['timestamp']}".ljust(69) + '│')
            print(f"│     Type: {s['type']} | Category: {s['category']}".ljust(69) + '│')
            print(f"│     Target: {', '.join(s['target_files'])}".ljust(69) + '│')
            if i < len(high) and i < 10:
                print('│'.ljust(70) + '│')
    else:
        print('│ No high-confidence signals detected'.ljust(69) + '│')

    print('└' + '─' * 68 + '┘')
    print()

    # Medium confidence
    medium = signals['medium']
    print('┌' + '─' * 68 + '┐')
    print(f"│ MEDIUM CONFIDENCE SIGNALS ({len(medium)} found)".ljust(69) + '│')
    print('├' + '─' * 68 + '┤')

    if medium:
        for i, s in enumerate(medium[:5], 1):
            text_preview = s['text'][:50].replace('\n', ' ')
            print(f"│ [{i}] \"{text_preview}...\"".ljust(69) + '│')
            print(f"│     Type: {s['type']} | Category: {s['category']}".ljust(69) + '│')
            if i < len(medium) and i < 5:
                print('│'.ljust(70) + '│')
    else:
        print('│ No medium-confidence signals detected'.ljust(69) + '│')

    print('└' + '─' * 68 + '┘')
    print()

    # Low confidence (just count)
    low = signals['low']
    print(f"Low confidence signals: {len(low)} (use --verbose to see)")
    print()

    # Summary
    if high:
        print('═' * 70)
        print('  PROPOSED ACTIONS')
        print('═' * 70)
        print()

        # Group by target file
        by_target = defaultdict(list)
        for s in high:
            for target in s['target_files']:
                by_target[target].append(s)

        for target, target_signals in by_target.items():
            print(f"  {target}:")
            for s in target_signals[:3]:
                action = "Add prohibition" if s['type'] == 'prohibition' else "Add requirement"
                print(f"    - {action}: {s['text'][:40]}...")
            print()

        print("Review and apply these learnings with /langfuse:apply")
        print()


def main():
    parser = argparse.ArgumentParser(description='Analyze Langfuse traces for learnings')
    parser.add_argument('--sessions', type=int, default=10, help='Number of sessions to analyze')
    parser.add_argument('--since', type=str, help='Only analyze sessions since date (YYYY-MM-DD)')
    parser.add_argument('--verbose', action='store_true', help='Show low-confidence signals')
    args = parser.parse_args()

    try:
        client = LangfuseClient()
    except ValueError as e:
        print(f"Error: {e}")
        print("Set LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY in ~/.secrets")
        sys.exit(1)

    print("Analyzing Langfuse traces...")
    results = analyze_traces(client, limit=args.sessions, since=args.since)
    print_results(results)


if __name__ == '__main__':
    main()
