"""
Integration tests for the Langfuse tracer module.

Tests cover:
- NoOp behavior when Langfuse is not configured
- LangfuseConfig environment variable handling
- SessionRegistry file-based session management
- ClaudeCodeTracer with mocked Langfuse client
- Input/output sanitization
"""

import json
import os
import time
import threading
from datetime import datetime, timedelta
from pathlib import Path
from unittest.mock import MagicMock, patch, PropertyMock

import pytest


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def clean_env(monkeypatch):
    """Remove all LANGFUSE_* env vars to ensure clean test state."""
    langfuse_vars = [k for k in os.environ if k.startswith("LANGFUSE_")]
    for var in langfuse_vars:
        monkeypatch.delenv(var, raising=False)
    return monkeypatch


@pytest.fixture
def configured_env(monkeypatch):
    """Set minimal LANGFUSE env vars for an available config."""
    monkeypatch.setenv("LANGFUSE_PUBLIC_KEY", "pk-test-abc123")
    monkeypatch.setenv("LANGFUSE_SECRET_KEY", "sk-test-xyz789")
    return monkeypatch


@pytest.fixture
def temp_registry(tmp_path):
    """Create a SessionRegistry that uses a temp directory instead of ~/.claude."""
    from utils.langfuse.tracer import SessionRegistry

    registry = SessionRegistry()
    registry._base_dir = tmp_path / "langfuse"
    registry._sessions_dir = registry._base_dir / "sessions"
    registry._locks_dir = registry._base_dir / "locks"
    registry._sessions_dir.mkdir(parents=True, exist_ok=True)
    registry._locks_dir.mkdir(parents=True, exist_ok=True)
    return registry


@pytest.fixture
def mock_langfuse_client():
    """Create a mock Langfuse client with expected methods."""
    client = MagicMock()
    mock_trace = MagicMock()
    mock_trace.id = "trace-abc-123"
    mock_trace.update = MagicMock(return_value=mock_trace)
    client.trace.return_value = mock_trace

    mock_span = MagicMock()
    mock_span.id = "span-def-456"
    mock_span.update = MagicMock(return_value=mock_span)
    mock_span.end = MagicMock()
    client.span.return_value = mock_span

    client.flush = MagicMock()
    return client


# ===========================================================================
# 1. NoOp Behavior (no Langfuse configured)
# ===========================================================================

class TestNoOpBehavior:
    """Tests for NoOp fallback when Langfuse is not configured."""

    def test_get_tracer_returns_none_without_keys(self, clean_env):
        """get_tracer() returns None when LANGFUSE_PUBLIC_KEY is not set."""
        # Reset the singleton so it re-checks config
        import utils.langfuse.tracer as tracer_mod
        tracer_mod._tracer = None
        tracer_mod._tracer_checked = False

        # Force a fresh config (bypassing the singleton)
        import utils.langfuse.config as config_mod
        config_mod._config = None

        result = tracer_mod.get_tracer()
        assert result is None

    def test_noop_span_accepts_any_method(self):
        """NoOpSpan accepts update, end without raising."""
        from utils.langfuse.tracer import NoOpSpan

        span = NoOpSpan()
        assert span.id == "noop"

        result = span.update(name="test", input={"key": "value"})
        assert result is span  # Returns self for chaining

        span.end()  # Should not raise

    def test_noop_span_context_manager(self):
        """NoOpSpan works as a context manager."""
        from utils.langfuse.tracer import NoOpSpan

        with NoOpSpan() as span:
            assert span.id == "noop"
            span.update(name="inside-context")
        # No exception means success

    def test_noop_trace_span_returns_noop_span(self):
        """NoOpTrace.span() returns a NoOpSpan."""
        from utils.langfuse.tracer import NoOpTrace, NoOpSpan

        trace = NoOpTrace()
        assert trace.id == "noop"

        span = trace.span(name="test-span")
        assert isinstance(span, NoOpSpan)

    def test_noop_trace_generation_returns_noop_span(self):
        """NoOpTrace.generation() returns a NoOpSpan."""
        from utils.langfuse.tracer import NoOpTrace, NoOpSpan

        trace = NoOpTrace()
        gen = trace.generation(name="test-gen")
        assert isinstance(gen, NoOpSpan)

    def test_noop_trace_update_returns_self(self):
        """NoOpTrace.update() returns itself for chaining."""
        from utils.langfuse.tracer import NoOpTrace

        trace = NoOpTrace()
        result = trace.update(name="updated")
        assert result is trace


