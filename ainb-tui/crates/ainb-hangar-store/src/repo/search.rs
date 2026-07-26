//! Cross-entity command-palette search over a workspace (e38.13).
//!
//! The Hangar command palette (`Ctrl+P`) jumps across *all four* entity kinds —
//! issues, agents, skills, autopilots — which the per-screen `/` filter (one
//! screen's loaded rows) and the issue-only [`IssueRepo::search_ranked`] cannot.
//! [`cross_entity_search`] runs a workspace-scoped substring match over each
//! entity's human-readable field (issue title / agent name / skill name /
//! autopilot name), then ranks the hits **exact > prefix > substring**, breaking
//! ties by a stable kind order (issues, agents, skills, autopilots) then label.
//!
//! The match is computed in SQL (a `LIKE` per table, metachar-escaped so the
//! query is a literal substring) and the *strength* ranking in Rust (the exact /
//! prefix / substring distinction is a cheap string compare on the small matched
//! set, and keeps the four per-table queries identical). The result is a flat
//! [`SearchHit`] list the daemon maps onto the proto wire entries.
//!
//! [`IssueRepo::search_ranked`]: crate::repo::issue::IssueRepo::search_ranked

use sqlx::{Row, SqlitePool};

/// Which entity a [`SearchHit`] points at. The variant order is *also* the
/// tie-break ranking order (issues before agents before skills before
/// autopilots), so the palette ordering is deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SearchHitKind {
    /// An issue, matched on its title.
    Issue,
    /// An agent, matched on its name.
    Agent,
    /// A skill, matched on its name.
    Skill,
    /// An autopilot, matched on its name.
    Autopilot,
}

/// One cross-entity search hit: the entity kind, its workspace-local id, and the
/// human-readable label that matched (the field the palette renders + ranks on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The entity kind that matched.
    pub kind: SearchHitKind,
    /// The matched entity's workspace-local id.
    pub id: String,
    /// The human-readable label that matched (issue title / entity name).
    pub label: String,
}

/// Match strength of a label against the query: `3` exact, `2` prefix, `1`
/// substring, `0` no match. Both sides are lower-cased by the caller.
fn match_strength(label_lower: &str, query_lower: &str) -> u8 {
    if label_lower == query_lower {
        3
    } else if label_lower.starts_with(query_lower) {
        2
    } else {
        // `1` (substring) or `0` (no match); the caller's LIKE already
        // guarantees a substring, so in practice this is `1`.
        u8::from(label_lower.contains(query_lower))
    }
}

