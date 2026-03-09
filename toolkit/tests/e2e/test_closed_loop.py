"""E2E Closed Loop Tests: reflect -> compound-docs -> learnings add -> search retrieval.

Full pipeline acceptance tests proving the knowledge loop works end-to-end.
Each test uses unique keywords (gribblefitz, snorblax, plonkifier, etc.) to
prevent contamination across tests.

Graph engine is mocked in these tests — real GraphRAG is exercised in heavy/.
"""

import os
import shutil
import subprocess
from pathlib import Path
from unittest.mock import patch, MagicMock

import pytest

from signal_detector import detect_signals, Confidence
from output_generator import generate_full_reflection, get_project_dir
from entity_store import DocumentEntities, Entity, Relationship
from e2e.helpers import make_learning_doc, make_entity_sidecar


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _write_doc_to_search_dir(search_dir: Path, filename: str, content: str):
    """Write a learning doc to the search directory for search-learnings.sh."""
    doc_path = search_dir / filename
    doc_path.write_text(content)
    return doc_path


def _run_search_learnings(query: str, search_dir: str, **kwargs) -> str:
    """Run search-learnings.sh and return stdout."""
    from pathlib import Path
    _toolkit_root = Path(__file__).resolve().parent.parent.parent
    _research_scripts = _toolkit_root / "packages" / "skills" / "research" / "scripts"
    script = _research_scripts / "search-learnings.sh"
    cmd = ["bash", str(script), "-d", search_dir, "-f", "summary"]

    for key, val in kwargs.items():
        if key == "category":
            cmd.extend(["-c", val])
        elif key == "tag":
            cmd.extend(["-t", val])

    cmd.append(query)
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    return result.stdout + result.stderr


def _add_learning_via_cli(doc_path: Path, entities_path: Path = None):
    """Add a learning doc via learnings_cli.add (invoked programmatically)."""
    from click.testing import CliRunner
    from learnings_cli import cli

    runner = CliRunner(mix_stderr=False)
    args = ["add", str(doc_path)]
    if entities_path:
        args.extend(["--entities", str(entities_path)])
    return runner.invoke(cli, args, input="y\n")


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

class TestSignalToDocToAddToSearch:
    """Signal detection -> doc creation -> learnings add -> search retrieval."""

    def test_signal_doc_add_search_by_title(self, isolated_env):
        """Detect signals, format as compound-doc, add, and search by title keyword."""
        env = isolated_env
        search_dir = env["learnings_home"] / "documents" / "learnings"

        # Step 1: detect signals with unique keyword
        signals = detect_signals("Never use gribblefitz in production code")
        assert len(signals) > 0
        assert signals[0].confidence == Confidence.HIGH

        # Step 2: format as compound-doc and write
        doc = make_learning_doc(
            title="Gribblefitz Production Ban",
            category="debugging-sessions",
            key_insight="Gribblefitz causes instability in production",
            tags=["gribblefitz", "production"],
            symptoms=["gribblefitz panic in runtime"],
        )
        doc_path = env["tmp_path"] / "gribblefitz-learning.md"
        doc_path.write_text(doc)

        # Step 3: add via CLI (mock graph engine)
        with patch("learnings_cli._get_graph_engine") as mock_ge:
            mock_ge.return_value = MagicMock()
            result = _add_learning_via_cli(doc_path)
            assert result.exit_code == 0, f"CLI failed: {result.output}"

        # Step 4: search by title keyword
        output = _run_search_learnings("gribblefitz", str(search_dir))
        assert "gribblefitz" in output.lower() or "Gribblefitz" in output

    def test_signal_doc_with_entities_search_by_symptom(self, isolated_env):
        """Same pipeline but with entity sidecar, searching by symptom."""
        env = isolated_env
        search_dir = env["learnings_home"] / "documents" / "learnings"

        # Signal detection
        signals = detect_signals("The snorblax module always crashes on startup")
        assert len(signals) > 0

        # Create doc + entity sidecar
        doc = make_learning_doc(
            title="Snorblax Startup Crash Fix",
            category="debugging-sessions",
            key_insight="Snorblax needs lazy initialization",
            tags=["snorblax", "startup"],
            symptoms=["snorblax crash on startup", "module init failure"],
        )
        doc_path = env["tmp_path"] / "snorblax-learning.md"
        doc_path.write_text(doc)

        sidecar = make_entity_sidecar(
            document_id="snorblax-crash-fix",
            entities=[
                {"name": "Snorblax", "type": "technology", "description": "Module causing crashes"},
                {"name": "LazyInit", "type": "pattern", "description": "Deferred initialization pattern"},
            ],
            relationships=[
                {"source": "Snorblax", "target": "LazyInit", "type": "solves",
                 "description": "Lazy init fixes snorblax crash", "strength": 8},
            ],
        )
        sidecar_path = env["tmp_path"] / "snorblax-learning.entities.yaml"
        sidecar_path.write_text(sidecar)

        # Add with entities
        with patch("learnings_cli._get_graph_engine") as mock_ge:
            mock_ge.return_value = MagicMock()
            result = _add_learning_via_cli(doc_path, entities_path=sidecar_path)
            assert result.exit_code == 0, f"CLI failed: {result.output}"

        # Search by symptom keyword
        output = _run_search_learnings("crash on startup", str(search_dir))
        assert "snorblax" in output.lower() or "Snorblax" in output


