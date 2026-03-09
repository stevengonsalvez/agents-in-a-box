"""E2E Reflect Workflow Tests: signal_detector -> output_generator -> state_manager -> metrics_updater.

Tests the /reflect skill chain end-to-end. Each test exercises the full
pipeline from signal detection through state/metrics persistence.
"""

import os
from pathlib import Path
from unittest.mock import patch

import pytest
import yaml

from signal_detector import detect_signals, Confidence, Category
from output_generator import (
    create_reflection_file,
    generate_full_reflection,
    update_project_index,
    update_global_index,
    update_agent_learnings,
    get_project_reflections_dir,
    get_global_reflections_dir,
)
from state_manager import (
    init_state,
    add_learning,
    add_pending_low_confidence,
    get_state,
    load_yaml,
    get_learnings_file,
    get_state_file,
)
from metrics_updater import update_metrics, load_metrics


class TestSignalsToReflection:
    """Signal detection output feeds into reflection file creation."""

    def test_signals_produce_reflection_file(self, isolated_env):
        """detect_signals -> create_reflection_file -> file exists with content."""
        env = isolated_env

        # Detect signals
        conversation = (
            "Never use deprecated API for xyloquartz\n"
            "The xyloquartz module must use v2 endpoint\n"
        )
        signals = detect_signals(conversation)
        assert len(signals) >= 1

        # Convert Signal dataclasses to dicts for output_generator
        signal_dicts = [
            {
                "signal": s.signal,
                "confidence": s.confidence.value,
                "source_quote": s.source_quote,
                "category": s.category.value,
            }
            for s in signals
        ]

        # Create reflection file
        reflection_path = create_reflection_file(
            signals=signal_dicts,
            agent_updates=[],
            new_skills=[],
            session_context={"message_count": 5, "focus": "xyloquartz"},
        )

        assert reflection_path.exists()
        content = reflection_path.read_text()
        assert "Signals Detected" in content
        assert "xyloquartz" in content.lower()


class TestHighSignalsStatePersistence:
    """HIGH confidence signals are persisted to state."""

    def test_high_signals_stored_in_learnings(self, isolated_env):
        """HIGH signals -> state_manager.add_learning() stores them."""
        env = isolated_env

        # Detect a HIGH confidence signal
        signals = detect_signals("Always validate murfnax input before processing")
        high_signals = [s for s in signals if s.confidence == Confidence.HIGH]
        assert len(high_signals) >= 1

        # Initialize state and add learning
        init_state()
        add_learning({
            "signal": high_signals[0].signal,
            "confidence": "HIGH",
            "source": "user correction",
            "target": "CLAUDE.md",
            "session_id": "test-murfnax",
        })

        # Verify stored
        learnings = load_yaml(get_learnings_file())
        assert "entries" in learnings
        assert len(learnings["entries"]) == 1
        assert learnings["entries"][0]["confidence"] == "HIGH"


class TestLowSignalsPendingQueue:
    """LOW confidence signals go to pending review queue."""

    def test_low_signals_queued_for_review(self, isolated_env):
        """LOW signals -> state_manager.add_pending_low_confidence() queues them."""
        env = isolated_env

        # Detect a LOW confidence signal
        signals = detect_signals("The krazzlepuff module seems like it could use refactoring")
        # Should detect via "seems like" (LOW) + category match
        assert len(signals) >= 1

        # Initialize state and add to pending
        init_state()
        for s in signals:
            if s.confidence == Confidence.LOW:
                add_pending_low_confidence({
                    "signal": s.signal,
                    "source_quote": s.source_quote,
                    "category": s.category.value,
                })
                break

        # Verify queued
        state = get_state()
        pending = state.get("pending_low_confidence", [])
        assert len(pending) >= 1
        assert pending[0]["awaiting_validation"] is True


class TestMetricsAfterReflection:
    """Metrics are correctly updated after reflection."""

    def test_metrics_updated_after_reflection(self, isolated_env):
        """Session count incremented, confidence breakdown correct."""
        env = isolated_env

        # Detect signals
        signals = detect_signals(
            "Never use global state in frotzinator\n"
            "That's exactly right, keep it modular\n"
            "The frotzinator seems like it could be faster\n"
        )

        high_count = sum(1 for s in signals if s.confidence == Confidence.HIGH)
        medium_count = sum(1 for s in signals if s.confidence == Confidence.MEDIUM)
        low_count = sum(1 for s in signals if s.confidence == Confidence.LOW)

        # Update metrics
        metrics = update_metrics(
            accepted=1,
            rejected=0,
            high=high_count,
            medium=medium_count,
            low=low_count,
        )

        assert metrics["total_sessions_analyzed"] == 1
        assert metrics["total_signals_detected"] == high_count + medium_count + low_count
        assert metrics["confidence_breakdown"]["high"] == high_count


