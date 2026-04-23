#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "pytest>=7.0.0",
#     "python-dotenv",
# ]
# ///

# ABOUTME: Test suite for Claude Code hooks to ensure they trigger expected behaviors
# Tests the pre_compact handover trigger and session_start git status functionality

import json
import subprocess
import os
import pytest
import importlib.util
from pathlib import Path


def load_module(module_name: str, path: Path):
    """Load a hook script as a module for direct unit testing."""
    spec = importlib.util.spec_from_file_location(module_name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


HOOKS_DIR = Path(__file__).parent
UTILS_DIR = HOOKS_DIR / "utils"


class TestPreCompactHook:
    """Test pre_compact.py hook triggers /handover command."""
    
    def test_pre_compact_triggers_handover_command(self):
        """Test that pre_compact hook triggers /handover command when --handover flag is used."""
        # Arrange
        hook_path = Path(__file__).parent / "pre_compact.py"
        test_input = {
            "session_id": "test-session-123",
            "transcript_path": "/path/to/transcript.jsonl", 
            "trigger": "manual",
            "custom_instructions": "Test handover"
        }
        
        # Act
        result = subprocess.run([
            "uv", "run", str(hook_path), "--handover"
        ], 
        input=json.dumps(test_input),
        text=True,
        capture_output=True
        )
        
        # Assert - Should trigger handover command via JSON output
        assert result.returncode == 0
        assert "hookSpecificOutput" in result.stdout
        assert "handover" in result.stdout.lower()
    
    def test_pre_compact_without_handover_flag_works_normally(self):
        """Test that pre_compact hook works normally without --handover flag."""
        # Arrange
        hook_path = Path(__file__).parent / "pre_compact.py"
        test_input = {
            "session_id": "test-session-456",
            "transcript_path": "/path/to/transcript.jsonl",
            "trigger": "auto"
        }
        
        # Act
        result = subprocess.run([
            "uv", "run", str(hook_path), "--verbose"
        ],
        input=json.dumps(test_input),
        text=True, 
        capture_output=True
        )
        
        # Assert - Should work normally without handover
        assert result.returncode == 0
        assert "/handover" not in result.stdout


class TestSessionStartHook:
    """Test session_start.py hook runs git status."""
    
    def test_session_start_runs_git_status(self):
        """Test that session_start hook runs git status when --git-status flag is used."""
        # Arrange
        hook_path = Path(__file__).parent / "session_start.py"
        test_input = {
            "session_id": "session-789",
            "source": "startup"
        }
        
        # Act
        result = subprocess.run([
            "uv", "run", str(hook_path), "--git-status"
        ],
        input=json.dumps(test_input),
        text=True,
        capture_output=True
        )
        
        # Assert - Should run git status via JSON output
        assert result.returncode == 0
        # Should contain git status information in JSON format
        assert "hookSpecificOutput" in result.stdout
        output_lower = result.stdout.lower()
        assert any(word in output_lower for word in ["git", "status", "branch", "changes"])
    
    def test_session_start_without_git_flag_works_normally(self):
        """Test that session_start hook works normally without --git-status flag."""
        # Arrange  
        hook_path = Path(__file__).parent / "session_start.py"
        test_input = {
            "session_id": "session-101112", 
            "source": "resume"
        }
        
        # Act
        result = subprocess.run([
            "uv", "run", str(hook_path)
        ],
        input=json.dumps(test_input),
        text=True,
        capture_output=True
        )
        
        # Assert - Should work normally
        assert result.returncode == 0


class TestHooksIntegration:
    """Test hooks work together without conflicts."""
    
    def test_both_hooks_can_run_independently(self):
        """Test that both hooks can run without interfering with each other."""
        # Test pre_compact hook
        pre_compact_path = Path(__file__).parent / "pre_compact.py"
        pre_compact_input = {
            "session_id": "integration-test-1",
            "transcript_path": "/tmp/test.jsonl",
            "trigger": "manual"
        }
        
        pre_compact_result = subprocess.run([
            "uv", "run", str(pre_compact_path)
        ],
        input=json.dumps(pre_compact_input),
        text=True,
        capture_output=True
        )
        
        # Test session_start hook
        session_start_path = Path(__file__).parent / "session_start.py" 
        session_start_input = {
            "session_id": "integration-test-2",
            "source": "startup"
        }
        
        session_start_result = subprocess.run([
            "uv", "run", str(session_start_path)
        ],
        input=json.dumps(session_start_input),
        text=True,
        capture_output=True
        )
        
        # Assert both work independently
        assert pre_compact_result.returncode == 0
        assert session_start_result.returncode == 0


class TestHookContextHelpers:
    """Test shared hook context helpers."""

    def test_extract_latest_todo_snapshot_uses_latest_snapshot(self, tmp_path):
        helper = load_module("hook_context", UTILS_DIR / "hook_context.py")
        transcript = tmp_path / "transcript.jsonl"
        transcript.write_text(
            "\n".join(
                [
                    json.dumps({
                        "type": "assistant",
                        "message": {
                            "content": [{
                                "type": "tool_use",
                                "name": "TodoWrite",
                                "input": {
                                    "todos": [
                                        {"content": "Task one", "status": "pending"},
                                        {"content": "Task two", "status": "active"},
                                    ]
                                },
                            }]
                        },
                    }),
                    json.dumps({
                        "type": "assistant",
                        "message": {
                            "content": [{
                                "type": "tool_use",
                                "name": "TodoWrite",
                                "input": {
                                    "todos": [
                                        {"content": "Task one", "status": "completed"},
                                        {"content": "Task three", "status": "in_progress"},
                                        {"content": "Task four", "status": "pending"},
                                    ]
                                },
                            }]
                        },
                    }),
                ]
            )
        )

        snapshot = helper.extract_latest_todo_snapshot(str(transcript))

        assert snapshot is not None
        assert snapshot["done"] == 1
        assert snapshot["in_progress"] == 1
        assert snapshot["pending"] == 1
        assert helper.summarize_todos(snapshot) == "1 pending, 1 in progress"

    def test_extract_latest_todo_snapshot_scans_past_large_trailing_output(self, tmp_path):
        helper = load_module("hook_context_large_tail", UTILS_DIR / "hook_context.py")
        transcript = tmp_path / "large-tail.jsonl"
        large_output = "x" * 120000
        transcript.write_text(
            "\n".join(
                [
                    json.dumps({
                        "type": "assistant",
                        "message": {
                            "content": [{
                                "type": "tool_use",
                                "name": "TodoWrite",
                                "input": {
                                    "todos": [
                                        {"content": "Review logs", "status": "pending"},
                                        {"content": "Patch fix", "status": "active"},
                                    ]
                                },
                            }]
                        },
                    }),
                    json.dumps({
                        "type": "assistant",
                        "message": {
                            "content": [{
                                "type": "text",
                                "text": large_output,
                            }]
                        },
                    }),
                ]
            )
        )

        snapshot = helper.extract_latest_todo_snapshot(str(transcript))

        assert snapshot is not None
        assert snapshot["pending"] == 1
        assert snapshot["in_progress"] == 1

    def test_build_session_label_falls_back_without_git(self, tmp_path):
        helper = load_module("hook_context_label", UTILS_DIR / "hook_context.py")
        workspace = tmp_path / "my_test-worktree"
        workspace.mkdir()

        assert helper.build_session_label(str(workspace)) == "my test worktree"


class TestContextAwareMessages:
    """Test deterministic message builders for TTS hooks."""

    def test_notification_message_uses_idle_prompt_and_todo_summary(self, tmp_path, monkeypatch):
        helper = load_module("hook_context_notification", UTILS_DIR / "hook_context.py")
        monkeypatch.setenv("ENGINEER_NAME", "Stevie")

        transcript = tmp_path / "notification.jsonl"
        transcript.write_text(
            json.dumps({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "name": "TodoWrite",
                        "input": {
                            "todos": [
                                {"content": "Review code", "status": "pending"},
                                {"content": "Implement fix", "status": "in_progress"},
                            ]
                        },
                    }]
                },
            })
        )

        notification = load_module("notification_hook", HOOKS_DIR / "notification.py")
        monkeypatch.setattr(notification, "build_session_label", helper.build_session_label)
        monkeypatch.setattr(notification, "extract_latest_todo_snapshot", helper.extract_latest_todo_snapshot)
        monkeypatch.setattr(notification, "summarize_todos", helper.summarize_todos)

        message = notification.build_notification_message({
            "cwd": str(tmp_path / "fix_post-hook"),
            "notification_type": "idle_prompt",
            "message": "Claude is waiting for your input",
            "transcript_path": str(transcript),
        })

        assert "Stevie" in message
        assert "waiting for input" in message
        assert "1 pending, 1 in progress" in message

    def test_stop_message_uses_label_and_summary(self, tmp_path, monkeypatch):
        transcript = tmp_path / "stop.jsonl"
        transcript.write_text(
            json.dumps({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "name": "TodoWrite",
                        "input": {
                            "todos": [
                                {"content": "Ship it", "status": "pending"},
                            ]
                        },
                    }]
                },
            })
        )

        stop_module = load_module("stop_hook", HOOKS_DIR / "stop.py")
        message = stop_module.build_completion_message({
            "cwd": str(tmp_path / "feature-demo"),
            "transcript_path": str(transcript),
        })

        assert "complete" in message
        assert "feature demo" in message
        assert "1 pending" in message

    def test_subagent_message_prefers_agent_transcript_and_agent_type(self, tmp_path):
        main_transcript = tmp_path / "main.jsonl"
        main_transcript.write_text("")
        agent_transcript = tmp_path / "agent.jsonl"
        agent_transcript.write_text(
            json.dumps({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "name": "TodoWrite",
                        "input": {
                            "todos": [
                                {"content": "Investigate", "status": "active"},
                            ]
                        },
                    }]
                },
            })
        )

        subagent_module = load_module("subagent_stop_hook", HOOKS_DIR / "subagent_stop.py")
        message = subagent_module.build_subagent_completion_message({
            "cwd": str(tmp_path / "agent-worktree"),
            "agent_type": "explorer",
            "transcript_path": str(main_transcript),
            "agent_transcript_path": str(agent_transcript),
        })

        assert message.startswith("explorer complete in")
        assert "1 in progress" in message


if __name__ == "__main__":
    # Run tests if executed directly
    import sys
    pytest.main([__file__] + sys.argv[1:])
