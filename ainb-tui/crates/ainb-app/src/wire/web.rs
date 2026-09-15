// ABOUTME: The web dashboard's session rows, projected from the redacted
// Sessions section frame. The browser never sees a session field that did not
// first pass through `section_json` (issue #1056).
//
//   AppState.sessions ──Frame::new──▶ redacted body ──session_rows──▶ /api/snapshot.sessions[]
//
// The projection only picks and renames values already in the frame body, so
// whatever the frame withholds (`display_name`) or scrubs cannot reappear on
// the web. `wire::shape` traces these rows into the committed key-path
// fixture, so a new row field fails the same gate a new frame field does.

use crate::app::AppState;
use crate::app::versioned::SectionId;
use crate::wire::frame::Frame;
use serde::Serialize;
use serde_json::Value;

/// One row of the web dashboard's session list, in the shape `frontend/app.js`
/// draws: the keys `ainb list --format json` has always used, minus the label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebSessionRow {
    pub session_id: String,
    pub tmux_session_name: Option<String>,
    pub workspace_name: String,
    pub worktree_path: String,
    pub created_at: String,
    /// The tmux session exists.
    pub is_running: bool,
    /// The agent is running inside it.
    pub claude_active: bool,
}

/// The web rows for every session of `state`, read from its Sessions frame.
#[must_use]
pub fn session_rows(state: &AppState) -> Vec<WebSessionRow> {
    rows_from_frame(&Frame::new(state, SectionId::Sessions))
}

/// The web rows a Sessions frame describes. Any other section's frame has no
/// workspaces and gives no rows.
#[must_use]
pub fn rows_from_frame(frame: &Frame) -> Vec<WebSessionRow> {
    let text = |value: &Value| value.as_str().map(str::to_string);
    frame.body()["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|workspace| {
            let workspace_name = text(&workspace["name"]).unwrap_or_default();
            workspace["sessions"].as_array().into_iter().flatten().map(move |session| {
                // `Running` and `Idle` have a live tmux session; `Stopped` and
                // `Error(..)` do not.
                let status = session["status"].as_str();
                WebSessionRow {
                    session_id: text(&session["id"]).unwrap_or_default(),
                    tmux_session_name: text(&session["tmux_session_name"]),
                    workspace_name: workspace_name.clone(),
                    worktree_path: text(&session["workspace_path"]).unwrap_or_default(),
                    created_at: text(&session["created_at"]).unwrap_or_default(),
                    is_running: matches!(status, Some("Running" | "Idle")),
                    claude_active: status == Some("Running"),
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SessionStatus;
    use crate::wire::shape::{PlainSeed, sample_state};

    const CANARY: &str = "ghp_ProofCanary0123456789abcdefghijklmnopq";

    #[test]
    fn a_credential_shaped_label_never_reaches_the_web_rows() {
        let mut state = sample_state(&mut PlainSeed);
        let session = &mut state.sessions.get_mut().workspaces[0].sessions[0];
        session.display_name = Some(format!("deploy {CANARY}"));
        session.tmux_session_name = Some("tmux_repo-1".to_string());

        let rows = session_rows(&state);
        let json = serde_json::to_string(&rows).expect("rows serialise");

        assert_eq!(rows.len(), 1);
        assert!(!json.contains(CANARY), "{json}");
        assert!(!json.contains("display_name"), "{json}");
        assert_eq!(rows[0].workspace_name, "sample-repo");
        assert_eq!(rows[0].tmux_session_name.as_deref(), Some("tmux_repo-1"));
        assert_eq!(rows[0].worktree_path, "/work/sample-repo");
    }

    #[test]
    fn the_session_status_decides_running_and_agent_active() {
        let mut state = sample_state(&mut PlainSeed);
        let expect = [
            (SessionStatus::Running, true, true),
            (SessionStatus::Idle, true, false),
            (SessionStatus::Stopped, false, false),
            (SessionStatus::Error("pane gone".to_string()), false, false),
        ];
        for (status, running, active) in expect {
            state.sessions.get_mut().workspaces[0].sessions[0].status = status.clone();
            let row = &session_rows(&state)[0];
            assert_eq!(
                (row.is_running, row.claude_active),
                (running, active),
                "{status:?}"
            );
        }
    }
}