class TestTwoReflectionCycles:
    """State accumulates over multiple reflection cycles."""

    def test_two_cycles_accumulate(self, isolated_env):
        """Metrics show 2 sessions, learnings accumulate."""
        env = isolated_env

        init_state()

        # Cycle 1
        add_learning({
            "signal": "Blitzmorph needs initialization",
            "confidence": "HIGH",
            "source": "correction",
            "target": "CLAUDE.md",
            "session_id": "cycle-1",
        })
        update_metrics(accepted=1, high=1)

        # Cycle 2
        add_learning({
            "signal": "Quantiflex must be closed after use",
            "confidence": "HIGH",
            "source": "correction",
            "target": "CLAUDE.md",
            "session_id": "cycle-2",
        })
        update_metrics(accepted=1, high=1)

        # Verify accumulation
        learnings = load_yaml(get_learnings_file())
        assert len(learnings["entries"]) == 2

        metrics = load_metrics()
        assert metrics["total_sessions_analyzed"] == 2
        assert metrics["confidence_breakdown"]["high"] == 2
        assert metrics["total_changes_accepted"] == 2


class TestEmptySession:
    """Empty session still creates valid output."""

    def test_empty_session_creates_output(self, isolated_env):
        """Reflection file with 'No signals detected' when no signals found."""
        env = isolated_env

        # Empty conversation with no pattern matches
        signals = detect_signals("the weather is nice today")

        signal_dicts = [
            {
                "signal": s.signal,
                "confidence": s.confidence.value,
                "source_quote": s.source_quote,
                "category": s.category.value,
            }
            for s in signals
        ]

        reflection_path = create_reflection_file(
            signals=signal_dicts,
            agent_updates=[],
            new_skills=[],
            session_context={"message_count": 1},
        )

        assert reflection_path.exists()
        content = reflection_path.read_text()
        assert "Reflection Analysis" in content
        # If no signals, should have the "No signals detected" row
        if not signals:
            assert "No signals detected" in content


class TestProjectAndGlobalIndexes:
    """Both project and global indexes contain entries after reflection."""

    def test_indexes_updated(self, isolated_env):
        """Project index and global index both contain reflection entries."""
        env = isolated_env

        signals = [
            {"signal": "Gribnatz requires auth", "confidence": "HIGH",
             "source_quote": "gribnatz needs auth", "category": "Security"},
        ]

        result = generate_full_reflection(
            signals=signals,
            agent_updates=[],
            new_skills=[],
            session_context={"message_count": 5},
            update_indexes=True,
        )

        # Check project index
        project_index = get_project_reflections_dir() / "index.md"
        assert project_index.exists()
        project_content = project_index.read_text()
        assert "Reflection Index" in project_content
        assert "1 detected" in project_content

        # Check global index
        global_index = get_global_reflections_dir() / "index.md"
        assert global_index.exists()
        global_content = global_index.read_text()
        assert "Global Reflection Index" in global_content
        assert "1 detected" in global_content


class TestAgentLearningsOnlyForHigh:
    """Agent learnings file updated only for HIGH agent_updates."""

    def test_high_update_creates_agent_learning(self, isolated_env):
        """HIGH agent_update -> per-agent learnings file; MEDIUM -> nothing."""
        env = isolated_env

        agent_updates = [
            {
                "agent_name": "superstar-engineer",
                "file_path": "~/.claude/agents/superstar-engineer.md",
                "section": "Best Practices",
                "confidence": "HIGH",
                "summary": "Vorpalax requires strict typing",
                "source_quote": "vorpalax crashed due to type mismatch",
                "rationale": "Prevent type-related crashes",
                "diff": "+ Always use strict types with vorpalax",
            },
            {
                "agent_name": "backend-developer",
                "file_path": "~/.claude/agents/backend-developer.md",
                "section": "Patterns",
                "confidence": "MEDIUM",
                "summary": "Consider caching for snazzle",
                "source_quote": "snazzle is a bit slow",
                "rationale": "Performance improvement",
                "diff": "+ Consider caching snazzle results",
            },
        ]

        result = generate_full_reflection(
            signals=[],
            agent_updates=agent_updates,
            new_skills=[],
            update_indexes=True,
        )

        # HIGH update should create agent learnings
        assert "superstar-engineer" in result["agent_learnings_updated"]

        # MEDIUM update should NOT create agent learnings
        assert "backend-developer" not in result["agent_learnings_updated"]

        # Verify the file exists for superstar-engineer
        agent_learnings = (
            get_global_reflections_dir() / "by-agent" / "superstar-engineer" / "learnings.md"
        )
        assert agent_learnings.exists()
        content = agent_learnings.read_text()
        assert "vorpalax" in content.lower()
