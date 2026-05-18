//! Codex adapter — `~/.codex` layout (P3 keeps writes under
//! `$AINB_HOME/tools/codex/`).
//!
//! Per spec §7.4, codex accepts skill, agent, command, and
//! mcp-server. Declines plugin, hook, and statusline with a
//! reason that surfaces in the lockfile as `status: skipped`.

use std::collections::HashMap;
use std::path::PathBuf;

use ainb_adapters_source::ResolvedUnit;
use ainb_skill_core::{DeployedRef, UnitKind};

use crate::install_root::install_root_for;
use crate::plan::{apply_plan, InstallPlan, InstallReport};
use crate::{convention, AcceptDecision, ToolAdapter};

pub struct CodexAdapter;

impl CodexAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolAdapter for CodexAdapter {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn install_root(&self) -> PathBuf {
        install_root_for("codex")
    }

    fn accepts(&self, kind: UnitKind) -> AcceptDecision {
        match kind {
            UnitKind::Skill
            | UnitKind::Agent
            | UnitKind::Command
            | UnitKind::McpServer => AcceptDecision::Yes,
            UnitKind::Plugin => AcceptDecision::No {
                reason: "codex does not support plugins (claude-native)".into(),
            },
            UnitKind::Hook => AcceptDecision::No {
                reason: "codex does not support hooks".into(),
            },
            UnitKind::Statusline => AcceptDecision::No {
                reason: "codex does not support statuslines".into(),
            },
        }
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
        convention::collect_subdir_entries(&root, "mcp-servers", &mut out);
        convention::collect_flat_md(&root, "agents", &mut out);
        convention::collect_flat_md(&root, "commands", &mut out);
        Ok(out)
    }

    fn template_substitutions(&self) -> HashMap<&'static str, String> {
        let mut m = HashMap::new();
        m.insert("TOOL_DIR", ".codex".to_string());
        m.insert("TOOL_NAME", "codex".to_string());
        m.insert("HOME_TOOL_DIR", "~/.codex".to_string());
        m
    }
}
