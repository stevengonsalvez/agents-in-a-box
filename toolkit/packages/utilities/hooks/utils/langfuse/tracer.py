"""
Langfuse tracer with NoOp fallback for Claude Code hooks.

This module provides a tracer that:
- Works seamlessly when Langfuse is configured
- Has zero overhead when Langfuse is not configured (NoOp pattern)
- Supports multiple parallel Claude Code sessions (via SessionRegistry)
- Uses CWD-based correlation with PPID fallback for tool hooks
- Never crashes - all errors are caught and logged
"""

import os
import sys
import json
import hashlib
import logging
from pathlib import Path

# Platform-specific file locking
# fcntl is Unix-only; on Windows we fall back to no-op locking
HAS_FCNTL = False
if sys.platform != 'win32':
    try:
        import fcntl
        HAS_FCNTL = True
    except ImportError:
        pass
from datetime import datetime, timedelta
from typing import Optional, Dict, Any, Tuple, List

from .config import get_config, LangfuseConfig

# Set up logging
logger = logging.getLogger(__name__)

# Try to import Langfuse SDK
HAS_LANGFUSE = False

config = get_config()
if config.is_available():
    try:
        from langfuse import Langfuse
        HAS_LANGFUSE = True
    except ImportError:
        logger.debug("Langfuse SDK not installed")
    except Exception as e:
        logger.debug(f"Failed to import Langfuse: {e}")


class NoOpSpan:
    """No-op span that accepts any method call and returns itself."""

    def __init__(self, *args, **kwargs):
        self.id = "noop"

    def update(self, **kwargs) -> 'NoOpSpan':
        return self

    def end(self, **kwargs) -> None:
        pass

    def __enter__(self) -> 'NoOpSpan':
        return self

    def __exit__(self, *args) -> None:
        pass


class NoOpTrace:
    """No-op trace that accepts any method call."""

    def __init__(self, *args, **kwargs):
        self.id = "noop"

    def update(self, **kwargs) -> 'NoOpTrace':
        return self

    def span(self, **kwargs) -> NoOpSpan:
        return NoOpSpan()

    def generation(self, **kwargs) -> NoOpSpan:
        return NoOpSpan()