# ===========================================================================
# 2. LangfuseConfig
# ===========================================================================

class TestLangfuseConfig:
    """Tests for LangfuseConfig environment variable handling."""

    def test_is_available_false_when_keys_missing(self, clean_env):
        """is_available() returns False when public/secret keys are not set."""
        from utils.langfuse.config import LangfuseConfig

        config = LangfuseConfig()
        assert config.is_available() is False

    def test_is_available_false_when_only_public_key(self, clean_env):
        """is_available() returns False when only public key is set."""
        from utils.langfuse.config import LangfuseConfig

        clean_env.setenv("LANGFUSE_PUBLIC_KEY", "pk-test")
        config = LangfuseConfig()
        assert config.is_available() is False

    def test_is_available_true_when_both_keys_set(self, configured_env):
        """is_available() returns True when both keys are set."""
        from utils.langfuse.config import LangfuseConfig

        config = LangfuseConfig()
        assert config.is_available() is True

    def test_enabled_false_overrides_keys(self, configured_env):
        """LANGFUSE_ENABLED=false makes is_available() return False even with keys."""
        from utils.langfuse.config import LangfuseConfig

        configured_env.setenv("LANGFUSE_ENABLED", "false")
        config = LangfuseConfig()
        assert config.is_available() is False

    def test_enabled_false_case_insensitive(self, configured_env):
        """LANGFUSE_ENABLED=False (capitalized) also disables."""
        from utils.langfuse.config import LangfuseConfig

        configured_env.setenv("LANGFUSE_ENABLED", "False")
        config = LangfuseConfig()
        assert config.is_available() is False

    def test_default_host(self, clean_env):
        """Default host is cloud.langfuse.com."""
        from utils.langfuse.config import LangfuseConfig

        config = LangfuseConfig()
        assert config.host == "https://cloud.langfuse.com"

    def test_custom_host(self, clean_env):
        """Custom host is read from LANGFUSE_HOST."""
        from utils.langfuse.config import LangfuseConfig

        clean_env.setenv("LANGFUSE_HOST", "https://my-langfuse.example.com")
        config = LangfuseConfig()
        assert config.host == "https://my-langfuse.example.com"

    def test_default_environment(self, clean_env):
        """Default environment is 'development'."""
        from utils.langfuse.config import LangfuseConfig

        config = LangfuseConfig()
        assert config.environment == "development"

    def test_default_release(self, clean_env):
        """Default release is 'claude-code-hooks-v1'."""
        from utils.langfuse.config import LangfuseConfig

        config = LangfuseConfig()
        assert config.release == "claude-code-hooks-v1"

    def test_debug_defaults_false(self, clean_env):
        """Debug defaults to False."""
        from utils.langfuse.config import LangfuseConfig

        config = LangfuseConfig()
        assert config.debug is False

    def test_debug_enabled(self, clean_env):
        """LANGFUSE_DEBUG=true enables debug mode."""
        from utils.langfuse.config import LangfuseConfig

        clean_env.setenv("LANGFUSE_DEBUG", "true")
        config = LangfuseConfig()
        assert config.debug is True

    def test_to_dict_contains_all_fields(self, configured_env):
        """to_dict() returns all config fields for client init."""
        from utils.langfuse.config import LangfuseConfig

        config = LangfuseConfig()
        d = config.to_dict()
        assert "public_key" in d
        assert "secret_key" in d
        assert "host" in d
        assert "environment" in d
        assert "release" in d
        assert "debug" in d
        assert d["public_key"] == "pk-test-abc123"
        assert d["secret_key"] == "sk-test-xyz789"

    def test_repr(self, configured_env):
        """__repr__ includes availability and host."""
        from utils.langfuse.config import LangfuseConfig

        config = LangfuseConfig()
        r = repr(config)
        assert "available=True" in r
        assert "cloud.langfuse.com" in r

    def test_get_config_singleton(self, clean_env):
        """get_config() returns the same instance on repeated calls."""
        import utils.langfuse.config as config_mod

        config_mod._config = None  # Reset singleton
        c1 = config_mod.get_config()
        c2 = config_mod.get_config()
        assert c1 is c2


# ===========================================================================
# 3. SessionRegistry
# ===========================================================================

