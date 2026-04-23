#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "python-dotenv",
# ]
# ///

import argparse
import json
import os
import sys
import subprocess
from pathlib import Path

try:
    from dotenv import load_dotenv
    load_dotenv()
except ImportError:
    pass  # dotenv is optional

from utils.hook_context import (
    build_session_label,
    extract_latest_todo_snapshot,
    summarize_todos,
)


def get_tts_script_path():
    """
    Determine which TTS script to use based on available API keys and platform.
    Priority order: ElevenLabs > OpenAI > Cross-Platform > pyttsx3
    """
    # Get current script directory and construct utils/tts path
    script_dir = Path(__file__).parent
    tts_dir = script_dir / "utils" / "tts"
    
    # Check for ElevenLabs API key (highest priority)
    if os.getenv('ELEVENLABS_API_KEY'):
        elevenlabs_script = tts_dir / "elevenlabs_tts.py"
        if elevenlabs_script.exists():
            return str(elevenlabs_script)
    
    # Check for OpenAI API key (second priority)
    if os.getenv('OPENAI_API_KEY'):
        openai_script = tts_dir / "openai_tts.py"
        if openai_script.exists():
            return str(openai_script)
    
    # Use cross-platform TTS (third priority - automatically detects best method)
    cross_platform_script = tts_dir / "cross_platform_tts.py"
    if cross_platform_script.exists():
        return str(cross_platform_script)
    
    # Fall back to pyttsx3 (lowest priority)
    pyttsx3_script = tts_dir / "pyttsx3_tts.py"
    if pyttsx3_script.exists():
        return str(pyttsx3_script)
    
    return None


def build_notification_message(input_data):
    """Build a context-aware notification message."""
    label = build_session_label(input_data.get("cwd", os.getcwd()))
    notification_type = input_data.get("notification_type", "")
    engineer_name = os.getenv('ENGINEER_NAME', '').strip()
    prefix = f"{engineer_name}, " if engineer_name else ""

    transcript_path = input_data.get("agent_transcript_path") or input_data.get("transcript_path", "")
    summary = summarize_todos(extract_latest_todo_snapshot(transcript_path))

    if notification_type == "idle_prompt" or input_data.get("message") == "Claude is waiting for your input":
        message = f"{prefix}{label} is waiting for input"
        if summary:
            message = f"{message}. {summary}"
        return message

    if notification_type == "permission_prompt":
        return f"{prefix}{label} needs permission"

    title = (input_data.get("title") or "").strip()
    message = (input_data.get("message") or "").strip()
    if title:
        return f"{prefix}{label}: {title}"
    if message:
        return f"{prefix}{label}: {message}"

    return f"{prefix}{label} needs attention"


def announce_notification(notification_message):
    """Announce that the agent needs user input."""
    try:
        tts_script = get_tts_script_path()
        if not tts_script:
            return  # No TTS scripts available

        # Call the TTS script with the notification message
        subprocess.run([
            "uv", "run", tts_script, notification_message
        ], 
        capture_output=True,  # Suppress output
        timeout=10  # 10-second timeout
        )
        
    except (subprocess.TimeoutExpired, subprocess.SubprocessError, FileNotFoundError):
        # Fail silently if TTS encounters issues
        pass
    except Exception:
        # Fail silently for any other errors
        pass


def main():
    try:
        # Parse command line arguments
        parser = argparse.ArgumentParser()
        parser.add_argument('--notify', action='store_true', help='Enable TTS notifications')
        args = parser.parse_args()
        
        # Read JSON input from stdin
        input_data = json.loads(sys.stdin.read())
        
        # Ensure log directory exists
        import os
        log_dir = os.path.join(os.getcwd(), 'logs')
        os.makedirs(log_dir, exist_ok=True)
        log_file = os.path.join(log_dir, 'notification.json')
        
        # Read existing log data or initialize empty list
        if os.path.exists(log_file):
            with open(log_file, 'r') as f:
                try:
                    log_data = json.load(f)
                except (json.JSONDecodeError, ValueError):
                    log_data = []
        else:
            log_data = []
        
        # Append new data
        log_data.append(input_data)
        
        # Write back to file with formatting
        with open(log_file, 'w') as f:
            json.dump(log_data, f, indent=2)
        
        # Announce notification via TTS only if --notify flag is set
        if args.notify:
            announce_notification(build_notification_message(input_data))
        
        sys.exit(0)
        
    except json.JSONDecodeError:
        # Handle JSON decode errors gracefully
        sys.exit(0)
    except Exception:
        # Handle any other errors gracefully
        sys.exit(0)

if __name__ == '__main__':
    main()
