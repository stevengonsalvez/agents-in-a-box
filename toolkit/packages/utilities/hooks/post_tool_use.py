#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "langfuse>=2.44.0,<3.0.0",
# ]
# ///

import json
import os
import sys
import logging
import hashlib
from pathlib import Path
from datetime import datetime

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

# --- Session-specific file paths ---
def get_session_specific_paths():
    """Generate session-specific paths based on working directory."""
    cwd = os.getcwd()
    # Create a short hash of the working directory for unique identification
    cwd_hash = hashlib.md5(cwd.encode()).hexdigest()[:8]

    return {
        'supervisor_log': f"/tmp/claude_supervisor_{cwd_hash}.log",
        'state_file': f"/tmp/claude_todo_hook_{cwd_hash}.state",
        'project_name': os.path.basename(cwd)
    }

# Get session-specific paths
paths = get_session_specific_paths()

# --- Logging Configuration ---
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - [%(project)s] - %(message)s',
    filename=paths['supervisor_log'],
    filemode='a'
)

# Add project name to all log records
old_factory = logging.getLogRecordFactory()
def record_factory(*args, **kwargs):
    record = old_factory(*args, **kwargs)
    record.project = paths['project_name']
    return record
logging.setLogRecordFactory(record_factory)

def log_to_json_file(input_data):
    """Original functionality: log all tool usage to JSON file."""
    try:
        # Ensure log directory exists
        log_dir = Path.cwd() / 'logs'
        log_dir.mkdir(parents=True, exist_ok=True)
        log_path = log_dir / 'post_tool_use.json'

        # Read existing log data or initialize empty list
        if log_path.exists():
            with open(log_path, 'r') as f:
                try:
                    log_data = json.load(f)
                except (json.JSONDecodeError, ValueError):
                    log_data = []
        else:
            log_data = []

        # Add timestamp to the log entry
        input_data['logged_at'] = datetime.now().isoformat()

        # Append new data
        log_data.append(input_data)

        # Write back to file with formatting
        with open(log_path, 'w') as f:
            json.dump(log_data, f, indent=2)

        return True
    except Exception as e:
        logging.error(f"Failed to log to JSON file: {e}")
        return False

def handle_todo_write_reflection(input_data):
    """Handle TodoWrite-specific reflection prompting."""
    try:
        tool_input_data = input_data.get("tool_input", {})
        todo_objects = tool_input_data.get("todos", [])

        if not todo_objects:
            logging.info("TodoWrite called, but 'todos' list is empty. Skipping reflection.")
            return None

        # Extract todo content for hashing
        tasks_to_process_content = [task.get("content", "") for task in todo_objects]
        todo_content_full = "\n".join(tasks_to_process_content)

        # Calculate hash of current todo list
        current_hash = hashlib.md5(todo_content_full.encode()).hexdigest()

        # Check if state file exists and compare hashes
        last_hash = ""
        if os.path.exists(paths['state_file']):
            try:
                with open(paths['state_file'], 'r') as f:
                    state_data = json.load(f)
                    last_hash = state_data.get('hash', '')
                    last_time = state_data.get('timestamp', '')
                    logging.debug(f"Last state: hash={last_hash[:8]}..., time={last_time}")
            except (json.JSONDecodeError, IOError):
                # State file corrupted or old format, treat as new
                pass

        if current_hash == last_hash:
            logging.info("Todo list has not changed. Skipping reflection prompt.")
            return None

        logging.info(f"New todo list detected (hash: {current_hash[:8]}...). Preparing reflection prompt.")

        # The reflection prompt to inject
        reflection_prompt = """
**Supervisor's Prompt: Review and Parallelize the Plan**

The initial plan has been drafted. Now, **think** to optimize its execution.

1. **Analyze Dependencies**: Critically review the list of tasks.
2. **Group for Parallelism**: Identify any tasks that are independent and can be executed concurrently. Group them into a parallel stage.
3. **Format for Parallel Execution**: To run a group of tasks in parallel, you **must** place multiple `<invoke name="Task">` calls inside a **single** `<function_calls>` block in your response.

Reminder of example format for running two tasks in parallel:
```xml
<function_calls>
  <invoke name="Task">
    <parameter name="description">First parallel task...</parameter>
    <parameter name="prompt">Details for the first task...</parameter>
    <parameter name="subagent_type">appropriate-agent-type</parameter>
  </invoke>
  <invoke name="Task">
    <parameter name="description">Second parallel task...</parameter>
    <parameter name="prompt">Details for the second task...</parameter>
    <parameter name="subagent_type">appropriate-agent-type</parameter>
  </invoke>
</function_calls>
```

Please present your analysis of parallel stages and then proceed with the first stage using the correct format.
"""

        # Save new state with timestamp
        state_data = {
            'hash': current_hash,
            'timestamp': datetime.now().isoformat(),
            'todo_count': len(todo_objects)
        }
        with open(paths['state_file'], 'w') as f:
            json.dump(state_data, f, indent=2)
        logging.info(f"Updated state file with new hash: {current_hash[:8]}...")

        return reflection_prompt

    except Exception as e:
        logging.exception(f"Error in TodoWrite reflection handler: {e}")
        return None

def main():
    """Main entry point for the hook."""
    logging.info("--- Post-Tool-Use Hook Triggered ---")

    try:
        # Read JSON input from stdin
        input_data = json.load(sys.stdin)

        # Log tool name for debugging
        tool_name = input_data.get("tool_name", "unknown")
        logging.info(f"Tool used: {tool_name}")

        # Always log to JSON file (original functionality)
        log_to_json_file(input_data)

        # Log to Langfuse (no-op if not configured)
        tracer = get_tracer()
        if tracer:
            tool_result = input_data.get("tool_response", {})
            tracer.end_tool_span(tool_name, tool_result)

        # Check if this is a TodoWrite tool call
        if tool_name == "TodoWrite":
            reflection_prompt = handle_todo_write_reflection(input_data)

            if reflection_prompt:
                # Return the reflection prompt to Claude
                response = {
                    "hookSpecificOutput": {
                        "hookEventName": "PostToolUse",
                        "additionalContext": reflection_prompt
                    }
                }
                logging.info("Injecting reflection prompt for task parallelization.")
                print(json.dumps(response), flush=True)
                sys.exit(0)

        # For all other tools or when no reflection needed, exit cleanly
        sys.exit(0)

    except json.JSONDecodeError as e:
        logging.error(f"Failed to parse JSON input: {e}")
        sys.exit(0)
    except Exception as e:
        logging.exception("An unexpected error occurred in the post-tool-use hook.")
        sys.exit(0)

if __name__ == '__main__':
    main()
