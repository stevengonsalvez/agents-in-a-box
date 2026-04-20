"""
Langfuse integration for Claude Code hooks.

This module provides optional Langfuse observability with zero overhead
when not configured. If LANGFUSE_PUBLIC_KEY is not set, all operations
become no-ops.

Usage:
    from utils.langfuse import get_tracer

    tracer = get_tracer()
    tracer.start_session_trace(session_id, source)
    # ... hook logic ...
    tracer.flush()
"""

from .config import LangfuseConfig
from .tracer import get_tracer, ClaudeCodeTracer

__all__ = ['get_tracer', 'ClaudeCodeTracer', 'LangfuseConfig']
