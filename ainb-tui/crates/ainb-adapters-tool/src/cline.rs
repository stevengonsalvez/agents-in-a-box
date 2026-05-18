//! Cline adapter — Cline VS Code extension layout (`~/.cline`).
//!
//! Per spec §7.4 cline accepts `skill` and `mcp-server`. Every other
//! kind declines with a recorded reason.

use std::collections::HashMap;
use std::path::PathBuf;

use ainb_adapters_source::ResolvedUnit;
use ainb_skill_core::{DeployedRef, UnitKind};

use crate::install_root::install_root_for;
use crate::plan::{InstallPlan, InstallReport, apply_plan};
use crate::{AcceptDecision, ToolAdapter, convention};

pub struct ClineAdapter;

impl ClineAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClineAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolAdapter for ClineAdapter {
    fn name(&self) -> &'static str {
        "cline"
    }

    fn install_root(&self) -> PathBuf {
        install_root_for("cline")
    }

    fn accepts(&self, kind: UnitKind) -> AcceptDecision {
        match kind {
            UnitKind::Skill | UnitKind::McpServer => AcceptDecision::Yes,
            UnitKind::Plugin => AcceptDecision::No {
                reason: "cline does not support plugins (claude-native)".into(),
            },
            UnitKind::Agent => AcceptDecision::No {
                reason: "cline does not support agent definitions".into(),
            },
            UnitKind::Command => AcceptDecision::No {
                reason: "cline does not support custom commands".into(),
            },
            UnitKind::Hook => AcceptDecision::No {
                reason: "cline does not support hooks".into(),
            },
            UnitKind::Statusline => AcceptDecision::No {
                reason: "cline does not support statuslines".into(),
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
        convention::collect_subdir_entries(&root, "mcp-servers", &mut out);
        Ok(out)
    }

    fn template_substitutions(&self) -> HashMap<&'static str, String> {
        let mut m = HashMap::new();
        m.insert("TOOL_DIR", ".cline".to_string());
        m.insert("TOOL_NAME", "cline".to_string());
        m.insert("HOME_TOOL_DIR", "~/.cline".to_string());
        m
    }
}
