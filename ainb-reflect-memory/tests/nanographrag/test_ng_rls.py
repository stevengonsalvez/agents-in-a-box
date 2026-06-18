# ABOUTME: RLS fail-closed + per-workspace isolation for the Phase 2 ng_ tables,
# ABOUTME: exercising the direct (PostgREST/JWT) path via an unprivileged role.

from __future__ import annotations

import pytest

pytestmark = pytest.mark.integration

WS_A = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
WS_B = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"


def test_ng_kv_rls_isolates_by_workspace_guc(clean) -> None:
    import psycopg

    # Seed two tenants as the table owner (RLS does not apply to owners).
    owner = psycopg.connect(clean, autocommit=True)
    with owner.cursor() as cur:
        for ws, val in ((WS_A, "secret in A"), (WS_B, "secret in B")):
            cur.execute(
                "insert into reflect_memory.ng_kv (workspace_id, namespace, key, value) "
                "values (%s,'full_docs','d1', %s::jsonb)",
                (ws, f'{{"content":"{val}"}}'),
            )

        # Clean unprivileged role — RLS applies to it.
        cur.execute(
            "do $$ begin "
            "  if exists (select 1 from pg_roles where rolname='ng_rls_test') then "
            "    execute 'drop owned by ng_rls_test'; execute 'drop role ng_rls_test'; "
            "  end if; end $$;"
        )
        cur.execute("create role ng_rls_test nologin;")
        cur.execute("grant usage on schema reflect_memory to ng_rls_test;")
        cur.execute("grant select on reflect_memory.ng_kv to ng_rls_test;")
        cur.execute(
            "grant execute on function reflect_memory.current_workspace_id() to ng_rls_test;"
        )

        cur.execute("set role ng_rls_test;")
        # No workspace resolvable -> deny all (fail closed).
        cur.execute("select count(*) from reflect_memory.ng_kv;")
        assert cur.fetchone()[0] == 0
        # Scope to A.
        cur.execute("select set_config('app.current_workspace', %s, false);", (WS_A,))
        cur.execute("select value->>'content' as c from reflect_memory.ng_kv order by c;")
        assert [r[0] for r in cur.fetchall()] == ["secret in A"]
        # Scope to B.
        cur.execute("select set_config('app.current_workspace', %s, false);", (WS_B,))
        cur.execute("select value->>'content' as c from reflect_memory.ng_kv order by c;")
        assert [r[0] for r in cur.fetchall()] == ["secret in B"]
        cur.execute("reset role;")
    owner.close()


def test_jwt_claim_wins_over_guc(clean) -> None:
    """The signed JWT workspace_id is AUTHORITATIVE: an attacker who sets the
    app.current_workspace GUC cannot override their JWT tenant. Regression guard
    for the resolver's JWT-first precedence (closes the GUC trust-inversion)."""
    import psycopg

    owner = psycopg.connect(clean, autocommit=True)
    with owner.cursor() as cur:
        for ws, val in ((WS_A, "secret in A"), (WS_B, "secret in B")):
            cur.execute(
                "insert into reflect_memory.ng_kv (workspace_id, namespace, key, value) "
                "values (%s,'full_docs','d1', %s::jsonb)",
                (ws, f'{{"content":"{val}"}}'),
            )
        cur.execute(
            "do $$ begin "
            "  if exists (select 1 from pg_roles where rolname='ng_rls_test') then "
            "    execute 'drop owned by ng_rls_test'; execute 'drop role ng_rls_test'; "
            "  end if; end $$;"
        )
        cur.execute("create role ng_rls_test nologin;")
        cur.execute("grant usage on schema reflect_memory to ng_rls_test;")
        cur.execute("grant select on reflect_memory.ng_kv to ng_rls_test;")
        cur.execute(
            "grant execute on function reflect_memory.current_workspace_id() to ng_rls_test;"
        )
        cur.execute("set role ng_rls_test;")

        # JWT pins tenant A; attacker also sets the GUC to tenant B.
        cur.execute(
            "select set_config('request.jwt.claims', %s, false);", ('{"workspace_id":"%s"}' % WS_A,)
        )
        cur.execute("select set_config('app.current_workspace', %s, false);", (WS_B,))
        cur.execute("select value->>'content' as c from reflect_memory.ng_kv;")
        rows = [r[0] for r in cur.fetchall()]
        assert rows == ["secret in A"], f"JWT must win over GUC, got {rows}"

        # JWT present but no workspace_id -> deny, even with the GUC set to B.
        cur.execute("select set_config('request.jwt.claims', %s, false);", ('{"sub":"u1"}',))
        cur.execute("select count(*) as n from reflect_memory.ng_kv;")
        assert cur.fetchone()[0] == 0, "JWT without workspace_id must deny, not fall back to GUC"

        cur.execute("reset role;")
        cur.execute("select set_config('request.jwt.claims', '', false);")
    owner.close()
