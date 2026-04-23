#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "pytest",
# ]
# ///
"""
Integration tests for recall-preamble wiring in Tier 1 + 2 skills.

Two layers:

1. Structural — every tier-1/2 skill's SKILL.md contains a recall block
   delimited by <!-- recall:begin --> / <!-- recall:end --> with the
   expected command shape.

2. Sandbox e2e — a fake KB dir is seeded with one distinctive learning;
   recall.py is invoked with a query aligned to that seed; output must
   contain the seed's ID. Proves the recall CLI actually retrieves when
   a skill would call it.

Run:
    uv run toolkit/packages/plugins/reflect/tests/test_skill_recall_integration.py
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[5]
SKILLS = ROOT / "toolkit" / "packages" / "skills"
RECALL = ROOT / "toolkit" / "packages" / "plugins" / "reflect" / "skills" / "recall" / "scripts" / "recall.py"

# Must match TARGETS in inject_recall_preamble.py
SKILLS_WITH_RECALL = [
    "plan", "plan-tdd", "plan-gh", "research", "brainstorm", "critique",
    "implement", "gh-issue", "find-missing-tests", "validate",
]

BEGIN = "<!-- recall:begin -->"
END = "<!-- recall:end -->"


# ─── Layer 1: structural ──────────────────────────────────────────────

@pytest.mark.parametrize("skill", SKILLS_WITH_RECALL)
def test_skill_has_recall_block(skill: str):
    """Every tier-1/2 skill has the recall preamble block."""
    path = SKILLS / skill / "SKILL.md"
    assert path.exists(), f"skill source missing: {path}"
    content = path.read_text()
    assert BEGIN in content, f"{skill}: missing {BEGIN}"
    assert END in content, f"{skill}: missing {END}"


@pytest.mark.parametrize("skill", SKILLS_WITH_RECALL)
def test_skill_recall_block_has_correct_command(skill: str):
    """The recall block invokes recall.py with the right flags."""
    content = (SKILLS / skill / "SKILL.md").read_text()
    block = content.split(BEGIN, 1)[1].split(END, 1)[0]
    assert "recall.py" in block, f"{skill}: recall.py not referenced"
    assert "--limit" in block, f"{skill}: missing --limit flag"
    assert "--format markdown" in block, f"{skill}: missing --format markdown"
    assert "{{HOME_TOOL_DIR}}" in block, f"{skill}: missing template placeholder"


@pytest.mark.parametrize("skill", SKILLS_WITH_RECALL)
def test_skill_recall_block_has_query_guidance(skill: str):
    """Each skill has skill-specific guidance on building the query."""
    content = (SKILLS / skill / "SKILL.md").read_text()
    block = content.split(BEGIN, 1)[1].split(END, 1)[0]
    assert f"/{skill}" in block, f"{skill}: block doesn't mention its own name"
    assert "Query construction" in block, f"{skill}: no query construction hint"


def test_recall_preamble_positioned_before_main_flow():
    """Preamble must precede the first imperative step in every skill."""
    for skill in SKILLS_WITH_RECALL:
        content = (SKILLS / skill / "SKILL.md").read_text()
        preamble_pos = content.find(BEGIN)
        assert preamble_pos > 0, f"{skill}: preamble not found"
        # find the first non-preamble imperative marker
        for marker in ("## Planning Process", "## Workflow", "## Process",
                       "## Implementation", "## Review"):
            idx = content.find(marker)
            if idx > 0:
                assert preamble_pos < idx, \
                    f"{skill}: preamble at {preamble_pos} is AFTER '{marker}' at {idx}"


# ─── Layer 2: sandbox e2e ─────────────────────────────────────────────

@pytest.fixture
def sandbox_kb(tmp_path):
    """Minimal fake KB that the `learnings` CLI can search."""
    # Return a callable that installs a stub `learnings` binary + seed docs.
    # We don't exercise nano-graphrag here — we stub the subprocess output.
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()

    # Stub `learnings` CLI that returns a fixed JSON result containing the seed.
    # Shape matches what real CLI emits: {"context": "<chunks separated by --New Chunk-->"}
    seed_id = "lrn-test-fixture-abc123"
    chunk = (
        "---\n"
        f"id: {seed_id}\n"
        "title: Test fixture learning\n"
        "key_insight: This is the seeded fixture for sandbox e2e testing\n"
        "confidence: HIGH\n"
        "tags: [test, fixture, sandbox]\n"
        "---\n\n"
        "**How to apply:** If this text appears in recall output, the loop works.\n"
    )
    fake_output = json.dumps({"context": chunk})

    stub = bin_dir / "learnings"
    stub.write_text(
        "#!/usr/bin/env bash\n"
        "# stub learnings CLI for sandbox tests\n"
        f"cat <<'EOF'\n{fake_output}\nEOF\n"
    )
    stub.chmod(0o755)

    # Fake ~/.learnings/cli/ layout so recall.py finds it via candidate path.
    home_learnings = tmp_path / "home" / ".learnings" / "cli"
    home_learnings.mkdir(parents=True)
    real_stub = home_learnings / "learnings"
    shutil.copy(stub, real_stub)
    real_stub.chmod(0o755)

    # Also create the nano_graphrag_cache dir so kb_last_modified() works.
    (tmp_path / "home" / ".learnings" / "nano_graphrag_cache").mkdir()

    yield {
        "home": tmp_path / "home",
        "seed_id": seed_id,
        "reflect_state": tmp_path / "reflect_state",
    }


UV = shutil.which("uv") or "/opt/homebrew/bin/uv"


def test_recall_returns_seeded_learning(sandbox_kb):
    """With a seeded KB, recall.py surfaces the fixture ID."""
    env = os.environ.copy()
    env["HOME"] = str(sandbox_kb["home"])
    env["REFLECT_STATE_DIR"] = str(sandbox_kb["reflect_state"])

    result = subprocess.run(
        [
            UV, "run", "--quiet", str(RECALL),
            "anything",  # query doesn't matter — stub returns fixed output
            "--limit", "5",
            "--format", "markdown",
            "--no-cache",
        ],
        capture_output=True,
        text=True,
        timeout=60,
        env=env,
    )

    assert result.returncode == 0, f"recall exited {result.returncode}: {result.stderr}"
    assert sandbox_kb["seed_id"] in result.stdout, (
        f"seeded ID {sandbox_kb['seed_id']} not in recall output.\n"
        f"stdout: {result.stdout!r}\nstderr: {result.stderr!r}"
    )


def test_recall_graceful_on_missing_kb(tmp_path):
    """D9: when KB is absent, recall exits 0 with no output."""
    # Isolate PATH to a dir that ONLY contains uv — no `learnings` binary.
    isolated_bin = tmp_path / "bin"
    isolated_bin.mkdir()
    (isolated_bin / "uv").symlink_to(UV)

    env = os.environ.copy()
    env["HOME"] = str(tmp_path)  # empty — no ~/.learnings
    env["PATH"] = str(isolated_bin)

    result = subprocess.run(
        [UV, "run", "--quiet", str(RECALL), "anything",
         "--limit", "3", "--format", "markdown", "--no-cache"],
        capture_output=True, text=True, timeout=30, env=env,
    )
    assert result.returncode == 0, f"should exit 0, got {result.returncode}"
    # Output should be empty-ish when KB absent (no "[lrn-...]" entries)
    assert "[lrn-" not in result.stdout


if __name__ == "__main__":
    import sys
    sys.exit(pytest.main([__file__, "-v"]))