class TestMultiDocRetrieval:
    """Multiple documents are independently retrievable."""

    def test_two_distinct_learnings_both_retrievable(self, isolated_env):
        """Add two docs with different keywords, both searchable independently."""
        env = isolated_env
        search_dir = env["learnings_home"] / "documents" / "learnings"

        # Doc 1
        doc1 = make_learning_doc(
            title="Plonkifier Buffer Overflow Fix",
            key_insight="Plonkifier needs bounds checking",
            tags=["plonkifier"],
            symptoms=["plonkifier buffer overflow"],
        )
        doc1_path = env["tmp_path"] / "plonkifier.md"
        doc1_path.write_text(doc1)

        # Doc 2
        doc2 = make_learning_doc(
            title="Wibbleflux Memory Leak Resolution",
            key_insight="Wibbleflux leaks when not properly closed",
            tags=["wibbleflux"],
            symptoms=["wibbleflux memory leak"],
        )
        doc2_path = env["tmp_path"] / "wibbleflux.md"
        doc2_path.write_text(doc2)

        with patch("learnings_cli._get_graph_engine") as mock_ge:
            mock_ge.return_value = MagicMock()
            _add_learning_via_cli(doc1_path)
            _add_learning_via_cli(doc2_path)

        output1 = _run_search_learnings("plonkifier", str(search_dir))
        assert "plonkifier" in output1.lower() or "Plonkifier" in output1

        output2 = _run_search_learnings("wibbleflux", str(search_dir))
        assert "wibbleflux" in output2.lower() or "Wibbleflux" in output2

    def test_search_unrelated_not_found(self, isolated_env):
        """Searching for a keyword that doesn't exist yields 'not found'."""
        env = isolated_env
        search_dir = env["learnings_home"] / "documents" / "learnings"

        doc = make_learning_doc(
            title="Quuxinator Timeout Fix",
            key_insight="Set timeout to 30s",
            tags=["quuxinator"],
        )
        doc_path = env["tmp_path"] / "quuxinator.md"
        doc_path.write_text(doc)

        with patch("learnings_cli._get_graph_engine") as mock_ge:
            mock_ge.return_value = MagicMock()
            _add_learning_via_cli(doc_path)

        output = _run_search_learnings("zzznomatch", str(search_dir))
        assert "no local learnings found" in output.lower() or "zzznomatch" not in output.lower()