class SessionRegistry:
    """
    Registry for managing multiple Claude Code sessions.

    Supports 5+ parallel sessions by using:
    - Per-session state files: ~/.claude/langfuse/sessions/{session_id}.json
    - CWD-based lookup (primary): Most sessions run in unique directories (worktrees)
    - PPID-based fallback: For rare cases of multiple sessions in same directory
    - Index file for fast O(1) lookups

    Thread-safe via file locking per session.
    """

    def __init__(self):
        self._base_dir = Path.home() / ".claude" / "langfuse"
        self._sessions_dir = self._base_dir / "sessions"
        self._locks_dir = self._base_dir / "locks"

        # Ensure directories exist
        for d in [self._sessions_dir, self._locks_dir]:
            d.mkdir(parents=True, exist_ok=True)

    def _hash_cwd(self, cwd: Optional[str] = None) -> str:
        """Generate a short hash of the current working directory."""
        path = cwd or os.getcwd()
        return hashlib.md5(path.encode()).hexdigest()[:8]

    def _get_session_file(self, session_id: str) -> Path:
        """Get path to session state file."""
        return self._sessions_dir / f"{session_id}.json"

    def _get_index_file(self) -> Path:
        """Get path to index file for fast lookups."""
        return self._sessions_dir / "index.json"

    def _get_lock_file(self, name: str) -> Path:
        """Get lock file path for atomic operations."""
        return self._locks_dir / f"{name}.lock"

    def _acquire_lock(self, name: str) -> int:
        """
        Acquire exclusive lock, returns file descriptor.

        On Windows or if fcntl is not available, returns -1 (no-op).
        """
        if not HAS_FCNTL:
            return -1

        lock_file = self._get_lock_file(name)
        fd = os.open(str(lock_file), os.O_RDWR | os.O_CREAT)
        fcntl.flock(fd, fcntl.LOCK_EX)
        return fd

    def _release_lock(self, fd: int) -> None:
        """Release lock and close file descriptor."""
        if fd < 0:
            return  # No-op lock (Windows or fcntl unavailable)

        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
            os.close(fd)
        except Exception as e:
            logger.debug(f"Failed to release lock or close fd {fd}: {e}")

    def _load_json(self, path: Path) -> Dict[str, Any]:
        """Load JSON file, return empty dict on failure."""
        if path.exists():
            try:
                with open(path, 'r') as f:
                    return json.load(f)
            except (json.JSONDecodeError, IOError):
                pass
        return {}

    def _save_json(self, path: Path, data: Dict[str, Any]) -> None:
        """Save JSON file."""
        try:
            with open(path, 'w') as f:
                json.dump(data, f, indent=2)
        except IOError as e:
            logger.debug(f"Failed to save {path}: {e}")

    def _load_index(self) -> Dict[str, Any]:
        """Load the session index."""
        return self._load_json(self._get_index_file())

    def _save_index(self, index: Dict[str, Any]) -> None:
        """Save the session index."""
        self._save_json(self._get_index_file(), index)

    def register_session(
        self,
        session_id: str,
        trace_id: str,
        cwd: Optional[str] = None,
        git_branch: Optional[str] = None,
        **metadata
    ) -> None:
        """
        Register a new session or reactivate an existing one.

        Called by session_start hook. Records session with PPID for
        later correlation by tool hooks.
        """
        cwd = cwd or os.getcwd()
        cwd_hash = self._hash_cwd(cwd)
        ppid = os.getppid()
        now = datetime.now().isoformat()

        session_data = {
            "session_id": session_id,
            "trace_id": trace_id,
            "created_at": now,
            "last_activity": now,
            "cwd": cwd,
            "cwd_hash": cwd_hash,
            "ppid": ppid,
            "git_branch": git_branch,
            "status": "active",
            "pending_spans": [],
            **metadata
        }

        # Save session file with lock
        lock_fd = None
        try:
            lock_fd = self._acquire_lock(session_id)
            self._save_json(self._get_session_file(session_id), session_data)
        finally:
            if lock_fd is not None:
                self._release_lock(lock_fd)

        # Update index (with separate lock)
        lock_fd = None
        try:
            lock_fd = self._acquire_lock("index")
            index = self._load_index()

            # Update cwd_to_sessions mapping
            if "cwd_to_sessions" not in index:
                index["cwd_to_sessions"] = {}
            if cwd_hash not in index["cwd_to_sessions"]:
                index["cwd_to_sessions"][cwd_hash] = []
            if session_id not in index["cwd_to_sessions"][cwd_hash]:
                index["cwd_to_sessions"][cwd_hash].append(session_id)

            # Update ppid_to_session mapping
            if "ppid_to_session" not in index:
                index["ppid_to_session"] = {}
            index["ppid_to_session"][str(ppid)] = session_id

            self._save_index(index)
        finally:
            if lock_fd is not None:
                self._release_lock(lock_fd)

        logger.debug(f"Registered session {session_id} (cwd_hash={cwd_hash}, ppid={ppid})")

    def find_session_for_tool(self) -> Optional[Dict[str, Any]]:
        """
        Find the session for the current tool hook invocation.

        Uses CWD-primary, PPID-fallback correlation:
        1. Get all active sessions in current CWD (typically 1 with worktrees)
        2. If single session, return it (common case)
        3. If multiple sessions in same CWD, use PPID to discriminate
        4. Fallback to most recently active session

        Returns session data dict or None if no matching session found.
        """
        cwd_hash = self._hash_cwd()
        ppid = os.getppid()

        try:
            # Load index for fast lookup
            index = self._load_index()

            # Get session IDs for this CWD
            session_ids = index.get("cwd_to_sessions", {}).get(cwd_hash, [])

            if not session_ids:
                # No sessions in this CWD - try PPID lookup as fallback
                ppid_session = index.get("ppid_to_session", {}).get(str(ppid))
                if ppid_session:
                    session = self._load_session(ppid_session)
                    if session and session.get("status") == "active":
                        return session
                logger.debug(f"No session found for cwd_hash={cwd_hash}")
                return None

            # Load active sessions in this CWD
            active_sessions = []
            for sid in session_ids:
                session = self._load_session(sid)
                if session and session.get("status") == "active":
                    active_sessions.append(session)

            if not active_sessions:
                logger.debug(f"No active sessions in cwd_hash={cwd_hash}")
                return None

            # Single session - return it (typical case with worktrees)
            if len(active_sessions) == 1:
                return active_sessions[0]

            # Multiple sessions in same CWD - use PPID to discriminate
            for session in active_sessions:
                if session.get("ppid") == ppid:
                    return session

            # PPID didn't match - return most recently active
            return max(active_sessions, key=lambda s: s.get("last_activity", ""))

        except Exception as e:
            logger.debug(f"Error finding session: {e}")
            return None

    def _load_session(self, session_id: str) -> Optional[Dict[str, Any]]:
        """Load a session by ID."""
        session_file = self._get_session_file(session_id)
        if session_file.exists():
            return self._load_json(session_file)
        return None

    def get_session(self, session_id: str) -> Optional[Dict[str, Any]]:
        """Get session by ID (public interface)."""
        return self._load_session(session_id)

    def update_session(self, session_id: str, **updates) -> None:
        """Update session fields atomically."""
        lock_fd = None
        try:
            lock_fd = self._acquire_lock(session_id)
            session = self._load_session(session_id)
            if session:
                session.update(updates)
                session["last_activity"] = datetime.now().isoformat()
                self._save_json(self._get_session_file(session_id), session)
        finally:
            if lock_fd is not None:
                self._release_lock(lock_fd)

    def update_session_activity(self, session_id: str) -> None:
        """Update last_activity timestamp."""
        self.update_session(session_id)

    def add_pending_span(
        self,
        session_id: str,
        span_id: str,
        tool_name: str
    ) -> None:
        """Add a pending span to session (called by start_tool_span)."""
        lock_fd = None
        try:
            lock_fd = self._acquire_lock(session_id)
            session = self._load_session(session_id)
            if session:
                pending = session.get("pending_spans", [])
                pending.append({
                    "span_id": span_id,
                    "tool_name": tool_name,
                    "started_at": datetime.now().isoformat()
                })
                session["pending_spans"] = pending
                session["last_activity"] = datetime.now().isoformat()
                self._save_json(self._get_session_file(session_id), session)
        finally:
            if lock_fd is not None:
                self._release_lock(lock_fd)

    def pop_pending_span(
        self,
        session_id: str,
        tool_name: str
    ) -> Optional[Dict[str, Any]]:
        """
        Pop oldest pending span matching tool_name (FIFO).

        Called by end_tool_span to find the matching start span.
        """
        lock_fd = None
        try:
            lock_fd = self._acquire_lock(session_id)
            session = self._load_session(session_id)
            if not session:
                return None

            pending = session.get("pending_spans", [])

            # Find oldest matching span (FIFO)
            for i, span_info in enumerate(pending):
                if span_info.get("tool_name") == tool_name:
                    # Remove and return
                    span = pending.pop(i)
                    session["pending_spans"] = pending
                    session["last_activity"] = datetime.now().isoformat()
                    self._save_json(self._get_session_file(session_id), session)
                    return span

            return None
        finally:
            if lock_fd is not None:
                self._release_lock(lock_fd)

    def mark_session_stopped(self, session_id: str) -> None:
        """Mark session as stopped (called by stop hook)."""
        self.update_session(session_id, status="stopped")
        logger.debug(f"Marked session {session_id} as stopped")

    def reactivate_session(self, session_id: str) -> Optional[str]:
        """
        Reactivate a stopped session with new PPID.

        Returns trace_id if successful, None otherwise.
        """
        ppid = os.getppid()
        cwd = os.getcwd()
        cwd_hash = self._hash_cwd(cwd)

        lock_fd = None
        try:
            lock_fd = self._acquire_lock(session_id)
            session = self._load_session(session_id)

            if not session:
                return None

            # Reactivate with new PPID and potentially new CWD
            session["status"] = "active"
            session["ppid"] = ppid
            session["cwd"] = cwd
            session["cwd_hash"] = cwd_hash
            session["last_activity"] = datetime.now().isoformat()

            self._save_json(self._get_session_file(session_id), session)

            # Update index
            try:
                idx_fd = self._acquire_lock("index")
                index = self._load_index()

                # Update PPID mapping
                if "ppid_to_session" not in index:
                    index["ppid_to_session"] = {}
                index["ppid_to_session"][str(ppid)] = session_id

                # Update CWD mapping if changed
                if "cwd_to_sessions" not in index:
                    index["cwd_to_sessions"] = {}
                if cwd_hash not in index["cwd_to_sessions"]:
                    index["cwd_to_sessions"][cwd_hash] = []
                if session_id not in index["cwd_to_sessions"][cwd_hash]:
                    index["cwd_to_sessions"][cwd_hash].append(session_id)

                self._save_index(index)
                self._release_lock(idx_fd)
            except Exception as e:
                logger.debug(f"Failed to update index on reactivate: {e}")

            logger.debug(f"Reactivated session {session_id} with ppid={ppid}")
            return session.get("trace_id")

        finally:
            if lock_fd is not None:
                self._release_lock(lock_fd)

    def cleanup_stale_sessions(self, max_age_hours: int = 24) -> None:
        """
        Clean up sessions that have been inactive too long.

        Called periodically on session_start to prevent accumulation.
        - Active sessions inactive for >24h → marked stale
        - Stopped/stale sessions inactive for >48h → deleted
        """
        now = datetime.now()

        try:
            for session_file in self._sessions_dir.glob("*.json"):
                if session_file.name == "index.json":
                    continue

                try:
                    session = self._load_json(session_file)
                    if not session:
                        continue

                    last_activity_str = session.get("last_activity", "")
                    if not last_activity_str:
                        continue

                    last_activity = datetime.fromisoformat(last_activity_str)
                    age_hours = (now - last_activity).total_seconds() / 3600

                    status = session.get("status", "active")
                    session_id = session.get("session_id", session_file.stem)

                    if status == "active" and age_hours > max_age_hours:
                        # Mark as stale
                        self.update_session(session_id, status="stale")
                        logger.debug(f"Marked session {session_id} as stale (inactive {age_hours:.1f}h)")

                    elif status in ("stopped", "stale") and age_hours > max_age_hours * 2:
                        # Delete old session
                        self._remove_session(session_id)
                        logger.debug(f"Removed old session {session_id}")

                except Exception as e:
                    logger.debug(f"Error processing {session_file}: {e}")

        except Exception as e:
            logger.debug(f"Error during cleanup: {e}")

    def _remove_session(self, session_id: str) -> None:
        """Remove a session completely."""
        # Remove session file
        session_file = self._get_session_file(session_id)
        try:
            if session_file.exists():
                session_file.unlink()
        except Exception:
            pass

        # Update index
        lock_fd = None
        try:
            lock_fd = self._acquire_lock("index")
            index = self._load_index()

            # Remove from cwd_to_sessions
            for cwd_hash, sessions in list(index.get("cwd_to_sessions", {}).items()):
                if session_id in sessions:
                    sessions.remove(session_id)
                    if not sessions:
                        del index["cwd_to_sessions"][cwd_hash]

            # Remove from ppid_to_session
            ppid_map = index.get("ppid_to_session", {})
            for ppid, sid in list(ppid_map.items()):
                if sid == session_id:
                    del ppid_map[ppid]

            self._save_index(index)
        finally:
            if lock_fd is not None:
                self._release_lock(lock_fd)

        # Remove lock file
        lock_file = self._get_lock_file(session_id)
        try:
            if lock_file.exists():
                lock_file.unlink()
        except Exception:
            pass


