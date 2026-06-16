#!/usr/bin/env python3
# ABOUTME: Deterministic generator for the synthetic eval corpus (docs + sidecars).
# ABOUTME: Output is committed; re-run only when deliberately evolving the corpus.
"""Generate the synthetic eval corpus.

Four query classes are engineered into the corpus:

- EXACT      unique terminology, keyword-matchable (vector/BM25 should nail these)
- GRAPH      entity-linked chains where the relevant doc is one hop away from
             the lexically-matching doc (proves the graph-expansion arm, R1)
- TEMPORAL   same topic, old vs new convention (proves recency/temporal ranking)
- OOD        queries with NO relevant docs (proves the OOD gate, R7)

Usage:  python3 make_corpus.py        # writes corpus/*.md + corpus/*.entities.yaml
"""
from __future__ import annotations

import pathlib

HERE = pathlib.Path(__file__).parent
CORPUS = HERE / "corpus"

# name, title, category, tags, confidence, created, key_insight, body,
# entities[(name,type,desc)], relationships[(src,tgt,type,desc,strength)]
DOCS = [
    # ---------------- EXACT class (8) ----------------
    dict(
        name="ex-01-tmux-socket-protection",
        title="Never kill the tmux server — sessions die irreversibly",
        category="tooling",
        tags=["tmux", "process-management", "safety"],
        confidence="high",
        created="2026-03-01",
        key_insight="Always kill tmux sessions by exact name; tmux kill-server destroys every session on the socket.",
        body="`tmux kill-server` and `pkill tmux` destroy all sessions across all projects. Use `tmux kill-session -t <exact-name>` only.",
        entities=[("tmux", "tool", "terminal multiplexer"), ("kill-server", "error", "destructive bulk kill command")],
        rels=[("kill-server", "tmux", "relates_to", "kill-server destroys all tmux sessions", 8)],
    ),
    dict(
        name="ex-02-pyenv-shim-ordering",
        title="pyenv shims must precede system python in PATH",
        category="environment",
        tags=["pyenv", "python", "path"],
        confidence="high",
        created="2026-02-15",
        key_insight="If system python wins PATH resolution, pyenv version pinning silently no-ops; put ~/.pyenv/shims first.",
        body="Symptom: `python --version` ignores `.python-version`. Root cause: PATH ordering. Fix: prepend pyenv shims in the shell rc.",
        entities=[("pyenv", "tool", "python version manager"), ("PATH-ordering", "concept", "shell resolution order")],
        rels=[("pyenv", "PATH-ordering", "requires", "shims must precede system binaries", 9)],
    ),
    dict(
        name="ex-03-zsh-glob-nomatch",
        title="zsh aborts scripts on unmatched globs — use null_glob or quote",
        category="shell",
        tags=["zsh", "globbing", "scripting"],
        confidence="high",
        created="2026-01-20",
        key_insight="zsh raises 'no matches found' and aborts where bash passes the literal pattern; set null_glob or quote the pattern.",
        body="`rm -f *.tmp` fails in zsh when no .tmp files exist. bash passes the literal; zsh errors. Fix: `setopt null_glob` or quote and test.",
        entities=[("zsh", "tool", "shell"), ("null-glob", "config", "zsh option for empty glob expansion")],
        rels=[("null-glob", "zsh", "configures", "controls unmatched glob behaviour", 7)],
    ),
    dict(
        name="ex-04-docker-buildkit-cache-mount",
        title="BuildKit cache mounts need explicit mode for non-root users",
        category="docker",
        tags=["docker", "buildkit", "caching"],
        confidence="medium",
        created="2026-04-02",
        key_insight="RUN --mount=type=cache defaults to root-owned; pass uid/gid/mode or chown in the same layer for non-root builds.",
        body="Non-root images fail with EACCES on the cache dir. The mount is root-owned by default.",
        entities=[("buildkit", "technology", "docker build engine"), ("cache-mount", "pattern", "persistent build cache")],
        rels=[("cache-mount", "buildkit", "part_of", "BuildKit mount feature", 6)],
    ),
    dict(
        name="ex-05-gh-graphql-pagination",
        title="gh api graphql pagination needs explicit cursor loops",
        category="github",
        tags=["gh-cli", "graphql", "pagination"],
        confidence="high",
        created="2026-03-18",
        key_insight="gh api graphql has no --paginate for nested connections; loop on pageInfo.endCursor manually.",
        body="--paginate only works for REST. GraphQL connections need a while loop reading hasNextPage/endCursor.",
        entities=[("gh-cli", "tool", "GitHub CLI"), ("cursor-pagination", "pattern", "GraphQL paging idiom")],
        rels=[("gh-cli", "cursor-pagination", "uses", "manual cursor loop for GraphQL", 7)],
    ),
    dict(
        name="ex-06-launchd-plist-env",
        title="launchd jobs don't inherit shell environment variables",
        category="macos",
        tags=["launchd", "macos", "environment"],
        confidence="high",
        created="2026-02-28",
        key_insight="launchd plists run with a minimal env; bake variables into EnvironmentVariables dict or wrap with a login shell.",
        body="A job that works in the terminal but fails under launchd is almost always missing PATH or HOME context.",
        entities=[("launchd", "platform", "macOS service manager"), ("environment-variables", "config", "process env")],
        rels=[("launchd", "environment-variables", "requires", "explicit env in plist", 8)],
    ),
    dict(
        name="ex-07-sqlite-wal-checkpoint",
        title="SQLite WAL grows unbounded without checkpointing",
        category="database",
        tags=["sqlite", "wal", "operations"],
        confidence="medium",
        created="2026-05-01",
        key_insight="Long-lived readers block WAL checkpoints; run PRAGMA wal_checkpoint(TRUNCATE) on a writer connection periodically.",
        body="A -wal file growing to GBs means checkpoint starvation. Close long readers or checkpoint explicitly.",
        entities=[("sqlite", "technology", "embedded database"), ("wal-checkpoint", "pattern", "write-ahead-log maintenance")],
        rels=[("wal-checkpoint", "sqlite", "configures", "bounds WAL growth", 7)],
    ),
    dict(
        name="ex-08-git-worktree-prune",
        title="Stale git worktree refs break new worktree creation",
        category="git",
        tags=["git", "worktree"],
        confidence="high",
        created="2026-04-20",
        key_insight="After deleting a worktree dir manually, run git worktree prune before reusing the branch.",
        body="'fatal: <branch> is already checked out' from a deleted dir means stale administrative files under .git/worktrees.",
        entities=[("git-worktree", "tool", "multiple working trees"), ("worktree-prune", "pattern", "stale ref cleanup")],
        rels=[("worktree-prune", "git-worktree", "solves", "clears stale checkout refs", 8)],
    ),
    # ---------------- GRAPH class (8 — two 4-doc chains) ----------------
    dict(
        name="gr-01-redis-pool-exhaustion",
        title="Redis connection pool exhaustion under burst load",
        category="infrastructure",
        tags=["redis", "connection-pool", "scaling"],
        confidence="high",
        created="2026-03-10",
        key_insight="Default pool of 10 connections exhausts under burst; size the pool to peak concurrency and add a wait timeout.",
        body="Symptom: intermittent ConnectionError under load while Redis itself is healthy. The client pool is the bottleneck.",
        entities=[("redis-connection-pool", "technology", "client-side connection pool"), ("burst-load", "concept", "traffic spike pattern")],
        rels=[("burst-load", "redis-connection-pool", "relates_to", "bursts exhaust small pools", 8)],
    ),
    dict(
        name="gr-02-api-gateway-timeout-cascade",
        title="Gateway 504 cascade traced to upstream pool waits",
        category="infrastructure",
        tags=["gateway", "timeout", "cascade"],
        confidence="high",
        created="2026-03-12",
        key_insight="Gateway 504s during spikes were queued waits on the redis pool, not slow handlers — fix the pool, not the timeout.",
        body="Tempting to raise gateway timeouts; the real fault was connection wait time upstream.",
        entities=[("gateway-timeout", "error", "504 from edge proxy"), ("redis-connection-pool", "technology", "client-side connection pool")],
        rels=[("gateway-timeout", "redis-connection-pool", "caused_by", "pool waits surface as 504s", 9)],
    ),
    dict(
        name="gr-03-circuit-breaker-rollout",
        title="Circuit breaker stops gateway cascade amplification",
        category="architecture",
        tags=["circuit-breaker", "resilience"],
        confidence="medium",
        created="2026-03-20",
        key_insight="A breaker in front of the flaky dependency converts cascade failures into fast, bounded degradation.",
        body="Half-open probes restore service automatically once the dependency recovers.",
        entities=[("circuit-breaker", "pattern", "resilience pattern"), ("gateway-timeout", "error", "504 from edge proxy")],
        rels=[("circuit-breaker", "gateway-timeout", "prevents", "bounds cascade blast radius", 8)],
    ),
    dict(
        name="gr-04-nginx-keepalive-upstream",
        title="nginx upstream keepalive needs explicit Connection header",
        category="infrastructure",
        tags=["nginx", "keepalive", "http"],
        confidence="medium",
        created="2026-03-25",
        key_insight="Set proxy_http_version 1.1 and clear the Connection header or upstream keepalive silently never engages.",
        body="Without it every request opens a fresh upstream TCP connection, magnifying timeout pressure.",
        entities=[("nginx", "technology", "reverse proxy"), ("gateway-timeout", "error", "504 from edge proxy")],
        rels=[("nginx", "gateway-timeout", "relates_to", "connection churn worsens timeouts", 6)],
    ),
    dict(
        name="gr-05-celery-prefetch-starvation",
        title="Celery prefetch multiplier starves long-task queues",
        category="queues",
        tags=["celery", "prefetch", "queues"],
        confidence="high",
        created="2026-04-05",
        key_insight="prefetch_multiplier>1 lets one worker hoard tasks it can't run; set to 1 for long-running task queues.",
        body="Queue shows pending tasks while workers idle: hoarded prefetched messages on a busy worker.",
        entities=[("celery-prefetch", "config", "worker prefetch multiplier"), ("task-starvation", "error", "idle workers with queued tasks")],
        rels=[("task-starvation", "celery-prefetch", "caused_by", "hoarding via prefetch", 9)],
    ),
    dict(
        name="gr-06-worker-memory-leak",
        title="Worker RSS creep traced to prefetched task payloads",
        category="queues",
        tags=["memory", "worker", "celery"],
        confidence="medium",
        created="2026-04-08",
        key_insight="Prefetched payloads held in worker memory between task runs leak RSS; max-tasks-per-child recycles cleanly.",
        body="RSS grows linearly with prefetched backlog. Recycling workers bounds it.",
        entities=[("worker-memory-leak", "error", "RSS creep in workers"), ("celery-prefetch", "config", "worker prefetch multiplier")],
        rels=[("worker-memory-leak", "celery-prefetch", "caused_by", "payload hoarding leaks memory", 8)],
    ),
    dict(
        name="gr-07-k8s-oomkill-debugging",
        title="OOMKilled pods: read the cgroup peak, not the pod limit",
        category="kubernetes",
        tags=["kubernetes", "oom", "debugging"],
        confidence="high",
        created="2026-04-12",
        key_insight="kubectl describe shows the limit, not the spike; memory.peak in the cgroup explains which burst killed the pod.",
        body="Exit code 137 with healthy averages means a short spike — find it in cgroup v2 memory.peak.",
        entities=[("oomkill", "error", "kernel OOM pod kill"), ("worker-memory-leak", "error", "RSS creep in workers")],
        rels=[("oomkill", "worker-memory-leak", "relates_to", "leaks end as OOM kills", 7)],
    ),
    dict(
        name="gr-08-grafana-alert-tuning",
        title="Alert on memory slope, not absolute threshold",
        category="observability",
        tags=["grafana", "alerting", "memory"],
        confidence="medium",
        created="2026-04-15",
        key_insight="Slope-based alerts (deriv over 30m) catch leaks days before any absolute threshold fires.",
        body="Absolute thresholds fire at 2am when it's too late; the slope was visible for days.",
        entities=[("grafana-alerting", "tool", "alert rules"), ("oomkill", "error", "kernel OOM pod kill")],
        rels=[("grafana-alerting", "oomkill", "prevents", "early slope detection avoids kills", 6)],
    ),
    # ---------------- TEMPORAL class (4 — two topics, old vs new) ----------------
    dict(
        name="tp-01-auth-jwt-convention",
        title="Auth convention: stateless JWT with RS256",
        category="conventions",
        tags=["auth", "jwt", "convention"],
        confidence="medium",
        created="2025-08-10",
        archived="2025-08-10T00:00:00",
        key_insight="Services authenticate via stateless RS256 JWTs minted by the identity service.",
        body="SUPERSEDED in practice by session-based auth (see the 2026 convention).",
        entities=[("jwt-auth", "pattern", "stateless token auth"), ("auth-convention", "concept", "team auth standard")],
        rels=[("jwt-auth", "auth-convention", "part_of", "the 2025 standard", 5)],
    ),
    dict(
        name="tp-02-auth-session-convention",
        title="Auth convention: server-side sessions replace JWT",
        category="conventions",
        tags=["auth", "sessions", "convention"],
        confidence="high",
        created="2026-05-15",
        archived="2026-05-15T00:00:00",
        key_insight="As of 2026-05 the team standard is server-side sessions with a shared session store; JWTs are deprecated for first-party auth.",
        body="Revocation pain and key-rotation incidents drove the move off JWTs. This supersedes the 2025 JWT convention.",
        entities=[("session-auth", "pattern", "server-side session auth"), ("auth-convention", "concept", "team auth standard")],
        rels=[("session-auth", "auth-convention", "part_of", "the current standard", 9),
              ("session-auth", "jwt-auth", "supersedes", "replaces stateless JWTs", 9)],
    ),
    dict(
        name="tp-03-deploy-heroku-process",
        title="Deploy process: Heroku pipelines with review apps",
        category="conventions",
        tags=["deploy", "heroku", "convention"],
        confidence="medium",
        created="2025-06-01",
        archived="2025-06-01T00:00:00",
        key_insight="Deploys ride Heroku pipelines; review apps per PR.",
        body="Legacy process before the Fly.io migration.",
        entities=[("heroku-deploy", "platform", "legacy deploy target"), ("deploy-process", "concept", "team deploy standard")],
        rels=[("heroku-deploy", "deploy-process", "part_of", "the 2025 process", 5)],
    ),
    dict(
        name="tp-04-deploy-fly-process",
        title="Deploy process: Fly.io machines via GitHub Actions",
        category="conventions",
        tags=["deploy", "fly", "convention"],
        confidence="high",
        created="2026-04-01",
        archived="2026-04-01T00:00:00",
        key_insight="Current deploys go to Fly.io machines from GitHub Actions on merge to main; Heroku is decommissioned.",
        body="Supersedes the Heroku pipeline process from 2025.",
        entities=[("fly-deploy", "platform", "current deploy target"), ("deploy-process", "concept", "team deploy standard")],
        rels=[("fly-deploy", "deploy-process", "part_of", "the current process", 9),
              ("fly-deploy", "heroku-deploy", "supersedes", "replaces Heroku pipelines", 9)],
    ),
]