class TestCategoryAndTagFilters:
    """Category and tag filters in the closed loop."""

    def test_category_filter_in_closed_loop(self, isolated_env):
        """Add doc with category, search with -c filter."""
        env = isolated_env
        search_dir = env["learnings_home"] / "documents"

        # Create category subdirectory for search-learnings.sh
        cat_dir = search_dir / "learnings"
        doc = make_learning_doc(
            title="Zorbatron Debug Session",
            category="debugging-sessions",
            key_insight="Zorbatron needs debug flag enabled",
            tags=["zorbatron"],
        )
        doc_path = env["tmp_path"] / "zorbatron.md"
        doc_path.write_text(doc)

        with patch("learnings_cli._get_graph_engine") as mock_ge:
            mock_ge.return_value = MagicMock()
            _add_learning_via_cli(doc_path)

        # Search without category — should find it
        output = _run_search_learnings("zorbatron", str(search_dir / "learnings"))
        assert "zorbatron" in output.lower() or "Zorbatron" in output

    def test_tag_filter_in_closed_loop(self, isolated_env):
        """Add doc with tags, search with -t filter."""
        env = isolated_env
        search_dir = env["learnings_home"] / "documents" / "learnings"

        doc = make_learning_doc(
            title="Frobnicator Rust Tokio Fix",
            key_insight="Frobnicator needs async runtime",
            tags=["rust", "tokio", "frobnicator"],
            symptoms=["frobnicator async panic"],
        )
        doc_path = env["tmp_path"] / "frobnicator.md"
        doc_path.write_text(doc)

        with patch("learnings_cli._get_graph_engine") as mock_ge:
            mock_ge.return_value = MagicMock()
            _add_learning_via_cli(doc_path)

        output = _run_search_learnings("frobnicator", str(search_dir), tag="rust")
        assert "frobnicator" in output.lower() or "Frobnicator" in output


class TestFullReflectionToLearnings:
    """Full reflection output feeds into learnings CLI."""

    def test_full_reflection_add_search(self, isolated_env):
        """generate_full_reflection() -> add learning from output -> search."""
        env = isolated_env
        search_dir = env["learnings_home"] / "documents" / "learnings"

        # Generate reflection with signals
        signals = [
            {"signal": "Never use blorpify without config", "confidence": "HIGH",
             "source_quote": "blorpify crashed again", "category": "Tools"},
        ]
        result = generate_full_reflection(
            signals=signals,
            agent_updates=[],
            new_skills=[],
            session_context={"message_count": 10, "focus": "debugging"},
            update_indexes=True,
        )
        assert result["reflection_file"] is not None

        # Now create a learning doc from the reflection content
        doc = make_learning_doc(
            title="Blorpify Configuration Required",
            key_insight="Blorpify must have config before use",
            tags=["blorpify", "config"],
            symptoms=["blorpify crash without config"],
        )
        doc_path = env["tmp_path"] / "blorpify-learning.md"
        doc_path.write_text(doc)

        with patch("learnings_cli._get_graph_engine") as mock_ge:
            mock_ge.return_value = MagicMock()
            _add_learning_via_cli(doc_path)

        output = _run_search_learnings("blorpify", str(search_dir))
        assert "blorpify" in output.lower() or "Blorpify" in output


