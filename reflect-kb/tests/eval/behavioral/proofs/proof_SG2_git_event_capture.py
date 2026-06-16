# ABOUTME: Behavioral proof for SG2 — reflect_db.record_commit captures a real
# ABOUTME: git commit into commit_links + commits.jsonl and a revert demotes the
# ABOUTME: reverted commit's session learnings (is_latest=0), with no LLM.
"""SG2 git-event capture proof.

Port SG2 is a SIGNAL/STORAGE port (surface=signal/storage). It ships the
post-commit capture path: ``plugins/reflect/hooks/post_commit.sh`` shells out to
``reflect_db.record_commit``, which (1) writes a ``commit_links`` row linking the
SHA to the active session, (2) appends a ``commits.jsonl`` line beside the DB,
and (3) on a ``git revert`` demotes the reverted commit's session learnings
(is_latest=0 — "contradicted by revert"). The signal is produced entirely at
*capture* time; ``recall.py`` and the GraphRAG engine never reference it, so
there is nothing to rank and the behavioral_kb retrieval fixture is the wrong
surface. This proof drives the REAL ``reflect_db`` module directly (no mock of
the thing under test, no torch engine, no network) against a throwaway git repo,
and NO LLM runs in any assertion — the commits + the module fully determine the
verdict.

Isolation: each test points ``REFLECT_DB_PATH`` + ``REFLECT_STATE_DIR`` at a
fresh tmp dir (so commits.jsonl and the DB are hermetic) and resets the
reflect_config + reflect_db connection caches, then builds a disposable git repo
under tmp.

Invariants (each arm's seed + the module fully determine the verdict — no LLM):

  A. NORMAL COMMIT CAPTURES ONE ROW + ONE LINE. Driving ``record_commit`` with a
     real commit's SHA writes exactly one ``commit_links`` row (sha<->session_id
     linkage correct) AND appends exactly one ``commits.jsonl`` line whose
     ``{sid, sha, branch, message, files, ts}`` round-trips the inputs. Capture
     is idempotent: re-capturing the same SHA writes NOTHING new.

  B. REVERT FLIPS PRIOR SESSION LEARNINGS is_latest=0. A learning created in
     session S (is_latest=1) is demoted to is_latest=0 with a revert
     ``revert_reason`` the moment a commit whose body says
     ``This reverts commit <sha>.`` (for a SHA captured in session S) is
     recorded. The decisive negative: a learning in a DIFFERENT session is left
     is_latest=1 — the demotion targets the reverted commit's session, not all
     sessions. A ``contradicted_by_revert`` audit event is written.

  C. NON-COMMIT / EMPTY SHA YIELDS NOTHING. Recording an empty SHA captures no
     row, appends no line, and demotes nothing — the gate is a real SHA, not
     incidental I/O.

Falsifiability: if capture wrote zero or two rows/lines, arm A fails. If the
revert demoted by SHA-presence-only (not session), arm B's cross-session
learning would also flip and the "untouched" assertion FAILS. If an empty SHA
still wrote a row, arm C fails.

PORT: SG2
"""
from __future__ import annotations

import importlib
import json
import subprocess
import sys
from pathlib import Path

import pytest

# Import the REAL reflect_db module from the plugin (not a copy). Path
# resolution mirrors proof_C4_lifecycle_events.py: parents[3] of the behavioral
# dir is the repo root where plugins/ sits alongside reflect-kb/.
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]
_PLUGIN_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next(
    (p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0]
)
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))


# --- helpers -------------------------------------------------------------

def _fresh_db(monkeypatch, tmp_path: Path):
    """Point reflect at an isolated tmp DB + state dir and return reflect_db.

    Resets the config + connection caches so the env override actually takes,
    then re-imports reflect_db so module-level config is rebound to the tmp DB.
    """
    db_path = tmp_path / "reflect.db"
    monkeypatch.setenv("REFLECT_DB_PATH", str(db_path))
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))

    import reflect_config

    reflect_config.load_config(force_reload=True)
    reflect_db = importlib.import_module("reflect_db")
    reflect_db.close_all()
    importlib.reload(reflect_db)
    reflect_db.close_all()
    return reflect_db


def _git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args],
        cwd=repo,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()


def _init_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q")
    _git(repo, "config", "user.email", "t@t.t")
    _git(repo, "config", "user.name", "t")
    _git(repo, "config", "commit.gpgsign", "false")
    return repo


def _commit(repo: Path, name: str, body: str, subject: str) -> str:
    (repo / name).write_text(body)
    _git(repo, "add", name)
    _git(repo, "commit", "-q", "-m", subject)
    return _git(repo, "rev-parse", "HEAD").lower()


def _read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(ln) for ln in path.read_text().splitlines() if ln.strip()]


# --- arm A: normal commit captures one row + one jsonl line ---------------

