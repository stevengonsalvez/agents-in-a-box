//! P6.5 — the IO-free skill service the daemon RPC handlers build on.
//!
//! The value is the *invariant*: a [`WorkspaceId`] threads through every call.
//!
//! The five P6.5 skill RPCs (`hangar/skills_list`, `hangar/skill_get`,
//! `hangar/skills_sync`, `hangar/skill_attach`, `hangar/skill_detach`) share one
//! orchestration shape: resolve the subscribed workspace, then read or mutate the
//! workspace-scoped skill rows. That orchestration is captured here as a pure
//! [`SkillService`] over a [`SkillBackend`] trait, so:
//!
//! - the daemon wraps `sqlx`/`SkillRepo` in a concrete backend and gets the
//!   workspace-scoping discipline for free (the service never issues an
//!   unscoped lookup — every backend method takes the [`WorkspaceId`]);
//! - tests fake an in-memory backend and exercise the service logic with no
//!   database.
//!
//! This crate is IO-free (`reference_plugin_sdk_no_host_deps` spirit: no
//! `tokio`/`sqlx`/`ratatui` here), so the backend trait is **synchronous** over
//! already-fetched rows — the daemon's async sqlx calls happen in its backend
//! impl, outside this module. The service's value is the *invariant*: a
//! [`WorkspaceId`] threads through every call, so a handler physically cannot
//! forget to scope a by-id read or a junction mutation.

use crate::ids::{AgentId, SkillId, WorkspaceId};
use crate::skill::SkillWithFiles;

/// The workspace-scoped skill operations a backend must provide.
///
/// Every method takes the [`WorkspaceId`] explicitly: there is no by-id read or
/// mutation that trusts a bare ulid, which is the tenant-isolation contract the
/// secured `SkillRepo` (P6.1+4pe) enforces in SQL. The daemon's backend forwards
/// each call to the matching scoped `SkillRepo` method; the in-memory test
/// backend filters on `workspace_id` the same way.
pub trait SkillBackend {
    /// The backend's error type (sqlx error in the daemon; `Infallible`-ish in
    /// tests).
    type Error;

    /// List every skill in `workspace`, ordered by name.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] on a backend failure.
    fn list(&self, workspace: &WorkspaceId) -> Result<Vec<SkillWithFiles>, Self::Error>;

    /// Fetch one skill by id, scoped to `workspace` (a foreign id → `None`).
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] on a backend failure.
    fn get(
        &self,
        workspace: &WorkspaceId,
        skill: &SkillId,
    ) -> Result<Option<SkillWithFiles>, Self::Error>;

    /// Attach `skill` to `agent` within `workspace` (idempotent).
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] when either id is not in `workspace`
    /// (cross-workspace guard) or on a backend failure.
    fn attach(
        &mut self,
        workspace: &WorkspaceId,
        agent: &AgentId,
        skill: &SkillId,
    ) -> Result<(), Self::Error>;

    /// Detach `skill` from `agent` within `workspace` (idempotent).
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] when either id is not in `workspace`
    /// (cross-workspace guard) or on a backend failure.
    fn detach(
        &mut self,
        workspace: &WorkspaceId,
        agent: &AgentId,
        skill: &SkillId,
    ) -> Result<(), Self::Error>;
}

/// The pure skill service: orchestrates the workspace-scoped reads/mutations a
/// daemon RPC handler needs, over any [`SkillBackend`].
///
/// Stateless wrapper around a `&mut B`; carrying the backend by reference keeps
/// the service zero-cost and lets the daemon construct one per request.
pub struct SkillService<'a, B> {
    backend: &'a mut B,
}

impl<'a, B: SkillBackend> SkillService<'a, B> {
    /// Wrap a backend.
    pub const fn new(backend: &'a mut B) -> Self {
        Self { backend }
    }

    /// List the workspace's skills (the `hangar/skills_list` body).
    ///
    /// # Errors
    ///
    /// Propagates the backend error.
    pub fn list(&self, workspace: &WorkspaceId) -> Result<Vec<SkillWithFiles>, B::Error> {
        self.backend.list(workspace)
    }

    /// Fetch one skill's detail (the `hangar/skill_get` body), scoped to
    /// `workspace`.
    ///
    /// # Errors
    ///
    /// Propagates the backend error.
    pub fn get(
        &self,
        workspace: &WorkspaceId,
        skill: &SkillId,
    ) -> Result<Option<SkillWithFiles>, B::Error> {
        self.backend.get(workspace, skill)
    }

    /// Attach a skill to an agent within `workspace` (`hangar/skill_attach`).
    ///
    /// # Errors
    ///
    /// Propagates the backend error (including the cross-workspace guard).
    pub fn attach(
        &mut self,
        workspace: &WorkspaceId,
        agent: &AgentId,
        skill: &SkillId,
    ) -> Result<(), B::Error> {
        self.backend.attach(workspace, agent, skill)
    }

