"""Shared enums for reflect lifecycle and persistence."""

from enum import StrEnum


class LearningStatus(StrEnum):
    DETECTED = "detected"
    PENDING = "pending"  # Compatibility with v3.1 callers
    PROPOSED = "proposed"
    APPROVED = "approved"
    MATERIALIZED = "materialized"
    INDEXED = "indexed"
    RECALLED = "recalled"
    SUPERSEDED = "superseded"
    REVERTED = "reverted"
    REJECTED = "rejected"


class ProposalStatus(StrEnum):
    PENDING = "pending"
    ACCEPTED = "accepted"
    REJECTED = "rejected"
    MATERIALIZED = "materialized"


class ProposalType(StrEnum):
    LEARNING = "learning"
    AGENT_UPDATE = "agent_update"
    KNOWLEDGE_NOTE = "knowledge_note"
    SKILL_UPDATE = "skill_update"


class SourceStatus(StrEnum):
    ACTIVE = "active"
    STALE = "stale"
    ARCHIVED = "archived"


class PrivacyLevel(StrEnum):
    INTERNAL = "internal"
    RESTRICTED = "restricted"
    SECRET_REDACTED = "secret_redacted"


class IndexJobStatus(StrEnum):
    PENDING = "pending"
    RUNNING = "running"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    SKIPPED = "skipped"


class IndexBackend(StrEnum):
    GRAPHRAG = "graphrag"
    QMD = "qmd"


class ArtifactType(StrEnum):
    KNOWLEDGE_NOTE = "knowledge_note"
    ENTITY_SIDECAR = "entity_sidecar"
    EPISODE_NOTE = "episode_note"
    ARCHIVED_MEMORY = "archived_memory"


class ArtifactStatus(StrEnum):
    CREATED = "created"
    VALIDATED = "validated"
    INDEXED = "indexed"
    FAILED = "failed"