def test_normal_commit_captures_one_row_and_line(monkeypatch, tmp_path):
    """A real commit's SHA -> exactly one commit_links row (sha<->session) AND
    exactly one commits.jsonl line, both round-tripping the inputs. Re-capture
    is idempotent (writes nothing new)."""
    R = _fresh_db(monkeypatch, tmp_path)
    repo = _init_repo(tmp_path)
    sha = _commit(repo, "a.py", "x = 1\n", "feat: add a")
    files = _git(repo, "diff-tree", "--no-commit-id", "--name-only", "-r", sha).split()

    result = R.record_commit(
        sha,
        session_id="sess-A",
        branch="feat/a",
        message="feat: add a",
        files=files,
    )
    assert result["captured"] is True
    assert result["is_revert"] is False

    # commit_links row: SHA <-> session linkage correct.
    link = R.get_commit_link(sha)
    assert link is not None
    assert link["sha"] == sha
    assert link["session_id"] == "sess-A"
    assert link["branch"] == "feat/a"

    # commits.jsonl: exactly one line, round-tripping the inputs.
    lines = _read_jsonl(tmp_path / "commits.jsonl")
    assert len(lines) == 1, lines
    rec = lines[0]
    assert rec["sha"] == sha
    assert rec["sid"] == "sess-A"
    assert rec["branch"] == "feat/a"
    assert rec["message"] == "feat: add a"
    assert rec["files"] == files
    assert "ts" in rec

    # Idempotent re-capture: no second row, no second line.
    again = R.record_commit(sha, session_id="sess-A", branch="feat/a", message="feat: add a")
    assert again["captured"] is False
    assert len(_read_jsonl(tmp_path / "commits.jsonl")) == 1


# --- arm B: revert flips the reverted commit's session learnings ----------

def test_revert_demotes_only_reverted_session_learnings(monkeypatch, tmp_path):
    """A learning in the reverted commit's session flips is_latest=1 -> 0 on
    revert; a learning in a DIFFERENT session is left untouched (the demotion
    targets the reverted commit's session, not every session)."""
    R = _fresh_db(monkeypatch, tmp_path)
    repo = _init_repo(tmp_path)

    # Capture an original commit in session S, with a learning from that session.
    orig_sha = _commit(repo, "a.py", "x = 1\n", "feat: add a")
    R.record_commit(orig_sha, session_id="sess-S", branch="feat/a", message="feat: add a")
    target_lid = R.add_learning("always use os.replace", session_id="sess-S", scope="project")
    other_lid = R.add_learning("prefer pathlib over os.path", session_id="sess-OTHER", scope="project")

    assert R.get_learning(target_lid)["is_latest"] == 1
    assert R.get_learning(other_lid)["is_latest"] == 1

    # A git revert of the original commit (git's own message shape).
    revert_subject = f'Revert "feat: add a"'
    revert_body = f'{revert_subject}\n\nThis reverts commit {orig_sha}.'
    _git(repo, "revert", "--no-edit", orig_sha)
    revert_sha = _git(repo, "rev-parse", "HEAD").lower()

    result = R.record_commit(
        revert_sha,
        session_id="sess-REV",
        branch="feat/a",
        message=revert_body,
    )
    assert result["is_revert"] is True
    assert result["reverted_sha"] == orig_sha
    assert target_lid in result["demoted_learning_ids"]

    # The reverted session's learning is demoted; the other session is untouched.
    assert R.get_learning(target_lid)["is_latest"] == 0
    assert R.get_learning(target_lid)["revert_reason"]
    assert R.get_learning(other_lid)["is_latest"] == 1  # decisive negative

    # A contradicted_by_revert audit event was written for the demoted learning.
    events = R.get_events_by_type(R.REVERT_CONTRADICTION_EVENT_TYPE)
    assert any(ev["learning_id"] == target_lid for ev in events)


def test_revert_parser_extracts_sha_from_body(monkeypatch, tmp_path):
    """The revert parser pulls the reverted SHA out of git's
    'This reverts commit <sha>.' body line, and returns None for a plain
    commit message (so an ordinary commit never triggers a revert)."""
    R = _fresh_db(monkeypatch, tmp_path)
    assert R.parse_reverted_sha(
        'Revert "feat: x"\n\nThis reverts commit deadbeef1234.'
    ) == "deadbeef1234"
    assert R.parse_reverted_sha("feat: a normal commit") is None
    assert R.parse_reverted_sha("") is None


# --- arm C: a non-commit (empty SHA) yields nothing -----------------------

def test_empty_sha_captures_nothing(monkeypatch, tmp_path):
    """Recording an empty SHA writes no commit_links row, appends no
    commits.jsonl line, and demotes nothing — the gate is a real SHA."""
    R = _fresh_db(monkeypatch, tmp_path)

    result = R.record_commit("", session_id="sess-A", branch="feat/a", message="")
    assert result["captured"] is False
    assert result["demoted_learning_ids"] == []

    assert R.get_commit_link("") is None
    assert _read_jsonl(tmp_path / "commits.jsonl") == []