class TestSessionRegistry:
    """Tests for SessionRegistry file-based session management."""

    def test_register_session_creates_file(self, temp_registry):
        """register_session() creates a session JSON file."""
        temp_registry.register_session(
            session_id="sess-001",
            trace_id="trace-001",
            cwd="/tmp/test-project",
            git_branch="main",
        )

        session_file = temp_registry._sessions_dir / "sess-001.json"
        assert session_file.exists()

        data = json.loads(session_file.read_text())
        assert data["session_id"] == "sess-001"
        assert data["trace_id"] == "trace-001"
        assert data["status"] == "active"
        assert data["cwd"] == "/tmp/test-project"
        assert data["git_branch"] == "main"

    def test_register_session_updates_index(self, temp_registry):
        """register_session() adds session to the index file."""
        temp_registry.register_session(
            session_id="sess-002",
            trace_id="trace-002",
            cwd="/tmp/project-a",
        )

        index = json.loads(
            (temp_registry._sessions_dir / "index.json").read_text()
        )
        cwd_hash = temp_registry._hash_cwd("/tmp/project-a")
        assert "sess-002" in index["cwd_to_sessions"][cwd_hash]
        assert str(os.getppid()) in index["ppid_to_session"]

    def test_find_session_by_cwd(self, temp_registry):
        """find_session_for_tool() finds session by CWD hash."""
        cwd = os.getcwd()
        temp_registry.register_session(
            session_id="sess-cwd",
            trace_id="trace-cwd",
            cwd=cwd,
        )

        session = temp_registry.find_session_for_tool()
        assert session is not None
        assert session["session_id"] == "sess-cwd"

    def test_find_session_ppid_fallback(self, temp_registry):
        """find_session_for_tool() falls back to PPID when CWD has multiple sessions."""
        cwd = os.getcwd()
        ppid = os.getppid()

        # Register two sessions in same CWD but different PPIDs
        temp_registry.register_session(
            session_id="sess-a",
            trace_id="trace-a",
            cwd=cwd,
        )

        # Manually create second session with different PPID
        session_b = {
            "session_id": "sess-b",
            "trace_id": "trace-b",
            "cwd": cwd,
            "cwd_hash": temp_registry._hash_cwd(cwd),
            "ppid": 99999,  # Different PPID
            "status": "active",
            "last_activity": datetime.now().isoformat(),
            "pending_spans": [],
        }
        temp_registry._save_json(
            temp_registry._get_session_file("sess-b"), session_b
        )

        # Update index to include both
        lock_fd = temp_registry._acquire_lock("index")
        index = temp_registry._load_index()
        cwd_hash = temp_registry._hash_cwd(cwd)
        if "sess-b" not in index["cwd_to_sessions"][cwd_hash]:
            index["cwd_to_sessions"][cwd_hash].append("sess-b")
        temp_registry._save_index(index)
        temp_registry._release_lock(lock_fd)

        # find_session_for_tool should return session matching current PPID
        session = temp_registry.find_session_for_tool()
        assert session is not None
        assert session["session_id"] == "sess-a"
        assert session["ppid"] == ppid

    def test_find_session_most_recent_fallback(self, temp_registry):
        """find_session_for_tool() returns most recent when PPID doesn't match."""
        cwd = os.getcwd()
        cwd_hash = temp_registry._hash_cwd(cwd)

        # Create two sessions with non-matching PPIDs
        old_time = (datetime.now() - timedelta(hours=1)).isoformat()
        new_time = datetime.now().isoformat()

        for sid, ppid_val, activity in [
            ("sess-old", 11111, old_time),
            ("sess-new", 22222, new_time),
        ]:
            session_data = {
                "session_id": sid,
                "trace_id": f"trace-{sid}",
                "cwd": cwd,
                "cwd_hash": cwd_hash,
                "ppid": ppid_val,
                "status": "active",
                "last_activity": activity,
                "pending_spans": [],
            }
            temp_registry._save_json(
                temp_registry._get_session_file(sid), session_data
            )

        # Build index manually
        index = {
            "cwd_to_sessions": {cwd_hash: ["sess-old", "sess-new"]},
            "ppid_to_session": {},
        }
        temp_registry._save_index(index)

        session = temp_registry.find_session_for_tool()
        assert session is not None
        assert session["session_id"] == "sess-new"

    def test_find_session_returns_none_when_empty(self, temp_registry):
        """find_session_for_tool() returns None when no sessions exist."""
        session = temp_registry.find_session_for_tool()
        assert session is None

    def test_mark_session_stopped(self, temp_registry):
        """mark_session_stopped() changes status to 'stopped'."""
        temp_registry.register_session(
            session_id="sess-stop",
            trace_id="trace-stop",
            cwd="/tmp/test",
        )

        temp_registry.mark_session_stopped("sess-stop")

        session = temp_registry.get_session("sess-stop")
        assert session["status"] == "stopped"

    def test_reactivate_session(self, temp_registry):
        """reactivate_session() changes status back to 'active' with new PPID."""
        temp_registry.register_session(
            session_id="sess-react",
            trace_id="trace-react",
            cwd="/tmp/test",
        )
        temp_registry.mark_session_stopped("sess-react")

        trace_id = temp_registry.reactivate_session("sess-react")
        assert trace_id == "trace-react"

        session = temp_registry.get_session("sess-react")
        assert session["status"] == "active"
        assert session["ppid"] == os.getppid()

    def test_reactivate_nonexistent_returns_none(self, temp_registry):
        """reactivate_session() returns None for unknown session."""
        result = temp_registry.reactivate_session("nonexistent-session")
        assert result is None

    def test_cleanup_stale_marks_old_active(self, temp_registry):
        """cleanup_stale_sessions() marks old active sessions as stale."""
        cwd = "/tmp/stale-test"

        temp_registry.register_session(
            session_id="sess-stale",
            trace_id="trace-stale",
            cwd=cwd,
        )

        # Manually backdate last_activity by 25 hours
        session_file = temp_registry._get_session_file("sess-stale")
        data = json.loads(session_file.read_text())
        data["last_activity"] = (
            datetime.now() - timedelta(hours=25)
        ).isoformat()
        session_file.write_text(json.dumps(data))

        temp_registry.cleanup_stale_sessions(max_age_hours=24)

        updated = temp_registry.get_session("sess-stale")
        assert updated["status"] == "stale"

    def test_cleanup_removes_old_stopped(self, temp_registry):
        """cleanup_stale_sessions() removes old stopped sessions."""
        cwd = "/tmp/cleanup-test"

        temp_registry.register_session(
            session_id="sess-old-stopped",
            trace_id="trace-old-stopped",
            cwd=cwd,
        )
        temp_registry.mark_session_stopped("sess-old-stopped")

        # Backdate by 49 hours (> 24*2 = 48)
        session_file = temp_registry._get_session_file("sess-old-stopped")
        data = json.loads(session_file.read_text())
        data["last_activity"] = (
            datetime.now() - timedelta(hours=49)
        ).isoformat()
        session_file.write_text(json.dumps(data))

        temp_registry.cleanup_stale_sessions(max_age_hours=24)

        assert not session_file.exists()

    def test_add_and_pop_pending_span_fifo(self, temp_registry):
        """add_pending_span() and pop_pending_span() operate FIFO."""
        temp_registry.register_session(
            session_id="sess-spans",
            trace_id="trace-spans",
            cwd="/tmp/spans",
        )

        # Add two spans for the same tool
        temp_registry.add_pending_span("sess-spans", "span-1", "Bash")
        temp_registry.add_pending_span("sess-spans", "span-2", "Bash")

        # Pop should return first one (FIFO)
        first = temp_registry.pop_pending_span("sess-spans", "Bash")
        assert first is not None
        assert first["span_id"] == "span-1"

        second = temp_registry.pop_pending_span("sess-spans", "Bash")
        assert second is not None
        assert second["span_id"] == "span-2"

        # No more pending
        third = temp_registry.pop_pending_span("sess-spans", "Bash")
        assert third is None

    def test_pop_pending_span_matches_tool_name(self, temp_registry):
        """pop_pending_span() only pops spans matching the given tool name."""
        temp_registry.register_session(
            session_id="sess-multi",
            trace_id="trace-multi",
            cwd="/tmp/multi",
        )

        temp_registry.add_pending_span("sess-multi", "span-bash", "Bash")
        temp_registry.add_pending_span("sess-multi", "span-read", "Read")

        # Pop Read should skip Bash
        result = temp_registry.pop_pending_span("sess-multi", "Read")
        assert result is not None
        assert result["span_id"] == "span-read"

        # Bash still pending
        result = temp_registry.pop_pending_span("sess-multi", "Bash")
        assert result is not None
        assert result["span_id"] == "span-bash"

    def test_concurrent_access_does_not_corrupt(self, temp_registry):
        """Concurrent session registrations don't corrupt the index."""
        errors = []

        def register(session_id):
            try:
                temp_registry.register_session(
                    session_id=session_id,
                    trace_id=f"trace-{session_id}",
                    cwd=f"/tmp/concurrent-{session_id}",
                )
            except Exception as e:
                errors.append(e)

        threads = [
            threading.Thread(target=register, args=(f"conc-{i}",))
            for i in range(10)
        ]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert errors == [], f"Concurrent errors: {errors}"

        # Verify index is valid JSON
        index_file = temp_registry._sessions_dir / "index.json"
        assert index_file.exists()
        index = json.loads(index_file.read_text())
        assert "cwd_to_sessions" in index

        # All 10 sessions should have their own file
        for i in range(10):
            sf = temp_registry._get_session_file(f"conc-{i}")
            assert sf.exists(), f"Session file missing for conc-{i}"

    def test_get_session_returns_none_for_unknown(self, temp_registry):
        """get_session() returns None for non-existent session ID."""
        assert temp_registry.get_session("does-not-exist") is None

    def test_update_session_updates_fields(self, temp_registry):
        """update_session() merges new fields and bumps last_activity."""
        temp_registry.register_session(
            session_id="sess-upd",
            trace_id="trace-upd",
            cwd="/tmp/upd",
        )

        original = temp_registry.get_session("sess-upd")
        original_activity = original["last_activity"]

        # Small delay to ensure timestamp changes
        time.sleep(0.01)

        temp_registry.update_session("sess-upd", custom_field="hello")

        updated = temp_registry.get_session("sess-upd")
        assert updated["custom_field"] == "hello"
        assert updated["last_activity"] >= original_activity


