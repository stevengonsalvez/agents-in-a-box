"""Typed records for reflect persistence and service boundaries."""

from __future__ import annotations

from dataclasses import dataclass, field

from .enums import (
    ArtifactStatus,
    ArtifactType,
    IndexBackend,
    IndexJobStatus,
    LearningStatus,
    PrivacyLevel,
    ProposalStatus,
    ProposalType,
    SourceStatus,
)


@dataclass(slots=True)
class LearningRecord:
    title: str
    category: str = "Unknown"
    confidence: str = "LOW"
    status: LearningStatus = LearningStatus.PENDING
    scope: str = "project"
    source_tool: str = ""
    source_provider: str = ""
    source_kind: str = ""
    source_path: str = ""
    source_quote: str = ""
    source_quote_hash: str = ""
    content_hash: str = ""
    session_id: str = ""
    thread_id: str = ""
    privacy_level: PrivacyLevel = PrivacyLevel.INTERNAL
    artifact_path: str = ""
    sidecar_path: str = ""
    commit_hash: str | None = None
    supersedes_learning_id: str | None = None
    superseded_by_learning_id: str | None = None


@dataclass(slots=True)
class ProposalRecord:
    learning_id: str
    proposal_type: ProposalType = ProposalType.LEARNING
    target_kind: str = ""
    target_path: str = ""
    agent_file: str = ""
    diff: str = ""
    status: ProposalStatus = ProposalStatus.PENDING
    decision_actor: str = ""
    rationale_json: str = "{}"


@dataclass(slots=True)
class SourceRecord:
    provider: str
    path: str
    project_name: str = ""
    source_kind: str = ""
    provider_id: str = ""
    canonical_project_id: str = ""
    content_hash: str = ""
    status: SourceStatus = SourceStatus.ACTIVE
    ingest_state: str = "discovered"


@dataclass(slots=True)
class IndexJobRecord:
    learning_id: str
    backend: IndexBackend
    status: IndexJobStatus = IndexJobStatus.PENDING
    idempotency_key: str = ""
    attempt_count: int = 0
    last_error: str = ""


@dataclass(slots=True)
class RecallEventRecord:
    learning_id: str
    query: str
    query_hash: str
    source_context: str = ""
    rank: int = 0
    feedback: str = ""


@dataclass(slots=True)
class ArtifactRecord:
    learning_id: str
    artifact_type: ArtifactType
    path: str
    content_hash: str = ""
    status: ArtifactStatus = ArtifactStatus.CREATED
    metadata: dict[str, str] = field(default_factory=dict)
