//! Claude Desktop adapter — desktop app MCP-config layout
//! (`~/Library/Application Support/Claude/claude_desktop_config.json`).
//!
//! Per spec §7.4 claude-desktop accepts only `mcp-server`. Every other
//! kind declines with a recorded reason. v1 convention-deploys
//! mcp-server units under `mcp-servers/<name>/`; P3.5+ will collapse
//! these into the real config JSON via JSON-merge-patch.

use std::collections::HashMap;
use std::path::PathBuf;

use ainb_adapters_source::ResolvedUnit;
use ainb_skill_core::{DeployedRef, UnitKind};

use crate::install_root::install_root_for;
use crate::plan::{InstallPlan, InstallReport, apply_plan};
use crate::{AcceptDecision, ToolAdapter, convention};

pub struct ClaudeDesktopAdapter;

impl ClaudeDesktopAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClaudeDesktopAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolAdapter for ClaudeDesktopAdapter {
    fn name(&self) -> &'static str {
        "claude-desktop"
    }

    fn install_root(&self) -> PathBuf {
        install_root_for("claude-desktop")
    }

    fn accepts(&self, kind: UnitKind) -> AcceptDecision {
        match kind {
            UnitKind::McpServer => AcceptDecision::Yes,
            UnitKind::Skill => AcceptDecision::No {
                reason: "claude-desktop does not support skills (desktop app surface only)".into(),
            },
            UnitKind::Plugin => AcceptDecision::No {
                reason: "claude-desktop does not support plugins".into(),
            },
            UnitKind::Agent => AcceptDecision::No {
                reason: "claude-desktop does not support agent definitions".into(),
            },
            UnitKind::Command => AcceptDecision::No {
                reason: "claude-desktop does not support custom commands".into(),
            },
            UnitKind::Hook => AcceptDecision::No {
                reason: "claude-desktop does not support hooks".into(),
            },
            UnitKind::Statusline => AcceptDecision::No {
                reason: "claude-desktop does not support statuslines".into(),
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
        convention::collect_subdir_entries(&root, "mcp-servers", &mut out);
        Ok(out)
    }

    fn template_substitutions(&self) -> HashMap<&'static str, String> {
        let mut m = HashMap::new();
        m.insert("TOOL_DIR", "Library/Application Support/Claude".to_string());
        m.insert("TOOL_NAME", "claude-desktop".to_string());
        m.insert(
            "HOME_TOOL_DIR",
            "~/Library/Application Support/Claude".to_string(),
        );
        m
    }
}
