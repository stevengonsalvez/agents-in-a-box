//! Cursor adapter — Cursor IDE config layout (`~/.cursor`).
//!
//! Per spec §7.4 cursor accepts `skill`, `command`, and `mcp-server`;
//! declines plugin/agent/hook/statusline.

use std::collections::HashMap;
use std::path::PathBuf;

use ainb_adapters_source::ResolvedUnit;
use ainb_skill_core::{DeployedRef, UnitKind};

use crate::install_root::install_root_for;
use crate::plan::{InstallPlan, InstallReport, apply_plan};
use crate::{AcceptDecision, ToolAdapter, convention};

pub struct CursorAdapter;

impl CursorAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CursorAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolAdapter for CursorAdapter {
    fn name(&self) -> &'static str {
        "cursor"
    }

    fn install_root(&self) -> PathBuf {
        install_root_for("cursor")
    }

    fn accepts(&self, kind: UnitKind) -> AcceptDecision {
        match kind {
            UnitKind::Skill | UnitKind::Command | UnitKind::McpServer => AcceptDecision::Yes,
            UnitKind::Plugin => AcceptDecision::No {
                reason: "cursor does not support plugins (claude-native)".into(),
            },
            UnitKind::Agent => AcceptDecision::No {
                reason: "cursor does not support agent definitions".into(),
            },
            UnitKind::Hook => AcceptDecision::No {
                reason: "cursor does not support hooks".into(),
            },
            UnitKind::Statusline => AcceptDecision::No {
                reason: "cursor does not support statuslines".into(),
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
        convention::collect_flat_md(&root, "commands", &mut out);
        convention::collect_subdir_entries(&root, "mcp-servers", &mut out);
        Ok(out)
    }

    fn template_substitutions(&self) -> HashMap<&'static str, String> {
        let mut m = HashMap::new();
        m.insert("TOOL_DIR", ".cursor".to_string());
        m.insert("TOOL_NAME", "cursor".to_string());
        m.insert("HOME_TOOL_DIR", "~/.cursor".to_string());
        m
    }
}
