"""Typed errors for the reflect memory substrate.

Kept separate so callers can catch the precise failure (tenant scope vs.
input validation) instead of a bare ``ValueError``.
"""

from __future__ import annotations


class ReflectMemoryError(Exception):
    """Base class for every error raised by this package."""


class TenantScopeError(ReflectMemoryError):
    """A tenant-scoped operation was attempted without a workspace id.

    Tenant isolation is mandatory: every read and write must be bound to a
    ``workspace_id`` before ranking or graph expansion. This is raised early
    (before any SQL is built) so a missing tenant can never silently widen a
    query to all tenants.
    """


class ValidationError(ReflectMemoryError):
    """An input value failed validation before reaching the database."""
