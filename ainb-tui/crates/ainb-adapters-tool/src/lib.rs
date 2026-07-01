//! ainb-adapters-tool — per-tool deploy adapters.
//!
//! Each adapter knows where a tool keeps its on-disk state, which
//! unit kinds it accepts, and how to plan / apply / uninstall a unit.
//! Adapters never touch the user's real `~/.claude` while the
//! skill-manager is pre-P6: install roots resolve under
//! `$AINB_HOME/tools/<tool>/` by default, with a per-tool env
//! override (`AINB_TOOL_HOME_CLAUDE`, …) so tests sandbox cleanly and
//! P6 can flip the default without changing the call sites.

use std::collections::HashMap;
use std::path::PathBuf;

use ainb_adapters_source::ResolvedUnit;
use ainb_skill_core::{DeployedRef, UnitKind};

pub mod amazonq;
pub mod claude;
pub mod claude_desktop;
pub mod cline;
pub mod codex;
pub mod convention;
pub mod copilot;
pub mod cursor;
pub mod gemini;
pub mod install_root;
pub mod plan;
pub mod roo;

pub use amazonq::AmazonqAdapter;
pub use claude::ClaudeAdapter;
pub use claude_desktop::ClaudeDesktopAdapter;
pub use cline::ClineAdapter;
pub use codex::CodexAdapter;
pub use copilot::CopilotAdapter;
pub use cursor::CursorAdapter;
pub use gemini::GeminiAdapter;
pub use install_root::{install_root_for, read_root_for};
pub use plan::{InstallPlan, InstallReport, PlanOp};
pub use roo::RooAdapter;

/// All v1 tool adapters in priority order. Built fresh per call so
/// callers can mix-and-match overrides per invocation.
pub fn all_adapters() -> Vec<Box<dyn ToolAdapter>> {
    vec![
        Box::new(ClaudeAdapter::new()),
        Box::new(CodexAdapter::new()),
        Box::new(CopilotAdapter::new()),
        Box::new(GeminiAdapter::new()),
        Box::new(CursorAdapter::new()),
        Box::new(AmazonqAdapter::new()),
        Box::new(ClaudeDesktopAdapter::new()),
        Box::new(ClineAdapter::new()),
        Box::new(RooAdapter::new()),
    ]
}

/// Look up an adapter by stable name.
pub fn adapter_by_name(name: &str) -> Option<Box<dyn ToolAdapter>> {
    match name {
        "claude" => Some(Box::new(ClaudeAdapter::new())),
        "codex" => Some(Box::new(CodexAdapter::new())),
        "copilot" => Some(Box::new(CopilotAdapter::new())),
        "gemini" => Some(Box::new(GeminiAdapter::new())),
        "cursor" => Some(Box::new(CursorAdapter::new())),
        "amazonq" => Some(Box::new(AmazonqAdapter::new())),
        "claude-desktop" => Some(Box::new(ClaudeDesktopAdapter::new())),
        "cline" => Some(Box::new(ClineAdapter::new())),
        "roo" => Some(Box::new(RooAdapter::new())),
        _ => None,
    }
}

/// `accepts(kind)` decision for one (tool, kind) pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptDecision {
    Yes,
    No { reason: String },
}

impl AcceptDecision {
    pub fn is_yes(&self) -> bool {
        matches!(self, Self::Yes)
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Yes => None,
            Self::No { reason } => Some(reason.as_str()),
        }
    }
}

/// Tool-adapter contract (spec §7.3).
pub trait ToolAdapter: Send + Sync {
    /// Stable identifier (`claude` / `codex` / `copilot` / …).
    fn name(&self) -> &'static str;

    /// On-disk root for this tool's managed state.
    fn install_root(&self) -> PathBuf;

    /// Per-kind acceptance — spec §7.4.
    fn accepts(&self, kind: UnitKind) -> AcceptDecision;

    /// Compute an install plan for `unit` against this tool's layout.
    fn plan_install(&self, unit: &ResolvedUnit) -> anyhow::Result<InstallPlan>;

    /// Apply a plan, returning the deployment record (paths + hashes)
    /// the lockfile should remember.
    fn apply(&self, plan: &InstallPlan) -> anyhow::Result<InstallReport>;

    /// Remove a previously-installed unit by its `DeployedRef::Deployed`
    /// record (the matching variant from the lockfile).
    fn uninstall(&self, deployed: &DeployedRef) -> anyhow::Result<()>;

    /// Enumerate units currently installed under [`Self::install_root`].
    /// Result entries are always [`DeployedRef::Deployed`] variants;
    /// adapters use these to detect drift in P3.5+.
    fn list_installed(&self) -> anyhow::Result<Vec<DeployedRef>>;

    /// Static template substitutions exposed to unit content (e.g.
    /// `{{TOOL_DIR}} → .claude`). Currently informational; P3.5 will
    /// rewrite unit content through this map before writing.
    fn template_substitutions(&self) -> HashMap<&'static str, String>;
}
