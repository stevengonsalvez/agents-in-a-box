#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "python-dotenv",
#     "langfuse>=2.44.0,<3.0.0",
# ]
# ///

import argparse
import json
import os
import sys
import subprocess
from pathlib import Path
from datetime import datetime

try:
    from dotenv import load_dotenv
    load_dotenv()
except ImportError:
    pass  # dotenv is optional

# Langfuse integration (optional - no-op if not configured)
_langfuse_enabled = os.getenv('LANGFUSE_ENABLED', 'false').lower() == 'true'
if _langfuse_enabled:
    try:
        from utils.langfuse import get_tracer
    except ImportError:
        _langfuse_enabled = False
        get_tracer = lambda: None
else:
    get_tracer = lambda: None


def log_session_start(input_data):
    """Log session start event to logs directory."""
    # Ensure logs directory exists
    log_dir = Path("logs")
    log_dir.mkdir(parents=True, exist_ok=True)
    log_file = log_dir / 'session_start.json'
    
    # Read existing log data or initialize empty list
    if log_file.exists():
        with open(log_file, 'r') as f:
            try:
                log_data = json.load(f)
            except (json.JSONDecodeError, ValueError):
                log_data = []
    else:
        log_data = []
    
    # Append the entire input data
    log_data.append(input_data)
    
    # Write back to file with formatting
    with open(log_file, 'w') as f:
        json.dump(log_data, f, indent=2)


def get_git_status():
    """Get current git status information."""
    try:
        # Get current branch
        branch_result = subprocess.run(
            ['git', 'rev-parse', '--abbrev-ref', 'HEAD'],
            capture_output=True,
            text=True,
            timeout=5
        )
        current_branch = branch_result.stdout.strip() if branch_result.returncode == 0 else "unknown"
        
        # Get uncommitted changes count
        status_result = subprocess.run(
            ['git', 'status', '--porcelain'],
            capture_output=True,
            text=True,
            timeout=5
        )
        if status_result.returncode == 0:
            changes = status_result.stdout.strip().split('\n') if status_result.stdout.strip() else []
            uncommitted_count = len(changes)
        else:
            uncommitted_count = 0
        
        return current_branch, uncommitted_count
    except Exception:
        return None, None


def get_recent_issues():
    """Get recent GitHub issues if gh CLI is available."""
    try:
        # Check if gh is available
        gh_check = subprocess.run(['which', 'gh'], capture_output=True)
        if gh_check.returncode != 0:
            return None
        
        # Get recent open issues
        result = subprocess.run(
            ['gh', 'issue', 'list', '--limit', '5', '--state', 'open'],
            capture_output=True,
            text=True,
            timeout=10
        )
        if result.returncode == 0 and result.stdout.strip():
            return result.stdout.strip()
    except Exception:
        pass
    return None