def doc_md(d: dict) -> str:
    lines = [
        "---",
        f"name: {d['name']}",
        f'title: "{d["title"]}"',
        f"category: {d['category']}",
        "tags:",
        *[f"  - {t}" for t in d["tags"]],
        f"confidence: {d['confidence']}",
        f'created: "{d["created"]}"',
        f'key_insight: "{d["key_insight"]}"',
        "---",
        "",
    ]
    fm = "\n".join(lines)
    archived = f"<!-- archived: {d['archived']} -->\n\n" if d.get("archived") else ""
    return fm + archived + f"## Learning\n\n{d['body']}\n\n**How to apply:** {d['key_insight']}\n"


def sidecar_yaml(d: dict) -> str:
    ents = "\n".join(
        f'  - name: "{n}"\n    type: {t}\n    description: "{desc}"' for n, t, desc in d["entities"]
    )
    rels = "\n".join(
        f'  - source: "{s}"\n    target: "{t}"\n    type: {ty}\n    description: "{desc}"\n    strength: {st}'
        for s, t, ty, desc, st in d["rels"]
    )
    return (
        f"document_id: {d['name']}\n"
        f"extracted_at: '{d['created']}T00:00:00'\n"
        f"entities:\n{ents}\n"
        f"relationships:\n{rels}\n"
    )


def main() -> None:
    CORPUS.mkdir(parents=True, exist_ok=True)
    for d in DOCS:
        (CORPUS / f"{d['name']}.md").write_text(doc_md(d))
        (CORPUS / f"{d['name']}.entities.yaml").write_text(sidecar_yaml(d))
    print(f"wrote {len(DOCS)} docs + sidecars to {CORPUS}")


if __name__ == "__main__":
    main()