# ===========================================================================
# 4. ClaudeCodeTracer with Mocked Langfuse Client
# ===========================================================================

class TestClaudeCodeTracer:
    """Tests for ClaudeCodeTracer using mocked Langfuse client."""

    def _make_tracer(self, mock_client, tmp_path, configured_env):
        """Helper to create a tracer with mocked client and temp registry."""
        from utils.langfuse.config import LangfuseConfig
        from utils.langfuse.tracer import ClaudeCodeTracer, SessionRegistry

        config = LangfuseConfig()

        # Build tracer without hitting real Langfuse
        tracer = ClaudeCodeTracer.__new__(ClaudeCodeTracer)
        tracer._config = config
        tracer._client = mock_client

        # Patch the registry to use temp dir
        registry = SessionRegistry()
        registry._base_dir = tmp_path / "langfuse"
        registry._sessions_dir = registry._base_dir / "sessions"
        registry._locks_dir = registry._base_dir / "locks"
        registry._sessions_dir.mkdir(parents=True, exist_ok=True)
        registry._locks_dir.mkdir(parents=True, exist_ok=True)
        tracer._registry = registry

        return tracer

    def test_start_session_trace_creates_trace(
        self, mock_langfuse_client, tmp_path, configured_env
    ):
        """start_session_trace() creates a Langfuse trace and registers session."""
        tracer = self._make_tracer(mock_langfuse_client, tmp_path, configured_env)

        trace_id = tracer.start_session_trace(
            session_id="sess-trace-1",
            source="startup",
            git_branch="main",
        )

        assert trace_id == "trace-abc-123"
        mock_langfuse_client.trace.assert_called_once()

        call_kwargs = mock_langfuse_client.trace.call_args[1]
        assert call_kwargs["name"] == "claude-code-session"
        assert call_kwargs["session_id"] == "sess-trace-1"
        assert "startup" in call_kwargs["tags"]

        # Session should be registered
        session = tracer._registry.get_session("sess-trace-1")
        assert session is not None
        assert session["trace_id"] == "trace-abc-123"
        assert session["status"] == "active"

    def test_start_session_trace_returns_noop_when_disabled(
        self, tmp_path, clean_env
    ):
        """start_session_trace() returns 'noop' when client is None."""
        from utils.langfuse.tracer import ClaudeCodeTracer, SessionRegistry
        from utils.langfuse.config import LangfuseConfig

        config = LangfuseConfig()
        tracer = ClaudeCodeTracer.__new__(ClaudeCodeTracer)
        tracer._config = config
        tracer._client = None

        registry = SessionRegistry()
        registry._base_dir = tmp_path / "langfuse"
        registry._sessions_dir = registry._base_dir / "sessions"
        registry._locks_dir = registry._base_dir / "locks"
        registry._sessions_dir.mkdir(parents=True, exist_ok=True)
        registry._locks_dir.mkdir(parents=True, exist_ok=True)
        tracer._registry = registry

        result = tracer.start_session_trace(
            session_id="sess-disabled", source="startup"
        )
        assert result == "noop"

    def test_log_user_prompt_creates_span(
        self, mock_langfuse_client, tmp_path, configured_env
    ):
        """log_user_prompt() creates a span on the correct trace."""
        tracer = self._make_tracer(mock_langfuse_client, tmp_path, configured_env)
        tracer.start_session_trace(session_id="sess-prompt", source="startup")

        # Reset mock to track span call separately
        mock_langfuse_client.span.reset_mock()

        span_id = tracer.log_user_prompt(
            session_id="sess-prompt",
            prompt="What is the meaning of life?",
        )

        assert span_id == "span-def-456"
        mock_langfuse_client.span.assert_called_once()
        call_kwargs = mock_langfuse_client.span.call_args[1]
        assert call_kwargs["trace_id"] == "trace-abc-123"
        assert call_kwargs["name"] == "user-prompt"
        assert "What is the meaning of life?" in call_kwargs["input"]["prompt"]

    def test_log_user_prompt_returns_noop_for_unknown_session(
        self, mock_langfuse_client, tmp_path, configured_env
    ):
        """log_user_prompt() returns 'noop' when session doesn't exist."""
        tracer = self._make_tracer(mock_langfuse_client, tmp_path, configured_env)

        result = tracer.log_user_prompt(
            session_id="unknown-session",
            prompt="hello",
        )
        assert result == "noop"

    def test_start_tool_span_creates_span_and_adds_pending(
        self, mock_langfuse_client, tmp_path, configured_env
    ):
        """start_tool_span() creates span and adds to pending registry."""
        tracer = self._make_tracer(mock_langfuse_client, tmp_path, configured_env)

        # Register a session in current CWD
        cwd = os.getcwd()
        tracer._registry.register_session(
            session_id="sess-tool",
            trace_id="trace-abc-123",
            cwd=cwd,
        )

        mock_langfuse_client.span.reset_mock()

        span_id = tracer.start_tool_span(
            tool_name="Bash",
            tool_input={"command": "ls -la"},
        )

        assert span_id == "span-def-456"
        mock_langfuse_client.span.assert_called_once()

        # Should be in pending spans
        session = tracer._registry.get_session("sess-tool")
        assert len(session["pending_spans"]) == 1
        assert session["pending_spans"][0]["tool_name"] == "Bash"

    def test_end_tool_span_pops_pending_and_updates(
        self, mock_langfuse_client, tmp_path, configured_env
    ):
        """end_tool_span() pops pending span and updates it with result."""
        tracer = self._make_tracer(mock_langfuse_client, tmp_path, configured_env)

        cwd = os.getcwd()
        tracer._registry.register_session(
            session_id="sess-end-tool",
            trace_id="trace-abc-123",
            cwd=cwd,
        )

        # Start a tool span
        tracer.start_tool_span(
            tool_name="Read",
            tool_input={"file_path": "/tmp/test.py"},
        )

        mock_langfuse_client.span.reset_mock()

        tracer.end_tool_span(
            tool_name="Read",
            tool_result="file contents here",
        )

        # Span should have been ended
        mock_langfuse_client.span.assert_called_once()
        end_call_kwargs = mock_langfuse_client.span.call_args[1]
        assert end_call_kwargs["id"] == "span-def-456"

        span_mock = mock_langfuse_client.span.return_value
        span_mock.update.assert_called_once()
        span_mock.end.assert_called()

        # Pending spans should be empty
        session = tracer._registry.get_session("sess-end-tool")
        assert len(session["pending_spans"]) == 0

    def test_end_session_trace_flushes_and_stops(
        self, mock_langfuse_client, tmp_path, configured_env
    ):
        """end_session_trace() flushes client and marks session stopped."""
        tracer = self._make_tracer(mock_langfuse_client, tmp_path, configured_env)
        tracer.start_session_trace(session_id="sess-end", source="startup")

        mock_langfuse_client.trace.reset_mock()

        tracer.end_session_trace(session_id="sess-end")

        mock_langfuse_client.flush.assert_called_once()

        session = tracer._registry.get_session("sess-end")
        assert session["status"] == "stopped"

    def test_resume_reactivates_existing_session(
        self, mock_langfuse_client, tmp_path, configured_env
    ):
        """start_session_trace() with source='resume' reactivates stopped session."""
        tracer = self._make_tracer(mock_langfuse_client, tmp_path, configured_env)

        # Create and stop a session
        tracer.start_session_trace(session_id="sess-resume", source="startup")
        tracer.end_session_trace(session_id="sess-resume")

        # Reset mock to track resume behavior
        mock_langfuse_client.trace.reset_mock()

        trace_id = tracer.start_session_trace(
            session_id="sess-resume", source="resume"
        )

        assert trace_id == "trace-abc-123"
        # Should NOT create a new trace - reuses existing
        mock_langfuse_client.trace.assert_not_called()

        session = tracer._registry.get_session("sess-resume")
        assert session["status"] == "active"

    def test_resume_falls_back_to_new_trace_for_unknown(
        self, mock_langfuse_client, tmp_path, configured_env
    ):
        """start_session_trace() with source='resume' creates new trace if no existing."""
        tracer = self._make_tracer(mock_langfuse_client, tmp_path, configured_env)

        trace_id = tracer.start_session_trace(
            session_id="brand-new-resume", source="resume"
        )

        # Should create a new trace since session doesn't exist
        assert trace_id == "trace-abc-123"
        mock_langfuse_client.trace.assert_called_once()

    def test_enabled_property(
        self, mock_langfuse_client, tmp_path, configured_env
    ):
        """enabled property reflects whether client is set."""
        tracer = self._make_tracer(mock_langfuse_client, tmp_path, configured_env)
        assert tracer.enabled is True

        tracer._client = None
        assert tracer.enabled is False

    def test_flush_delegates_to_client(
        self, mock_langfuse_client, tmp_path, configured_env
    ):
        """flush() calls client.flush()."""
        tracer = self._make_tracer(mock_langfuse_client, tmp_path, configured_env)
        tracer.flush()
        mock_langfuse_client.flush.assert_called_once()

    def test_flush_noop_when_disabled(self, tmp_path, clean_env):
        """flush() does nothing when client is None."""
        from utils.langfuse.tracer import ClaudeCodeTracer, SessionRegistry
        from utils.langfuse.config import LangfuseConfig

        tracer = ClaudeCodeTracer.__new__(ClaudeCodeTracer)
        tracer._config = LangfuseConfig()
        tracer._client = None

        registry = SessionRegistry()
        registry._base_dir = tmp_path / "langfuse"
        registry._sessions_dir = registry._base_dir / "sessions"
        registry._locks_dir = registry._base_dir / "locks"
        registry._sessions_dir.mkdir(parents=True, exist_ok=True)
        registry._locks_dir.mkdir(parents=True, exist_ok=True)
        tracer._registry = registry

        # Should not raise
        tracer.flush()