def load_development_context(source):
    """Load relevant development context based on session source."""
    context_parts = []
    
    # Add timestamp
    context_parts.append(f"Session started at: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    context_parts.append(f"Session source: {source}")
    
    # Add git information
    branch, changes = get_git_status()
    if branch:
        context_parts.append(f"Git branch: {branch}")
        if changes > 0:
            context_parts.append(f"Uncommitted changes: {changes} files")
    
    # Load project-specific context files if they exist
    context_files = [
        ".claude/CLAUDE.md",
        ".claude/TODO.md",
        "TODO.md",
        ".github/ISSUE_TEMPLATE.md"
    ]
    
    for file_path in context_files:
        if Path(file_path).exists():
            try:
                with open(file_path, 'r') as f:
                    content = f.read().strip()
                    if content:
                        context_parts.append(f"\n--- Content from {file_path} ---")
                        context_parts.append(content[:1000])  # Limit to first 1000 chars
            except Exception:
                pass
    
    # Add recent issues if available
    issues = get_recent_issues()
    if issues:
        context_parts.append("\n--- Recent GitHub Issues ---")
        context_parts.append(issues)
    
    return "\n".join(context_parts)


def load_tmux_sessions():
    """Load and display active tmux development sessions."""
    try:
        # Find all tmux session metadata files
        session_files = list(Path.cwd().glob('.tmux-*-session.json'))

        if not session_files:
            return "📋 No active development sessions found"

        sessions = []
        for file in session_files:
            try:
                with open(file, 'r') as f:
                    data = json.load(f)

                    # Verify session still exists
                    session_name = data.get('session')
                    if session_name:
                        check_result = subprocess.run(
                            ['tmux', 'has-session', '-t', session_name],
                            capture_output=True,
                            timeout=2
                        )

                        if check_result.returncode == 0:
                            sessions.append({
                                'type': file.stem.replace('.tmux-', '').replace('-session', ''),
                                'data': data
                            })
            except Exception:
                continue

        if not sessions:
            return "📋 No active development sessions found"

        # Format as table
        lines = [
            "═══════════════════════════════════════════════════════════════",
            "  Active Development Sessions",
            "═══════════════════════════════════════════════════════════════",
        ]

        for sess in sessions:
            data = sess['data']
            lines.append(f"\n  [{sess['type'].upper()}] {data.get('session')}")
            lines.append(f"  Project: {data.get('project_name', 'N/A')}")

            branch = data.get('branch')
            if branch and branch != 'main':
                lines.append(f"  Branch: {branch}")

            if 'dev_port' in data and data['dev_port']:
                lines.append(f"  Port: http://localhost:{data['dev_port']}")

            if 'environment' in data:
                lines.append(f"  Environment: {data['environment']}")

            lines.append(f"  Attach: tmux attach -t {data.get('session')}")

        lines.append("\n═══════════════════════════════════════════════════════════════")

        return "\n".join(lines)

    except Exception as e:
        return f"Failed to load tmux sessions: {e}"


def main():
    try:
        # Parse command line arguments
        parser = argparse.ArgumentParser()
        parser.add_argument('--load-context', action='store_true',
                          help='Load development context at session start')
        parser.add_argument('--announce', action='store_true',
                          help='Announce session start via TTS')
        parser.add_argument('--git-status', action='store_true',
                          help='Run git status and display current repository state')
        args = parser.parse_args()
        
        # Read JSON input from stdin
        input_data = json.loads(sys.stdin.read())
        
        # Extract fields
        session_id = input_data.get('session_id', 'unknown')
        source = input_data.get('source', 'unknown')  # "startup", "resume", or "clear"
        
        # Log the session start event
        log_session_start(input_data)

        # Start Langfuse trace (no-op if not configured)
        tracer = get_tracer()
        if tracer:
            branch, changes = get_git_status()
            tracer.start_session_trace(
                session_id=session_id,
                source=source,
                git_branch=branch,
                uncommitted_changes=changes
            )

        # Run git status if requested
        if args.git_status:
            git_status_info = []
            try:
                # Run git status --porcelain for machine-readable output
                status_result = subprocess.run(
                    ['git', 'status', '--porcelain', '--branch'],
                    capture_output=True,
                    text=True,
                    timeout=10
                )
                
                if status_result.returncode == 0:
                    git_output = status_result.stdout.strip()
                    if git_output:
                        git_status_info.append(f"Git Status:\n{git_output}")
                    
                    # Also run a more detailed status for human readability
                    detailed_result = subprocess.run(
                        ['git', 'status', '--short'],
                        capture_output=True,
                        text=True,
                        timeout=10
                    )
                    
                    if detailed_result.returncode == 0 and detailed_result.stdout.strip():
                        git_status_info.append(f"Changes Summary:\n{detailed_result.stdout.strip()}")
                else:
                    git_status_info.append("Git status unavailable (not a git repository or git not found)")
                    
            except Exception as e:
                git_status_info.append(f"Failed to run git status: {e}")
            
            # Store git status for potential combination with tmux sessions
            # (Don't exit yet, combine with tmux sessions below)

        # Always load tmux sessions
        tmux_sessions = load_tmux_sessions()

        # Combine git status (if requested) with tmux sessions
        context_parts = []
        if args.git_status and git_status_info:
            context_parts.extend(git_status_info)

        if tmux_sessions:
            context_parts.append(tmux_sessions)

        # If we have any context to display, output it
        if context_parts:
            output = {
                "hookSpecificOutput": {
                    "hookEventName": "SessionStart",
                    "additionalContext": "\n\n".join(context_parts)
                }
            }
            print(json.dumps(output))
            sys.exit(0)

        # Load development context if requested
        if args.load_context:
            context = load_development_context(source)
            if context:
                # Using JSON output to add context
                output = {
                    "hookSpecificOutput": {
                        "hookEventName": "SessionStart",
                        "additionalContext": context
                    }
                }
                print(json.dumps(output))
                sys.exit(0)
        
        # Announce session start if requested
        if args.announce:
            try:
                # Try to use TTS to announce session start
                script_dir = Path(__file__).parent
                tts_script = script_dir / "utils" / "tts" / "pyttsx3_tts.py"
                
                if tts_script.exists():
                    messages = {
                        "startup": "Claude Code session started",
                        "resume": "Resuming previous session",
                        "clear": "Starting fresh session"
                    }
                    message = messages.get(source, "Session started")
                    
                    subprocess.run(
                        ["uv", "run", str(tts_script), message],
                        capture_output=True,
                        timeout=5
                    )
            except Exception:
                pass
        
        # Success
        sys.exit(0)
        
    except json.JSONDecodeError:
        # Handle JSON decode errors gracefully
        sys.exit(0)
    except Exception:
        # Handle any other errors gracefully
        sys.exit(0)


if __name__ == '__main__':
    main()
