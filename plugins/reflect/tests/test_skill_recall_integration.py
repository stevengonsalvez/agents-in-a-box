#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "pytest",
#     "pyyaml",
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
    """Minimal fake KB that the `reflect` CLI can search."""
    # Install a stub `reflect` binary on PATH that returns canned JSON. We
    # don't exercise nano-graphrag here — recall.py just needs the subprocess
    # to return something parseable.
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()

    # Stub `reflect` CLI that returns a fixed JSON result containing the seed.
    # Shape matches what the real CLI emits: {"context": "<chunks separated by --New Chunk-->"}
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

    stub = bin_dir / "reflect"
    stub.write_text(
        "#!/usr/bin/env bash\n"
        "# stub reflect CLI for sandbox tests\n"
        f"cat <<'EOF'\n{fake_output}\nEOF\n"
    )
    stub.chmod(0o755)

    # Also create the nano_graphrag_cache dir so kb_last_modified() works.
    # (Path is unchanged from pre-migration — it's still under ~/.learnings/
    # for legacy QMD/cache-invalidation purposes; see recall.py.)
    (tmp_path / "home" / ".learnings" / "nano_graphrag_cache").mkdir(parents=True)

    yield {
        "home": tmp_path / "home",
        "bin": bin_dir,
        "seed_id": seed_id,
        "reflect_state": tmp_path / "reflect_state",
    }


UV = shutil.which("uv") or "/opt/homebrew/bin/uv"


def test_recall_returns_seeded_learning(sandbox_kb):
    """With a seeded KB, recall.py surfaces the fixture ID."""
    env = os.environ.copy()
    env["HOME"] = str(sandbox_kb["home"])
    env["REFLECT_STATE_DIR"] = str(sandbox_kb["reflect_state"])
    # Put the stub `reflect` binary first on PATH so shutil.which("reflect")
    # picks it up instead of any real install.
    env["PATH"] = f"{sandbox_kb['bin']}:{env.get('PATH', '/bin:/usr/bin')}"

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
    # Strip PATH so `reflect` and `qmd` are both unreachable; HOME-empty hides
    # ~/.learnings/ so the legacy fallback also can't fire. uv is invoked by
    # full path so PATH stripping is safe.
    env = os.environ.copy()
    env["HOME"] = str(tmp_path)  # empty — no ~/.learnings
    env["PATH"] = "/bin:/usr/bin"

    result = subprocess.run(
        [UV, "run", "--quiet", str(RECALL), "anything",
         "--limit", "3", "--format", "markdown", "--no-cache"],
        capture_output=True, text=True, timeout=30, env=env,
    )
    assert result.returncode == 0, f"should exit 0, got {result.returncode}"
    # Output should be empty-ish when KB absent (no "[lrn-...]" entries)
    assert "[lrn-" not in result.stdout


# ─── Layer 3: sandbox e2e — fusion with BOTH stubs ─────────────────────
# (RRF + QMD parse + parallel fan-out are all exercised by this e2e test —
# separate unit tests would just be importlib-fragile duplication.)

@pytest.fixture
def dual_stub_kb(tmp_path):
    """Install stubs for BOTH `reflect` and `qmd` + seed docs for both.

    reflect stub returns lrn-graph-X (distinct from lrn-qmd-X in QMD).
    This lets us verify fusion: the output should contain IDs from BOTH.
    """
    home = tmp_path / "home"
    (home / ".learnings" / "nano_graphrag_cache").mkdir(parents=True)
    docs_root = home / ".learnings" / "documents"
    (docs_root / "learnings").mkdir(parents=True)

    # Seed docs on disk so QMD stub can point at real files for parsing.
    for lid in ["lrn-qmd-x", "lrn-qmd-y"]:
        (docs_root / "learnings" / f"{lid}.md").write_text(
            f"---\nid: {lid}\ntitle: {lid}\nconfidence: HIGH\n"
            f"tags: [sandbox]\n---\n\n**How to apply:** qmd hit\n"
        )

    # reflect CLI stub — emits envelope JSON with 2 distinct chunks.
    graph_chunks = []
    for lid in ["lrn-graph-a", "lrn-graph-b"]:
        graph_chunks.append(
            f"---\nid: {lid}\ntitle: {lid}\nconfidence: HIGH\n"
            f"tags: [sandbox]\n---\n\n**How to apply:** graph hit\n"
        )
    graph_context = "\n--New Chunk--\n".join(graph_chunks)
    graph_envelope = json.dumps({"context": graph_context})

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    (bin_dir / "uv").symlink_to(UV)

    reflect_stub = bin_dir / "reflect"
    reflect_stub.write_text(
        f"#!/usr/bin/env bash\ncat <<'EOF'\n{graph_envelope}\nEOF\n"
    )
    reflect_stub.chmod(0o755)

    # qmd stub — emits text output with paths pointing at our seeded docs.
    qmd_stub = bin_dir / "qmd"
    qmd_stub.write_text(
        "#!/usr/bin/env bash\n"
        "cat <<'EOF'\n"
        "qmd://learnings/learnings/lrn-qmd-x.md:1 #aaa\n"
        "Title: lrn-qmd-x\n\n"
        "qmd://learnings/learnings/lrn-qmd-y.md:1 #bbb\n"
        "Title: lrn-qmd-y\n"
        "EOF\n"
    )
    qmd_stub.chmod(0o755)

    yield {
        "home": home,
        "bin": bin_dir,
        "reflect_state": tmp_path / "reflect_state",
    }