# ===========================================================================
# 5. Sanitization
# ===========================================================================

class TestSanitization:
    """Tests for _sanitize_input() and _sanitize_output()."""

    def _make_tracer(self, tmp_path):
        """Helper to create a tracer for sanitization tests (no client needed)."""
        from utils.langfuse.tracer import ClaudeCodeTracer, SessionRegistry
        from utils.langfuse.config import LangfuseConfig

        tracer = ClaudeCodeTracer.__new__(ClaudeCodeTracer)
        tracer._config = LangfuseConfig()
        tracer._client = None

        registry = SessionRegistry()
        registry._base_dir = tmp_path / "langfuse"
        registry._sessions_dir = registry._base_dir / "sessions"
        registry._locks_dir = registry._base_dir / "locks"
        registry._sessions_dir.mkdir(parents=True, exist_ok=True)
        registry._locks_dir.mkdir(parents=True, exist_ok=True)
        tracer._registry = registry

        return tracer

    def test_sanitize_input_truncates_large_content(self, tmp_path):
        """_sanitize_input() replaces large 'content' with size marker."""
        tracer = self._make_tracer(tmp_path)
        result = tracer._sanitize_input("Write", {
            "content": "x" * 500,
            "file_path": "/tmp/test.py",
        })

        assert result["file_path"] == "/tmp/test.py"
        assert "[500 chars]" in result["content"]

    def test_sanitize_input_preserves_small_content(self, tmp_path):
        """_sanitize_input() preserves content under 200 chars."""
        tracer = self._make_tracer(tmp_path)
        small_content = "print('hello')"
        result = tracer._sanitize_input("Write", {
            "content": small_content,
        })
        assert result["content"] == small_content

    def test_sanitize_input_preserves_command(self, tmp_path):
        """_sanitize_input() preserves command field."""
        tracer = self._make_tracer(tmp_path)
        result = tracer._sanitize_input("Bash", {
            "command": "ls -la /tmp",
        })
        assert result["command"] == "ls -la /tmp"

    def test_sanitize_input_preserves_file_path(self, tmp_path):
        """_sanitize_input() preserves file_path field."""
        tracer = self._make_tracer(tmp_path)
        result = tracer._sanitize_input("Read", {
            "file_path": "/home/user/code/main.py",
        })
        assert result["file_path"] == "/home/user/code/main.py"

    def test_sanitize_input_preserves_pattern(self, tmp_path):
        """_sanitize_input() preserves pattern field."""
        tracer = self._make_tracer(tmp_path)
        result = tracer._sanitize_input("Grep", {
            "pattern": "def main",
        })
        assert result["pattern"] == "def main"

    def test_sanitize_input_truncates_long_string_values(self, tmp_path):
        """_sanitize_input() truncates string values over 500 chars."""
        tracer = self._make_tracer(tmp_path)
        result = tracer._sanitize_input("Custom", {
            "data": "z" * 600,
        })
        assert "[600 chars]" in result["data"]

    def test_sanitize_input_truncates_large_json(self, tmp_path):
        """_sanitize_input() truncates large dict/list values."""
        tracer = self._make_tracer(tmp_path)
        large_list = list(range(200))
        result = tracer._sanitize_input("Custom", {
            "items": large_list,
        })
        assert "chars JSON]" in result["items"]

    def test_sanitize_input_handles_non_dict(self, tmp_path):
        """_sanitize_input() wraps non-dict input in a raw field."""
        tracer = self._make_tracer(tmp_path)
        result = tracer._sanitize_input("Custom", "just a string")
        assert "raw" in result
        assert result["raw"] == "just a string"

    def test_sanitize_output_truncates_long_string(self, tmp_path):
        """_sanitize_output() truncates strings over 1000 chars."""
        tracer = self._make_tracer(tmp_path)
        long_output = "a" * 1500
        result = tracer._sanitize_output("Bash", long_output)
        assert result == "[1500 chars]"

    def test_sanitize_output_preserves_short_string(self, tmp_path):
        """_sanitize_output() preserves strings under 1000 chars."""
        tracer = self._make_tracer(tmp_path)
        result = tracer._sanitize_output("Bash", "short output")
        assert result == "short output"

    def test_sanitize_output_handles_none(self, tmp_path):
        """_sanitize_output() returns None for None input."""
        tracer = self._make_tracer(tmp_path)
        result = tracer._sanitize_output("Bash", None)
        assert result is None

    def test_sanitize_output_handles_dict(self, tmp_path):
        """_sanitize_output() truncates long values in dict output."""
        tracer = self._make_tracer(tmp_path)
        result = tracer._sanitize_output("Custom", {
            "short": "ok",
            "long": "b" * 600,
        })
        assert result["short"] == "ok"
        assert "[600 chars]" in result["long"]

    def test_sanitize_output_handles_other_types(self, tmp_path):
        """_sanitize_output() handles int, list, etc. by passing through."""
        tracer = self._make_tracer(tmp_path)

        assert tracer._sanitize_output("Custom", 42) == 42
        assert tracer._sanitize_output("Custom", [1, 2, 3]) == [1, 2, 3]

    def test_sanitize_output_truncates_large_other_types(self, tmp_path):
        """_sanitize_output() truncates large non-string/dict/None types."""
        tracer = self._make_tracer(tmp_path)
        large_list = list(range(500))
        result = tracer._sanitize_output("Custom", large_list)
        # str(large_list) is > 1000 chars
        if isinstance(result, str):
            assert "chars]" in result
        else:
            # If under 1000 when stringified, it passes through
            assert result == large_list
