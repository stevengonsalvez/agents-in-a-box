//! Copilot adapter — narrowest v1 acceptance matrix.
//!
//! Per spec §7.4, copilot only accepts `skill` and `agent`. Every
//! other kind returns `No { reason }` so the manager records the
//! skip in the lockfile and surfaces it on the dashboard.

use std::collections::HashMap;
use std::path::PathBuf;

use ainb_adapters_source::ResolvedUnit;
use ainb_skill_core::{DeployedRef, UnitKind};

use crate::install_root::install_root_for;
use crate::plan::{InstallPlan, InstallReport, apply_plan};
use crate::{AcceptDecision, ToolAdapter, convention};

pub struct CopilotAdapter;

impl CopilotAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CopilotAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolAdapter for CopilotAdapter {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn install_root(&self) -> PathBuf {
        install_root_for("copilot")
    }

    fn accepts(&self, kind: UnitKind) -> AcceptDecision {
        match kind {
            UnitKind::Skill | UnitKind::Agent => AcceptDecision::Yes,
            UnitKind::Plugin => AcceptDecision::No {
                reason: "copilot does not support plugins".into(),
            },
            UnitKind::Command => AcceptDecision::No {
                reason: "copilot does not support custom commands".into(),
            },
            UnitKind::Hook => AcceptDecision::No {
                reason: "copilot does not support hooks".into(),
            },
            UnitKind::McpServer => AcceptDecision::No {
                reason: "copilot does not support MCP servers".into(),
            },
            UnitKind::Statusline => AcceptDecision::No {
                reason: "copilot does not support statuslines".into(),
            },
        }
    }

    fn plan_install(&self, unit: &ResolvedUnit) -> anyhow::Result<InstallPlan> {
        convention::plan_install_prefix_swap(
            self.name(),
            unit,
            &self.install_root(),
            &self.template_substitutions(),
        )
    }

    fn apply(&self, plan: &InstallPlan) -> anyhow::Result<InstallReport> {
        apply_plan(plan, &self.install_root())
    }

    fn uninstall(&self, deployed: &DeployedRef) -> anyhow::Result<()> {
        convention::uninstall(deployed, &self.install_root())
    }

    fn list_installed(&self) -> anyhow::Result<Vec<DeployedRef>> {
        let root = self.install_root();
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        convention::collect_subdir_entries(&root, "skills", &mut out);
        convention::collect_flat_md(&root, "agents", &mut out);
        Ok(out)
    }

    fn template_substitutions(&self) -> HashMap<&'static str, String> {
        let mut m = HashMap::new();
        m.insert("TOOL_DIR", ".github/copilot".to_string());
        m.insert("TOOL_NAME", "copilot".to_string());
        m.insert("HOME_TOOL_DIR", "~/.copilot".to_string());
        m
    }
}