def test_fusion_includes_both_backends(dual_stub_kb):
    """With both backends stubbed, fused output must contain IDs from EACH."""
    env = os.environ.copy()
    env["HOME"] = str(dual_stub_kb["home"])
    env["REFLECT_STATE_DIR"] = str(dual_stub_kb["reflect_state"])
    # Prepend stub bin so our qmd stub wins lookup; keep system PATH so uv
    # can still find bash/coreutils when running the stubs.
    env["PATH"] = f"{dual_stub_kb['bin']}:{env.get('PATH', '')}"

    result = subprocess.run(
        [UV, "run", "--quiet", str(RECALL), "sandbox",
         "--limit", "10", "--format", "markdown", "--no-cache"],
        capture_output=True, text=True, timeout=60, env=env,
    )
    assert result.returncode == 0, f"recall failed: {result.stderr}"
    # Must see at least one ID from each backend → fusion is happening
    assert "lrn-graph-" in result.stdout, (
        f"graph backend results missing — recall isn't calling reflect CLI.\n"
        f"stdout: {result.stdout}"
    )
    assert "lrn-qmd-" in result.stdout, (
        f"qmd backend results missing — fusion isn't happening.\n"
        f"stdout: {result.stdout}"
    )


# ─── Layer 6: REAL CLI smoke tests (skipped if KB absent) ──────────────

def _real_reflect_available() -> bool:
    return shutil.which("reflect") is not None


def _real_qmd_available() -> bool:
    return shutil.which("qmd") is not None


@pytest.mark.skipif(not _real_reflect_available(),
                    reason="reflect CLI not on $PATH (install with `uv tool install reflect-kb`)")
def test_real_reflect_cli_returns_results():
    """Smoke test: the real reflect CLI actually returns something for a
    generic query. Catches if the local KB is empty or broken."""
    result = subprocess.run(
        ["reflect",
         "search", "the", "--mode", "naive", "--format", "json", "--limit", "3"],
        capture_output=True, text=True, timeout=120,
    )
    assert result.returncode == 0, (
        f"reflect CLI failed: {result.stderr[:500]}"
    )
    payload = json.loads(result.stdout)
    assert isinstance(payload, dict), f"expected envelope dict, got {type(payload)}"
    context = payload.get("context", "")
    assert context, "empty context — either KB is empty or CLI is broken"


@pytest.mark.skipif(not _real_qmd_available(),
                    reason="qmd CLI not installed on this host")
def test_real_qmd_cli_returns_results():
    """Smoke test: the real qmd CLI actually returns something for a
    generic keyword against the learnings collection."""
    result = subprocess.run(
        ["qmd", "search", "the", "-c", "learnings", "--limit", "3"],
        capture_output=True, text=True, timeout=15,
    )
    assert result.returncode == 0, f"qmd failed: {result.stderr[:500]}"
    # QMD search emits qmd:// path lines for hits, or "No results found." for empty
    assert "qmd://learnings/" in result.stdout or "No results" in result.stdout, (
        f"unexpected qmd output: {result.stdout[:500]}"
    )


if __name__ == "__main__":
    import sys
    sys.exit(pytest.main([__file__, "-v"]))
