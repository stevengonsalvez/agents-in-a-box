// ABOUTME: Merge sessions from multiple sources, deduping by cwd.
//
// cwd is the primary identity. Pid-asymmetric sources (ainb publishes no
// pid; peers does) used to land in different buckets in the TS reference
// impl — this Rust port coalesces on cwd to fix that. Two claude sessions
// in the same cwd is a rare edge case; accept the merge.

use std::collections::BTreeMap;

use crate::fleet::types::Session;

#[must_use]
pub fn merge_sessions(groups: Vec<Vec<Session>>) -> Vec<Session> {
    let mut by_key: BTreeMap<String, Session> = BTreeMap::new();
    for group in groups {
        for s in group {
            let key = dedupe_key(&s);
            match by_key.remove(&key) {
                Some(existing) => {
                    by_key.insert(key, merge_one(existing, s));
                }
                None => {
                    by_key.insert(key, s);
                }
            }
        }
    }
    by_key.into_values().collect()
}

fn dedupe_key(s: &Session) -> String {
    if s.cwd.is_empty() {
        s.id.clone()
    } else {
        s.cwd.clone()
    }
}

fn merge_one(a: Session, b: Session) -> Session {
    let mut sources = a.sources;
    for src in b.sources {
        if !sources.contains(&src) {
            sources.push(src);
        }
    }
    Session {
        id: a.id,
        cwd: a.cwd,
        pid: a.pid.or(b.pid),
        git_root: a.git_root.or(b.git_root),
        tmux_session: a.tmux_session.or(b.tmux_session),
        workspace_name: a.workspace_name.or(b.workspace_name),
        worktree_path: a.worktree_path.or(b.worktree_path),
        peer_id: a.peer_id.or(b.peer_id),
        bg_job_id: a.bg_job_id.or(b.bg_job_id),
        transcript_path: a.transcript_path.or(b.transcript_path),
        sources,
        summary: a.summary.or(b.summary),
        last_seen_ms: match (a.last_seen_ms, b.last_seen_ms) {
            (Some(x), Some(y)) => Some(x.max(y)),
            (x, y) => x.or(y),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::types::SessionSource;

    fn session(cwd: &str, src: SessionSource, pid: Option<u32>) -> Session {
        Session {
            id: format!("{cwd}-{src:?}"),
            cwd: cwd.to_string(),
            pid,
            git_root: None,
            tmux_session: None,
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![src],
            summary: None,
            last_seen_ms: None,
        }
    }

    #[test]
    fn merges_same_cwd_across_sources() {
        let a = session("/repo", SessionSource::Ainb, None);
        let b = session("/repo", SessionSource::Peers, Some(123));
        let merged = merge_sessions(vec![vec![a], vec![b]]);
        assert_eq!(merged.len(), 1);
        let m = &merged[0];
        assert_eq!(m.cwd, "/repo");
        assert_eq!(m.pid, Some(123));
        assert_eq!(m.sources.len(), 2);
    }

    #[test]
    fn keeps_distinct_cwds() {
        let a = session("/r1", SessionSource::Ainb, None);
        let b = session("/r2", SessionSource::Peers, Some(1));
        let merged = merge_sessions(vec![vec![a], vec![b]]);
        assert_eq!(merged.len(), 2);
    }
}