    /// Detach a skill from an agent within `workspace` (`hangar/skill_detach`).
    ///
    /// # Errors
    ///
    /// Propagates the backend error (including the cross-workspace guard).
    pub fn detach(
        &mut self,
        workspace: &WorkspaceId,
        agent: &AgentId,
        skill: &SkillId,
    ) -> Result<(), B::Error> {
        self.backend.detach(workspace, agent, skill)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::{SkillFileInput, SkillName};

    /// An in-memory backend proving the service threads `workspace` through every
    /// call — a skill in workspace A is invisible to a workspace-B read, and a
    /// cross-workspace attach is rejected.
    #[derive(Default)]
    struct MemBackend {
        /// `(workspace_id, skill)` rows.
        skills: Vec<SkillWithFiles>,
        /// `(workspace_id, agent_id, skill_id)` agent ownership (for the guard).
        agents: Vec<(String, String)>,
        /// `(agent_id, skill_id)` junction.
        links: Vec<(String, String)>,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum MemError {
        CrossWorkspace,
    }

    impl MemBackend {
        fn with_skill(mut self, ws: &str, id: &str, name: &str) -> Self {
            self.skills.push(SkillWithFiles {
                id: SkillId::from_str(id).unwrap(),
                workspace_id: ws.to_string(),
                name: SkillName::new(name).unwrap(),
                description: None,
                content: Some(format!("# {name}\n")),
                files: vec![SkillFileInput::new("SKILL.md", format!("# {name}\n"))],
            });
            self
        }
        fn with_agent(mut self, ws: &str, agent: &str) -> Self {
            self.agents.push((ws.to_string(), agent.to_string()));
            self
        }
        fn skill_in(&self, ws: &str, id: &str) -> bool {
            self.skills.iter().any(|s| s.workspace_id == ws && s.id.as_str() == id)
        }
        fn agent_in(&self, ws: &str, agent: &str) -> bool {
            self.agents.iter().any(|(w, a)| w == ws && a == agent)
        }
    }

    impl SkillBackend for MemBackend {
        type Error = MemError;

        fn list(&self, workspace: &WorkspaceId) -> Result<Vec<SkillWithFiles>, MemError> {
            let mut out: Vec<SkillWithFiles> = self
                .skills
                .iter()
                .filter(|s| s.workspace_id == workspace.as_str())
                .cloned()
                .collect();
            out.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
            Ok(out)
        }

        fn get(
            &self,
            workspace: &WorkspaceId,
            skill: &SkillId,
        ) -> Result<Option<SkillWithFiles>, MemError> {
            Ok(self
                .skills
                .iter()
                .find(|s| s.workspace_id == workspace.as_str() && s.id.as_str() == skill.as_str())
                .cloned())
        }

        fn attach(
            &mut self,
            workspace: &WorkspaceId,
            agent: &AgentId,
            skill: &SkillId,
        ) -> Result<(), MemError> {
            if !self.agent_in(workspace.as_str(), agent.as_str())
                || !self.skill_in(workspace.as_str(), skill.as_str())
            {
                return Err(MemError::CrossWorkspace);
            }
            let pair = (agent.as_str().to_string(), skill.as_str().to_string());
            if !self.links.contains(&pair) {
                self.links.push(pair);
            }
            Ok(())
        }

        fn detach(
            &mut self,
            workspace: &WorkspaceId,
            agent: &AgentId,
            skill: &SkillId,
        ) -> Result<(), MemError> {
            if !self.agent_in(workspace.as_str(), agent.as_str())
                || !self.skill_in(workspace.as_str(), skill.as_str())
            {
                return Err(MemError::CrossWorkspace);
            }
            self.links.retain(|(a, s)| !(a == agent.as_str() && s == skill.as_str()));
            Ok(())
        }
    }

    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::from_str(id).unwrap()
    }

    #[test]
    fn list_is_workspace_scoped() {
        let mut backend = MemBackend::default()
            .with_skill("ws-a", "skill-a", "commit")
            .with_skill("ws-b", "skill-b", "review");
        let svc = SkillService::new(&mut backend);
        let a = svc.list(&ws("ws-a")).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].id.as_str(), "skill-a");
    }

    #[test]
    fn get_foreign_workspace_returns_none() {
        let mut backend = MemBackend::default().with_skill("ws-a", "skill-a", "commit");
        let svc = SkillService::new(&mut backend);
        let got = svc.get(&ws("ws-b"), &SkillId::from_str("skill-a").unwrap()).unwrap();
        assert!(
            got.is_none(),
            "a foreign-workspace get must resolve to None"
        );
    }

    #[test]
    fn attach_and_detach_are_idempotent_and_scoped() {
        let mut backend = MemBackend::default()
            .with_skill("ws-a", "skill-a", "commit")
            .with_agent("ws-a", "agent-a");
        let agent = AgentId::from_str("agent-a").unwrap();
        let skill = SkillId::from_str("skill-a").unwrap();
        {
            let mut svc = SkillService::new(&mut backend);
            svc.attach(&ws("ws-a"), &agent, &skill).unwrap();
            svc.attach(&ws("ws-a"), &agent, &skill).unwrap(); // idempotent
        }
        assert_eq!(backend.links.len(), 1);
        {
            let mut svc = SkillService::new(&mut backend);
            svc.detach(&ws("ws-a"), &agent, &skill).unwrap();
            svc.detach(&ws("ws-a"), &agent, &skill).unwrap(); // idempotent
        }
        assert!(backend.links.is_empty());
    }

    #[test]
    fn attach_cross_workspace_is_rejected() {
        let mut backend = MemBackend::default()
            .with_skill("ws-a", "skill-a", "commit")
            .with_agent("ws-b", "agent-b");
        let mut svc = SkillService::new(&mut backend);
        let err = svc
            .attach(
                &ws("ws-a"),
                &AgentId::from_str("agent-b").unwrap(),
                &SkillId::from_str("skill-a").unwrap(),
            )
            .unwrap_err();
        assert_eq!(err, MemError::CrossWorkspace);
    }
}