class ClaudeCodeTracer:
    """
    Tracer for Claude Code sessions with automatic NoOp fallback.

    Uses SessionRegistry for multi-session support:
    - Each parallel session tracked separately
    - Tool hooks discover session via CWD (primary) or PPID (fallback)
    - Session stop/resume handled correctly
    """

    def __init__(self, config: Optional[LangfuseConfig] = None):
        self._config = config or get_config()
        self._client = None
        self._registry = SessionRegistry()

        # Initialize Langfuse client if available and configured
        if HAS_LANGFUSE and self._config.is_available():
            try:
                self._client = Langfuse(
                    public_key=self._config.public_key,
                    secret_key=self._config.secret_key,
                    host=self._config.host,
                    release=self._config.release,
                    debug=self._config.debug,
                )
                logger.debug("Langfuse client initialized")
            except Exception as e:
                logger.warning(f"Failed to initialize Langfuse client: {e}")
                self._client = None

    @property
    def enabled(self) -> bool:
        """Check if Langfuse tracing is enabled and available."""
        return self._client is not None

    def start_session_trace(
        self,
        session_id: str,
        source: str,
        **metadata
    ) -> str:
        """
        Start a new trace for a Claude Code session.

        Args:
            session_id: Unique session identifier
            source: Session source ("startup", "resume", or "clear")
            **metadata: Additional metadata (git_branch, etc.)

        Returns:
            trace_id for correlation, or "noop" if not enabled
        """
        if not self.enabled:
            return "noop"

        try:
            # Check if resuming existing session (resume, compact, or any non-startup source)
            # "startup" = fresh new session, "clear" = user cleared context
            # "resume" = explicit resume, "compact" = context compaction resume
            if source not in ("startup", "clear"):
                existing = self._registry.get_session(session_id)
                if existing and existing.get("trace_id"):
                    # Reactivate with new PPID
                    trace_id = self._registry.reactivate_session(session_id)
                    if trace_id:
                        logger.debug(f"Resumed session {session_id} (source={source}), trace_id={trace_id}")
                        return trace_id

            # Create new trace
            trace = self._client.trace(
                name="claude-code-session",
                session_id=session_id,
                user_id=os.getenv('ENGINEER_NAME', os.getenv('USER', 'unknown')),
                metadata={
                    "source": source,
                    "cwd": os.getcwd(),
                    "project": os.path.basename(os.getcwd()),
                    "started_at": datetime.now().isoformat(),
                    **metadata
                },
                tags=["claude-code", source]
            )

            # Register session with registry
            self._registry.register_session(
                session_id=session_id,
                trace_id=trace.id,
                git_branch=metadata.get("git_branch")
            )

            # Periodically cleanup stale sessions
            self._registry.cleanup_stale_sessions()

            logger.debug(f"Started trace: {trace.id}")
            return trace.id

        except Exception as e:
            logger.warning(f"Failed to start session trace: {e}")
            return "noop"

    def log_user_prompt(
        self,
        session_id: str,
        prompt: str,
        **metadata
    ) -> str:
        """
        Log a user prompt as a span on the current trace.

        Args:
            session_id: Session identifier
            prompt: User prompt text
            **metadata: Additional metadata

        Returns:
            span_id or "noop" if not enabled
        """
        if not self.enabled:
            return "noop"

        try:
            # Get session by ID (user_prompt_submit has session_id)
            session = self._registry.get_session(session_id)
            if not session:
                return "noop"

            trace_id = session.get("trace_id")
            if not trace_id or trace_id == "noop":
                return "noop"

            # Create span for this prompt
            span = self._client.span(
                trace_id=trace_id,
                name="user-prompt",
                input={"prompt": prompt[:2000]},  # Truncate for safety
                metadata={
                    "prompt_length": len(prompt),
                    "timestamp": datetime.now().isoformat(),
                    **metadata
                }
            )
            span.end()

            # Update activity
            self._registry.update_session_activity(session_id)

            logger.debug(f"Logged user prompt: {span.id}")
            return span.id

        except Exception as e:
            logger.warning(f"Failed to log user prompt: {e}")
            return "noop"

    def start_tool_span(
        self,
        tool_name: str,
        tool_input: Dict[str, Any]
    ) -> str:
        """
        Start a span for a tool invocation (called from PreToolUse hook).

        Uses CWD-primary, PPID-fallback to find the correct session
        since tool hooks don't receive session_id.

        Args:
            tool_name: Name of the tool (Bash, Read, Write, etc.)
            tool_input: Tool input parameters

        Returns:
            span_id or "noop" if not enabled
        """
        if not self.enabled:
            return "noop"

        try:
            # Find session via CWD/PPID correlation
            session = self._registry.find_session_for_tool()
            if not session:
                logger.debug(f"No session found for tool {tool_name}")
                return "noop"

            trace_id = session.get("trace_id")
            session_id = session.get("session_id")

            if not trace_id or trace_id == "noop":
                return "noop"

            # Sanitize tool input (remove large content)
            safe_input = self._sanitize_input(tool_name, tool_input)

            # Create span
            span = self._client.span(
                trace_id=trace_id,
                name=f"tool:{tool_name}",
                input=safe_input,
                metadata={
                    "tool_name": tool_name,
                    "started_at": datetime.now().isoformat()
                }
            )

            # Add to pending spans
            self._registry.add_pending_span(session_id, span.id, tool_name)

            logger.debug(f"Started tool span: {tool_name} ({span.id})")
            return span.id

        except Exception as e:
            logger.warning(f"Failed to start tool span: {e}")
            return "noop"

    def end_tool_span(
        self,
        tool_name: str,
        tool_result: Any
    ) -> None:
        """
        End a tool span with its result (called from PostToolUse hook).

        Uses CWD-primary, PPID-fallback to find session and matches
        spans by tool_name (FIFO) to handle concurrent tool executions.

        Args:
            tool_name: Name of the tool
            tool_result: Tool execution result
        """
        if not self.enabled:
            return

        try:
            # Find session via CWD/PPID correlation
            session = self._registry.find_session_for_tool()
            if not session:
                return

            trace_id = session.get("trace_id")
            session_id = session.get("session_id")

            if not trace_id or trace_id == "noop":
                return

            # Pop matching span
            span_info = self._registry.pop_pending_span(session_id, tool_name)
            if not span_info:
                logger.debug(f"No pending span found for tool: {tool_name}")
                return

            # Sanitize output
            safe_output = self._sanitize_output(tool_name, tool_result)

            # Update and end the span
            span = self._client.span(
                id=span_info["span_id"],
                trace_id=trace_id
            )
            span.update(
                output=safe_output,
                end_time=datetime.now()
            )
            span.end()

            logger.debug(f"Ended tool span: {tool_name} ({span_info['span_id']})")

        except Exception as e:
            logger.warning(f"Failed to end tool span: {e}")

    def end_session_trace(
        self,
        session_id: str,
        **metadata
    ) -> None:
        """
        End the session trace and flush all pending data.

        Args:
            session_id: Session identifier
            **metadata: Final metadata to add
        """
        if not self.enabled:
            return

        try:
            session = self._registry.get_session(session_id)
            if not session:
                return

            trace_id = session.get("trace_id")

            if trace_id and trace_id != "noop":
                # Update trace with final metadata
                trace = self._client.trace(id=trace_id)
                trace.update(
                    metadata={
                        "ended_at": datetime.now().isoformat(),
                        **metadata
                    }
                )

            # Flush all pending events
            self._client.flush()

            # Mark session as stopped (keep for potential resume)
            self._registry.mark_session_stopped(session_id)

            logger.debug(f"Ended session trace: {trace_id}")

        except Exception as e:
            logger.warning(f"Failed to end session trace: {e}")

    def flush(self) -> None:
        """Flush any pending events to Langfuse."""
        if self.enabled:
            try:
                self._client.flush()
                logger.debug("Flushed Langfuse events")
            except Exception as e:
                logger.warning(f"Failed to flush: {e}")

    def _sanitize_input(
        self,
        tool_name: str,
        tool_input: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        Remove sensitive or large data from tool input.

        Truncates large values and marks them with size info.
        """
        if not isinstance(tool_input, dict):
            return {"raw": str(tool_input)[:500]}

        safe = {}
        for key, value in tool_input.items():
            if key in ('command', 'file_path', 'pattern', 'query', 'url'):
                # Keep these but truncate
                safe[key] = str(value)[:500] if value else None
            elif key == 'content':
                # File content - just show size
                content_len = len(str(value)) if value else 0
                if content_len > 200:
                    safe[key] = f"[{content_len} chars]"
                else:
                    safe[key] = value
            elif isinstance(value, str) and len(value) > 500:
                safe[key] = f"[{len(value)} chars]"
            elif isinstance(value, (dict, list)):
                # Serialize and check size
                serialized = json.dumps(value)
                if len(serialized) > 500:
                    safe[key] = f"[{len(serialized)} chars JSON]"
                else:
                    safe[key] = value
            else:
                safe[key] = value

        return safe

    def _sanitize_output(
        self,
        tool_name: str,
        tool_result: Any
    ) -> Any:
        """
        Remove sensitive or large data from tool output.
        """
        if tool_result is None:
            return None

        if isinstance(tool_result, str):
            if len(tool_result) > 1000:
                return f"[{len(tool_result)} chars]"
            return tool_result

        if isinstance(tool_result, dict):
            safe = {}
            for key, value in tool_result.items():
                if isinstance(value, str) and len(value) > 500:
                    safe[key] = f"[{len(value)} chars]"
                else:
                    safe[key] = value
            return safe

        # For other types, stringify and truncate
        result_str = str(tool_result)
        if len(result_str) > 1000:
            return f"[{len(result_str)} chars]"
        return tool_result


# Singleton tracer instance
_tracer: Optional[ClaudeCodeTracer] = None
_tracer_checked: bool = False


def get_tracer() -> Optional[ClaudeCodeTracer]:
    """
    Get the singleton tracer instance, or None if Langfuse is not configured.

    Returns None when:
    - LANGFUSE_PUBLIC_KEY is not set
    - LANGFUSE_SECRET_KEY is not set
    - LANGFUSE_ENABLED is explicitly set to 'false'
    - Langfuse SDK is not installed

    This ensures zero overhead when Langfuse is not configured - no objects
    created, no methods called, hooks work exactly as they did before.
    """
    global _tracer, _tracer_checked

    # Only check once per process
    if not _tracer_checked:
        _tracer_checked = True
        config = get_config()

        # Return None if Langfuse is not configured
        if not config.is_available():
            _tracer = None
        elif not HAS_LANGFUSE:
            _tracer = None
        else:
            _tracer = ClaudeCodeTracer(config)

    return _tracer
