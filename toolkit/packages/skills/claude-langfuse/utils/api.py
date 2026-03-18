#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "requests>=2.28.0",
# ]
# ///
"""
Langfuse API client for the langfuse skill.

Provides a simple interface to query Langfuse traces and observations.
"""

import os
import json
import logging
import requests
from datetime import datetime, timedelta
from typing import Optional, List, Dict, Any
from pathlib import Path

# Set up logging
logger = logging.getLogger(__name__)


class LangfuseClient:
    """Simple Langfuse API client."""

    def __init__(self):
        self.public_key = os.getenv('LANGFUSE_PUBLIC_KEY')
        self.secret_key = os.getenv('LANGFUSE_SECRET_KEY')
        self.host = os.getenv('LANGFUSE_HOST', 'https://cloud.langfuse.com')

        if not self.public_key or not self.secret_key:
            raise ValueError("LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY must be set")

        self.auth = (self.public_key, self.secret_key)

    def get_traces(self, limit: int = 10, **kwargs) -> List[Dict[str, Any]]:
        """Get recent traces."""
        params = {'limit': limit, **kwargs}
        response = requests.get(
            f'{self.host}/api/public/traces',
            auth=self.auth,
            params=params
        )
        response.raise_for_status()
        return response.json().get('data', [])

    def get_trace(self, trace_id: str) -> Dict[str, Any]:
        """Get a specific trace by ID."""
        response = requests.get(
            f'{self.host}/api/public/traces/{trace_id}',
            auth=self.auth
        )
        response.raise_for_status()
        return response.json()

    def get_observations(self, trace_id: str, limit: int = 100) -> List[Dict[str, Any]]:
        """Get observations for a trace."""
        response = requests.get(
            f'{self.host}/api/public/observations',
            auth=self.auth,
            params={'traceId': trace_id, 'limit': limit}
        )
        response.raise_for_status()
        return response.json().get('data', [])

    def get_current_session_id(self) -> Optional[str]:
        """Get current session ID from local state files."""
        sessions_dir = Path.home() / '.claude' / 'langfuse' / 'sessions'
        index_file = sessions_dir / 'index.json'

        if not index_file.exists():
            return None

        try:
            with open(index_file, 'r') as f:
                index = json.load(f)

            # Find session for current PPID
            ppid = str(os.getppid())
            if ppid in index.get('ppid_to_session', {}):
                return index['ppid_to_session'][ppid]

            # Fallback: get most recent active session
            for session_file in sorted(sessions_dir.glob('*.json'),
                                       key=lambda x: x.stat().st_mtime,
                                       reverse=True):
                if session_file.name == 'index.json':
                    continue
                with open(session_file, 'r') as f:
                    session = json.load(f)
                if session.get('status') == 'active':
                    return session.get('session_id')

        except Exception as e:
            logger.debug(f"Failed to get current session ID: {e}")

        return None

    def get_current_trace_id(self) -> Optional[str]:
        """Get current trace ID from local state files."""
        sessions_dir = Path.home() / '.claude' / 'langfuse' / 'sessions'
        session_id = self.get_current_session_id()

        if not session_id:
            return None

        session_file = sessions_dir / f'{session_id}.json'
        if session_file.exists():
            try:
                with open(session_file, 'r') as f:
                    session = json.load(f)
                return session.get('trace_id')
            except Exception as e:
                logger.debug(f"Failed to get trace ID from session file: {e}")

        return None


def format_duration(start: str, end: str) -> str:
    """Format duration between two ISO timestamps."""
    try:
        start_dt = datetime.fromisoformat(start.replace('Z', '+00:00'))
        end_dt = datetime.fromisoformat(end.replace('Z', '+00:00'))
        dur_sec = (end_dt - start_dt).total_seconds()

        if dur_sec < 1:
            return f'{dur_sec*1000:.0f}ms'
        elif dur_sec < 60:
            return f'{dur_sec:.1f}s'
        elif dur_sec < 3600:
            return f'{dur_sec/60:.1f}m'
        else:
            return f'{dur_sec/3600:.1f}h'
    except Exception:
        return 'N/A'


def format_timestamp(ts: str) -> str:
    """Format ISO timestamp for display."""
    if not ts:
        return 'N/A'
    return ts[:19].replace('T', ' ')
