//! Claude adapter — `~/.claude` layout (during P3 we keep all writes
//! under `$AINB_HOME/tools/claude/`; P6 will swap in the real home).
//!
//! Per spec §7.4, claude is the most permissive tool: it accepts
//! every unit kind (skill, plugin, agent, command, hook, mcp-server,
//! statusline). Deploy mechanics live in `convention::*` and are
//! shared with the other convention-layout tools (codex, copilot).

use std::collections::HashMap;
use std::path::PathBuf;

use ainb_adapters_source::ResolvedUnit;
use ainb_skill_core::{DeployedRef, UnitKind};

use crate::install_root::install_root_for;
use crate::plan::{apply_plan, InstallPlan, InstallReport};
use crate::{convention, AcceptDecision, ToolAdapter};

pub struct ClaudeAdapter;

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolAdapter for ClaudeAdapter {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn install_root(&self) -> PathBuf {
        install_root_for("claude")
    }

    fn accepts(&self, _kind: UnitKind) -> AcceptDecision {
        // Claude accepts every kind defined in the v1 matrix.
        AcceptDecision::Yes
    }

    fn plan_install(&self, unit: &ResolvedUnit) -> anyhow::Result<InstallPlan> {
        convention::plan_install_prefix_swap(self.name(), unit, &self.install_root(), &self.template_substitutions())
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
        convention::collect_subdir_entries(&root, "plugins", &mut out);
        convention::collect_flat_md(&root, "agents", &mut out);
        convention::collect_flat_md(&root, "commands", &mut out);
        convention::collect_subdir_entries(&root, "hooks", &mut out);
        convention::collect_subdir_entries(&root, "mcp-servers", &mut out);
        convention::collect_subdir_entries(&root, "statuslines", &mut out);
        Ok(out)
    }

    fn template_substitutions(&self) -> HashMap<&'static str, String> {
        let mut m = HashMap::new();
        m.insert("TOOL_DIR", ".claude".to_string());
        m.insert("TOOL_NAME", "claude".to_string());
        m.insert("HOME_TOOL_DIR", "~/.claude".to_string());
        m
    }
}