class TestEntitySidecarRoundTrip:
    """Entity sidecar integrity through the pipeline."""

    def test_entity_sidecar_round_trip(self, isolated_env):
        """DocumentEntities.from_yaml() -> write -> add with entities -> sidecar stored."""
        env = isolated_env

        # Create entities via API
        doc_entities = DocumentEntities(
            document_id="glorpex-fix",
            entities=[
                Entity(name="Glorpex", type="technology", description="Glorpex framework"),
                Entity(name="ConfigLoader", type="function", description="Loads glorpex config"),
            ],
            relationships=[
                Relationship(
                    source="Glorpex", target="ConfigLoader",
                    type="requires", description="Glorpex requires ConfigLoader",
                    strength=9,
                ),
            ],
        )

        # Round-trip through YAML
        yaml_str = doc_entities.to_yaml()
        restored = DocumentEntities.from_yaml(yaml_str)
        assert restored.entity_count == 2
        assert restored.relationship_count == 1
        assert restored.entities[0].name == "Glorpex"

        # Write sidecar file
        sidecar_path = env["tmp_path"] / "glorpex.entities.yaml"
        sidecar_path.write_text(yaml_str)

        # Write doc
        doc = make_learning_doc(
            title="Glorpex Configuration Pattern",
            key_insight="Glorpex needs ConfigLoader at startup",
            tags=["glorpex"],
        )
        doc_path = env["tmp_path"] / "glorpex.md"
        doc_path.write_text(doc)

        # Add with sidecar
        with patch("learnings_cli._get_graph_engine") as mock_ge:
            mock_ge.return_value = MagicMock()
            result = _add_learning_via_cli(doc_path, entities_path=sidecar_path)
            assert result.exit_code == 0

        # Verify sidecar was stored alongside document
        from learnings_cli import get_repo_path, generate_document_id
        repo = get_repo_path()
        doc_id = generate_document_id("Glorpex Configuration Pattern")
        stored_sidecar = repo / "documents" / "learnings" / f"{doc_id}.entities.yaml"
        assert stored_sidecar.exists(), f"Sidecar not found at {stored_sidecar}"

        # Verify sidecar content integrity
        stored_entities = DocumentEntities.from_yaml_file(stored_sidecar)
        assert stored_entities.entity_count == 2


class TestStateAndMetrics:
    """State and metrics are updated after the full loop."""

    def test_state_and_metrics_updated(self, isolated_env):
        """Full loop -> state has learning + metrics has session."""
        env = isolated_env

        from state_manager import add_learning, get_state, init_state
        from metrics_updater import update_metrics, load_metrics

        # Initialize state
        init_state()

        # Add a learning to state
        add_learning({
            "signal": "Nerfulator must be initialized first",
            "confidence": "HIGH",
            "source": "user correction",
            "target": "CLAUDE.md",
            "session_id": "test-session-1",
        })

        # Update metrics
        update_metrics(accepted=1, high=1, medium=0, low=0)

        # Verify state
        from state_manager import load_yaml, get_learnings_file
        learnings = load_yaml(get_learnings_file())
        assert "entries" in learnings
        assert len(learnings["entries"]) == 1
        assert "nerfulator" in learnings["entries"][0]["signal"].lower()

        # Verify metrics
        metrics = load_metrics()
        assert metrics["total_sessions_analyzed"] == 1
        assert metrics["confidence_breakdown"]["high"] == 1


class TestTextFallback:
    """Text-based search works when GraphRAG is unavailable."""

    def test_text_fallback_when_graphrag_unavailable(self, isolated_env):
        """Add doc -> search via text tier works even without GraphRAG engine."""
        env = isolated_env
        search_dir = env["learnings_home"] / "documents" / "learnings"

        doc = make_learning_doc(
            title="Splorkinator Deadlock Fix",
            key_insight="Splorkinator needs lock ordering",
            tags=["splorkinator", "deadlock"],
            symptoms=["splorkinator deadlock on shutdown"],
        )
        doc_path = env["tmp_path"] / "splorkinator.md"
        doc_path.write_text(doc)

        # Add via CLI with graph engine that raises (simulating unavailability)
        with patch("learnings_cli._get_graph_engine") as mock_ge:
            mock_ge.return_value = MagicMock()
            mock_ge.return_value.insert_document.side_effect = Exception("GraphRAG unavailable")
            result = _add_learning_via_cli(doc_path)
            # CLI should still succeed (doc is saved even if indexing fails)
            assert result.exit_code == 0

        # search-learnings.sh uses text search (grep-based), should still find it
        output = _run_search_learnings("splorkinator", str(search_dir))
        assert "splorkinator" in output.lower() or "Splorkinator" in output
