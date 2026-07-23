// ABOUTME: Merge Fleet observations using stable identity, never cwd.

use std::collections::BTreeMap;

use crate::fleet::types::{FleetSession, ManagementState, Session, SessionKey, TransportHealth};

#[must_use]
pub fn merge_sessions(groups: Vec<Vec<Session>>) -> Vec<Session> {
    let mut merged: Vec<Session> = Vec::new();
    for group in groups {
        for session in group {
            if let Some(index) = merged.iter().position(|current| same_identity(current, &session))
            {
                let current = merged.remove(index);
                merged.insert(index, merge_one(current, session));
            } else {
                merged.push(session);
            }
        }
    }
    merged
}

fn same_identity(a: &Session, b: &Session) -> bool {
    (!a.id.is_empty() && a.id == b.id)
        || matches!((&a.peer_id, &b.peer_id), (Some(x), Some(y)) if x == y)
        || matches!((&a.tmux_session, &b.tmux_session), (Some(x), Some(y)) if x == y)
        || matches!((&a.bg_job_id, &b.bg_job_id), (Some(x), Some(y)) if x == y)
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

/// Merge authoritative Fleet records by stable session key.
#[must_use]
pub fn merge_fleet_sessions(groups: Vec<Vec<FleetSession>>) -> Vec<FleetSession> {
    let mut by_key: BTreeMap<SessionKey, FleetSession> = BTreeMap::new();
    for group in groups {
        for session in group {
            match by_key.remove(&session.session_key) {
                Some(current) => {
                    by_key.insert(
                        session.session_key.clone(),
                        merge_fleet_one(current, session),
                    );
                }
                None => {
                    by_key.insert(session.session_key.clone(), session);
                }
            }
        }
    }
    by_key.into_values().collect()
}

fn merge_fleet_one(mut current: FleetSession, incoming: FleetSession) -> FleetSession {
    let incoming_is_fresher = incoming.confidence > current.confidence
        || (incoming.confidence == current.confidence
            && incoming.last_seen_ms.unwrap_or(i64::MIN)
                >= current.last_seen_ms.unwrap_or(i64::MIN));

    current.capabilities.extend(&incoming.capabilities);
    current.provenance.extend(incoming.provenance);
    current.provider_session_id = current.provider_session_id.or(incoming.provider_session_id);
    current.exact_tmux_target = current.exact_tmux_target.or(incoming.exact_tmux_target);
    current.pane_pid = current.pane_pid.or(incoming.pane_pid);
    current.process_start_fingerprint =
        current.process_start_fingerprint.or(incoming.process_start_fingerprint);
    current.first_seen_ms = match (current.first_seen_ms, incoming.first_seen_ms) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    current.last_seen_ms = match (current.last_seen_ms, incoming.last_seen_ms) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    current.version = current.version.max(incoming.version);

    if incoming.management == ManagementState::Managed {
        current.management = ManagementState::Managed;
    }
    if incoming.transport_health == TransportHealth::Healthy {
        current.transport_health = TransportHealth::Healthy;
    }
    if incoming_is_fresher {
        current.provider = incoming.provider;
        current.cwd = incoming.cwd;
        current.lifecycle = incoming.lifecycle;
        current.attention = incoming.attention;
        current.confidence = incoming.confidence;
        current.transport_health = incoming.transport_health;
    } else {
        current.confidence = current.confidence.max(incoming.confidence);
    }
    current
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
    fn keeps_same_cwd_sessions_distinct_without_strong_identity() {
        let a = session("/repo", SessionSource::Ainb, None);
        let b = session("/repo", SessionSource::Peers, Some(123));
        let merged = merge_sessions(vec![vec![a], vec![b]]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn keeps_distinct_cwds() {
        let a = session("/r1", SessionSource::Ainb, None);
        let b = session("/r2", SessionSource::Peers, Some(1));
        let merged = merge_sessions(vec![vec![a], vec![b]]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merges_exact_same_session_id_across_sources() {
        let mut a = session("/old", SessionSource::Ainb, None);
        let mut b = session("/new", SessionSource::Peers, Some(123));
        a.id = "provider-session-1".into();
        b.id = "provider-session-1".into();

        let merged = merge_sessions(vec![vec![a], vec![b]]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].pid, Some(123));
        assert_eq!(merged[0].sources.len(), 2);
    }
}