/// Escape the SQL `LIKE` metacharacters (`\`, `%`, `_`) in `term` so the value is
/// matched as a literal substring under an `ESCAPE '\'` clause. Escaping `\`
/// first is essential so the inserted escape chars aren't themselves re-escaped.
fn like_escape(term: &str) -> String {
    term.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Run one workspace-scoped substring query over `table`.`name_col`, returning
/// `(id, label)` rows whose `name_col` contains the (already-escaped) `pattern`.
///
/// `extra_where` is appended to the `WHERE` (e.g. `" AND archived = 0"` for
/// agents) — it is a static literal in every call site, never caller text, so no
/// injection surface. Used by [`cross_entity_search`] once per entity kind.
async fn like_rows(
    pool: &SqlitePool,
    table: &str,
    name_col: &str,
    extra_where: &str,
    workspace_id: &str,
    pattern: &str,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    let sql = format!(
        "SELECT id, {name_col} AS label FROM {table} \
         WHERE workspace_id = ?1 AND LOWER({name_col}) LIKE ?2 ESCAPE '\\'{extra_where} \
         ORDER BY {name_col}"
    );
    let rows = sqlx::query(&sql).bind(workspace_id).bind(pattern).fetch_all(pool).await?;
    rows.iter()
        .map(|r| {
            Ok((
                r.try_get::<String, _>("id")?,
                r.try_get::<String, _>("label")?,
            ))
        })
        .collect()
}

/// Ranked cross-entity search within `workspace_id`.
///
/// Matches the case-insensitive `query` substring against the issue title, agent
/// name, skill name, and autopilot name (archived agents are excluded — they are
/// hidden from the active surface). Hits are ranked **exact > prefix > substring**;
/// ties break by kind order ([`SearchHitKind`] declaration order) then label.
///
/// Workspace-scoped: a sibling tenant's matching entity is never returned. A blank
/// query matches nothing (never dump the whole workspace).
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if any per-table query fails.
pub async fn cross_entity_search(
    pool: &SqlitePool,
    workspace_id: &str,
    query: &str,
) -> Result<Vec<SearchHit>, sqlx::Error> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let query_lower = trimmed.to_lowercase();
    let pattern = format!("%{}%", like_escape(&query_lower));

    // One substring query per entity table; the kind tags the rows.
    let tables: [(SearchHitKind, &str, &str, &str); 4] = [
        (SearchHitKind::Issue, "issue", "title", ""),
        // Hidden `system` agents (migration 0050) never surface in the palette —
        // the same filter every roster/picker read carries (multica agent.sql:1258).
        (
            SearchHitKind::Agent,
            "agent",
            "name",
            " AND archived = 0 AND kind = 'user'",
        ),
        (SearchHitKind::Skill, "skill", "name", ""),
        (SearchHitKind::Autopilot, "autopilot", "name", ""),
    ];

    let mut hits: Vec<(u8, SearchHit)> = Vec::new();
    for (kind, table, name_col, extra_where) in tables {
        for (id, label) in
            like_rows(pool, table, name_col, extra_where, workspace_id, &pattern).await?
        {
            let strength = match_strength(&label.to_lowercase(), &query_lower);
            // The LIKE already guarantees a substring match, so strength >= 1;
            // the guard is defensive (a NUL-stripped collation edge would skip).
            if strength > 0 {
                hits.push((strength, SearchHit { kind, id, label }));
            }
        }
    }

    // Rank: strongest match first, then kind order, then label (case-insensitive).
    hits.sort_by(|(sa, a), (sb, b)| {
        sb.cmp(sa)
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    Ok(hits.into_iter().map(|(_, hit)| hit).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    async fn seed_ws(pool: &SqlitePool, ws: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(ws)
            .bind(ws)
            .bind(ws)
            .bind(1_000_i64)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
            .bind(format!("{ws}-user"))
            .bind(format!("{ws}@example.com"))
            .bind(1_000_i64)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn seed_issue(pool: &SqlitePool, ws: &str, id: &str, title: &str) {
        sqlx::query(
            "INSERT INTO issue (id, workspace_id, title, state, creator_type, creator_id, \
             created_at) VALUES (?, ?, ?, 'open', 'member', ?, 1000)",
        )
        .bind(id)
        .bind(ws)
        .bind(title)
        .bind(format!("{ws}-user"))
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_runtime(pool: &SqlitePool, ws: &str, id: &str) {
        sqlx::query(
            "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, \
             status) VALUES (?, ?, 'd1', 'claude', 'local', 'online')",
        )
        .bind(id)
        .bind(ws)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_agent(pool: &SqlitePool, ws: &str, id: &str, name: &str, archived: bool) {
        sqlx::query(
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id, \
             archived) VALUES (?, ?, ?, 'rt', 'workspace', ?, ?)",
        )
        .bind(id)
        .bind(ws)
        .bind(name)
        .bind(format!("{ws}-user"))
        .bind(i64::from(archived))
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_skill(pool: &SqlitePool, ws: &str, id: &str, name: &str) {
        sqlx::query("INSERT INTO skill (id, workspace_id, name) VALUES (?, ?, ?)")
            .bind(id)
            .bind(ws)
            .bind(name)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn seed_autopilot(pool: &SqlitePool, ws: &str, id: &str, name: &str, agent_id: &str) {
        sqlx::query(
            "INSERT INTO autopilot (id, workspace_id, agent_id, name, cron_expr, created_at) \
             VALUES (?, ?, ?, ?, '0 0 * * *', 1000)",
        )
        .bind(id)
        .bind(ws)
        .bind(agent_id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A query that matches one of every kind returns all four, ordered by
    /// match strength then kind order. Here every label is an exact match, so the
    /// tie-break is pure kind order: issue, agent, skill, autopilot.
    #[tokio::test]
    async fn search_spans_all_four_entity_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_runtime(pool, "ws-a", "rt").await;

        seed_issue(pool, "ws-a", "i1", "deploy").await;
        seed_agent(pool, "ws-a", "a1", "deploy", false).await;
        seed_skill(pool, "ws-a", "s1", "deploy").await;
        seed_autopilot(pool, "ws-a", "p1", "deploy", "a1").await;

        let hits = cross_entity_search(pool, "ws-a", "deploy").await.unwrap();
        let kinds: Vec<SearchHitKind> = hits.iter().map(|h| h.kind).collect();
        assert_eq!(
            kinds,
            [
                SearchHitKind::Issue,
                SearchHitKind::Agent,
                SearchHitKind::Skill,
                SearchHitKind::Autopilot,
            ],
            "all four kinds, ordered issue<agent<skill<autopilot on equal strength"
        );
    }

    /// Exact > prefix > substring ranking dominates kind order: a substring issue
    /// hit ranks BELOW an exact-match skill hit, even though issues precede skills
    /// in the kind tie-break.
    #[tokio::test]
    async fn search_ranks_exact_over_prefix_over_substring() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_runtime(pool, "ws-a", "rt").await;
        seed_agent(pool, "ws-a", "a1", "scout", false).await;

        // Query "api": issue is a substring hit (1), skill is a prefix hit (2),
        // autopilot is an exact hit (3).
        seed_issue(pool, "ws-a", "i1", "Refactor the api layer").await; // substring
        seed_skill(pool, "ws-a", "s1", "api-client").await; // prefix
        seed_autopilot(pool, "ws-a", "p1", "api", "a1").await; // exact

        let hits = cross_entity_search(pool, "ws-a", "api").await.unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(
            ids,
            ["p1", "s1", "i1"],
            "exact (autopilot) > prefix (skill) > substring (issue)"
        );
    }

    /// Search is workspace-scoped: a sibling tenant's matching entity is absent.
    #[tokio::test]
    async fn search_is_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_skill(pool, "ws-a", "s-a", "shared").await;
        seed_skill(pool, "ws-b", "s-b", "shared").await;

        let hits = cross_entity_search(pool, "ws-a", "shared").await.unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["s-a"], "only the owning tenant's entity is returned");
    }

    /// An archived agent is hidden from the palette (mirrors the active list).
    #[tokio::test]
    async fn search_excludes_archived_agents() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_runtime(pool, "ws-a", "rt").await;
        seed_agent(pool, "ws-a", "a-active", "scout", false).await;
        seed_agent(pool, "ws-a", "a-archived", "scout", true).await;

        let hits = cross_entity_search(pool, "ws-a", "scout").await.unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["a-active"], "archived agent is hidden");
    }

    /// A blank query matches nothing, and a non-matching query excludes every
    /// entity (never dump the workspace).
    #[tokio::test]
    async fn search_blank_and_nonmatch_are_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "i1", "anything").await;

        assert!(cross_entity_search(pool, "ws-a", "   ").await.unwrap().is_empty());
        assert!(cross_entity_search(pool, "ws-a", "").await.unwrap().is_empty());
        assert!(cross_entity_search(pool, "ws-a", "zzz-no-match").await.unwrap().is_empty());
    }

    /// LIKE metacharacters in the query match literally (`%` is not a wildcard).
    #[tokio::test]
    async fn search_escapes_like_metacharacters() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "i-pct", "cut load by 50% today").await;
        seed_issue(pool, "ws-a", "i-plain", "no percent here").await;

        let hits = cross_entity_search(pool, "ws-a", "50%").await.unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["i-pct"], "% is a literal, not a wildcard");
    }
}
