// ABOUTME: Renderer-agnostic half of the `setup_menu` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::setup_menu`, which re-exports this module.

/// Menu items in the setup menu
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupMenuItem {
    RerunWizard,
    CheckDependencies,
    ConfigureGitPaths,
    AuthenticationSettings,
    EditorPreference,
    FactoryReset,
}

impl SetupMenuItem {
    /// Get all menu items in order
    pub fn all() -> &'static [SetupMenuItem] {
        &[
            Self::RerunWizard,
            Self::CheckDependencies,
            Self::ConfigureGitPaths,
            Self::AuthenticationSettings,
            Self::EditorPreference,
            Self::FactoryReset,
        ]
    }

    /// Get display label
    pub fn label(&self) -> &'static str {
        match self {
            Self::RerunWizard => "Re-run Setup Wizard",
            Self::CheckDependencies => "Check Dependencies",
            Self::ConfigureGitPaths => "Configure Git Paths",
            Self::AuthenticationSettings => "Authentication Settings",
            Self::EditorPreference => "Editor Preference",
            Self::FactoryReset => "Factory Reset AINB",
        }
    }

    /// Get description for this item
    pub fn description(&self) -> &'static str {
        match self {
            Self::RerunWizard => "Start the full setup wizard from scratch",
            Self::CheckDependencies => "Verify all required tools are installed",
            Self::ConfigureGitPaths => "Update your project directories",
            Self::AuthenticationSettings => {
                "Configure auth for Claude, Codex, Antigravity, Gemini, Copilot"
            }
            Self::EditorPreference => "Choose your preferred code editor",
            Self::FactoryReset => "Remove all configuration and start fresh",
        }
    }

    /// Get icon for this item
    pub fn icon(&self) -> &'static str {
        match self {
            Self::RerunWizard => "🔄",
            Self::CheckDependencies => "🔍",
            Self::ConfigureGitPaths => "📁",
            Self::AuthenticationSettings => "🔐",
            Self::EditorPreference => "📝",
            Self::FactoryReset => "⚠️",
        }
    }

    /// Is this a dangerous action?
    pub fn is_dangerous(&self) -> bool {
        matches!(self, Self::FactoryReset)
    }
}

/// State for the setup menu
#[derive(Debug)]
pub struct SetupMenuState {
    /// Currently selected item index
    pub selected_index: usize,
    /// Whether confirmation dialog is showing
    pub showing_confirmation: bool,
    /// Item pending confirmation
    pub pending_action: Option<SetupMenuItem>,
}

impl SetupMenuState {
    pub fn new() -> Self {
        Self {
            selected_index: 0,
            showing_confirmation: false,
            pending_action: None,
        }
    }

    /// Get currently selected item
    pub fn selected_item(&self) -> SetupMenuItem {
        SetupMenuItem::all()[self.selected_index]
    }

    /// Move selection up
    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    /// Move selection down
    pub fn move_down(&mut self) {
        let max_index = SetupMenuItem::all().len() - 1;
        if self.selected_index < max_index {
            self.selected_index += 1;
        }
    }

    /// Request action (may show confirmation)
    pub fn request_action(&mut self) -> Option<SetupMenuItem> {
        let item = self.selected_item();
        if item.is_dangerous() {
            self.showing_confirmation = true;
            self.pending_action = Some(item);
            None
        } else {
            Some(item)
        }
    }

    /// Confirm pending action
    pub fn confirm_action(&mut self) -> Option<SetupMenuItem> {
        self.showing_confirmation = false;
        self.pending_action.take()
    }

    /// Cancel pending action
    pub fn cancel_action(&mut self) {
        self.showing_confirmation = false;
        self.pending_action = None;
    }
}

impl Default for SetupMenuState {
    fn default() -> Self {
        Self::new()
    }
}
