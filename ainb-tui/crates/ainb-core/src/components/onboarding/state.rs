// ABOUTME: State management for onboarding wizard
// Tracks current step, user inputs, and validation results

use std::path::PathBuf;
use super::dependency_checker::DependencyStatus;
use crate::editors;

/// Steps in the onboarding wizard
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingStep {
    Welcome,
    DependencyCheck,
    GitDirectories,
    Authentication,
    EditorSelection,
    Summary,
}

/// Available editor option for selection
#[derive(Debug, Clone)]
pub struct EditorOption {
    /// Display name (e.g., "VS Code", "Cursor")
    pub name: String,
    /// CLI command (e.g., "code", "cursor")
    pub command: String,
    /// Whether this editor is installed/available
    pub available: bool,
}

impl OnboardingStep {
    /// Get all steps in order
    pub fn all() -> &'static [OnboardingStep] {
        &[
            Self::Welcome,
            Self::DependencyCheck,
            Self::GitDirectories,
            Self::Authentication,
            Self::EditorSelection,
            Self::Summary,
        ]
    }

    /// Get the step number (1-indexed for display)
    pub fn number(&self) -> usize {
        match self {
            Self::Welcome => 1,
            Self::DependencyCheck => 2,
            Self::GitDirectories => 3,
            Self::Authentication => 4,
            Self::EditorSelection => 5,
            Self::Summary => 6,
        }
    }

    /// Get the total number of steps
    pub fn total() -> usize {
        6
    }

    /// Get display title for this step
    pub fn title(&self) -> &'static str {
        match self {
            Self::Welcome => "Welcome",
            Self::DependencyCheck => "Dependencies",
            Self::GitDirectories => "Git Directories",
            Self::Authentication => "Authentication",
            Self::EditorSelection => "Editor",
            Self::Summary => "Summary",
        }
    }

    /// Get description for this step
    pub fn description(&self) -> &'static str {
        match self {
            Self::Welcome => "Let's get you set up with AINB",
            Self::DependencyCheck => "Checking required tools",
            Self::GitDirectories => "Where are your projects?",
            Self::Authentication => "Set up AI agent authentication",
            Self::EditorSelection => "Choose your preferred editor",
            Self::Summary => "You're all set!",
        }
    }

    /// Can we go to the next step?
    pub fn can_advance(&self, state: &OnboardingState) -> bool {
        match self {
            Self::Welcome => true,
            Self::DependencyCheck => {
                // Can advance if mandatory deps are met
                state.dependency_status.as_ref()
                    .map(|s| s.mandatory_met)
                    .unwrap_or(false)
            }
            Self::GitDirectories => {
                // Can advance if at least one valid directory
                !state.validated_directories.is_empty()
            }
            Self::Authentication => {
                // Auth can be skipped
                true
            }
            Self::EditorSelection => {
                // Editor selection can be skipped (will use fallback)
                true
            }
            Self::Summary => {
                // Can finish from summary
                true
            }
        }
    }

    /// Get the next step, if any
    pub fn next(&self) -> Option<Self> {
        match self {
            Self::Welcome => Some(Self::DependencyCheck),
            Self::DependencyCheck => Some(Self::GitDirectories),
            Self::GitDirectories => Some(Self::Authentication),
            Self::Authentication => Some(Self::EditorSelection),
            Self::EditorSelection => Some(Self::Summary),
            Self::Summary => None,
        }
    }

    /// Get the previous step, if any
    pub fn previous(&self) -> Option<Self> {
        match self {
            Self::Welcome => None,
            Self::DependencyCheck => Some(Self::Welcome),
            Self::GitDirectories => Some(Self::DependencyCheck),
            Self::Authentication => Some(Self::GitDirectories),
            Self::EditorSelection => Some(Self::Authentication),
            Self::Summary => Some(Self::EditorSelection),
        }
    }
}

/// Validation result for a git directory path
#[derive(Debug, Clone)]
pub struct ValidatedPath {
    pub path: PathBuf,
    pub is_valid: bool,
    pub expanded_path: PathBuf,
    pub error: Option<String>,
}

impl ValidatedPath {
    /// Validate a path string
    pub fn from_string(path_str: &str) -> Self {
        let trimmed = path_str.trim();

        if trimmed.is_empty() {
            return Self {
                path: PathBuf::new(),
                is_valid: false,
                expanded_path: PathBuf::new(),
                error: Some("Path is empty".to_string()),
            };
        }

        // Expand ~ to home directory
        let expanded = if trimmed.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                home.join(&trimmed[2..])
            } else {
                PathBuf::from(trimmed)
            }
        } else if trimmed == "~" {
            dirs::home_dir().unwrap_or_else(|| PathBuf::from(trimmed))
        } else {
            PathBuf::from(trimmed)
        };

        // Check if path exists and is a directory
        if !expanded.exists() {
            return Self {
                path: PathBuf::from(trimmed),
                is_valid: false,
                expanded_path: expanded,
                error: Some("Directory does not exist".to_string()),
            };
        }

        if !expanded.is_dir() {
            return Self {
                path: PathBuf::from(trimmed),
                is_valid: false,
                expanded_path: expanded,
                error: Some("Path is not a directory".to_string()),
            };
        }

        Self {
            path: PathBuf::from(trimmed),
            is_valid: true,
            expanded_path: expanded,
            error: None,
        }
    }
}

/// Focus areas within steps that have multiple interactive elements
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingFocus {
    /// Main content area
    Content,
    /// Navigation buttons (Back/Next)
    Navigation,
    /// Specific item index (for lists)
    Item(usize),
}

/// Full onboarding wizard state
#[derive(Debug)]
pub struct OnboardingState {
    /// Current step in the wizard
    pub current_step: OnboardingStep,
    /// Current focus area
    pub focus: OnboardingFocus,
    /// Dependency check results (populated after check)
    pub dependency_status: Option<DependencyStatus>,
    /// Whether dependency check is in progress
    pub dependency_check_running: bool,
    /// Raw input for git directories (comma-separated)
    pub git_directories_input: String,
    /// Validated directory paths
    pub validated_directories: Vec<ValidatedPath>,
    /// Whether auth was completed/skipped
    pub auth_completed: bool,
    /// Auth method chosen (if any)
    pub auth_method: Option<String>,
    /// Whether this is a factory reset (re-running setup)
    pub is_factory_reset: bool,
    /// Cursor position for text input
    pub cursor_position: usize,
    /// Whether to show cursor
    pub show_cursor: bool,
    /// Error message to display
    pub error_message: Option<String>,
    /// Selected index in dependency list
    pub selected_dep_index: usize,
    /// Dependencies user chose to skip
    pub skipped_dependencies: Vec<String>,
    /// Available editor options (detected on EditorSelection step)
    pub available_editors: Vec<EditorOption>,
    /// Currently selected editor index
    pub selected_editor_index: usize,
}

impl OnboardingState {
    pub fn new() -> Self {
        Self {
            current_step: OnboardingStep::Welcome,
            focus: OnboardingFocus::Content,
            dependency_status: None,
            dependency_check_running: false,
            git_directories_input: Self::default_git_directories(),
            validated_directories: Vec::new(),
            auth_completed: false,
            auth_method: None,
            is_factory_reset: false,
            cursor_position: 0,
            show_cursor: true,
            error_message: None,
            selected_dep_index: 0,
            skipped_dependencies: Vec::new(),
            available_editors: Vec::new(),
            selected_editor_index: 0,
        }
    }

    /// Detect available editors on the system
    pub fn detect_available_editors() -> Vec<EditorOption> {
        editors::detect_available_editors()
            .into_iter()
            .map(|(name, command, available)| EditorOption {
                name,
                command,
                available,
            })
            .collect()
    }

    /// Get the currently selected editor command (if any available editor is selected)
    pub fn get_selected_editor(&self) -> Option<String> {
        self.available_editors
            .get(self.selected_editor_index)
            .filter(|e| e.available)
            .map(|e| e.command.clone())
    }

    /// Initialize editors if not already done
    pub fn init_editors_if_needed(&mut self) {
        if self.available_editors.is_empty() {
            self.available_editors = Self::detect_available_editors();
            // Select first available editor by default
            self.selected_editor_index = self
                .available_editors
                .iter()
                .position(|e| e.available)
                .unwrap_or(0);
        }
    }

    /// Create state for factory reset flow
    pub fn for_factory_reset() -> Self {
        let mut state = Self::new();
        state.is_factory_reset = true;
        state
    }

    /// Get default git directories suggestion
    fn default_git_directories() -> String {
        let home = dirs::home_dir()
            .map(|h| h.display().to_string())
            .unwrap_or_else(|| "~".to_string());

        // Common project directories
        let defaults = vec![
            format!("{}/projects", home),
            format!("{}/code", home),
            format!("{}/dev", home),
            format!("{}/git", home),
        ];

        // Filter to only existing directories
        let existing: Vec<String> = defaults
            .into_iter()
            .filter(|p| {
                let path = if p.starts_with(&home) {
                    PathBuf::from(p)
                } else {
                    PathBuf::from(p)
                };
                path.exists() && path.is_dir()
            })
            .collect();

        if existing.is_empty() {
            format!("{}/projects", home)
        } else {
            existing.join(", ")
        }
    }

    /// Validate the current git directories input
    pub fn validate_git_directories(&mut self) {
        let paths: Vec<&str> = self.git_directories_input
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        self.validated_directories = paths
            .iter()
            .map(|p| ValidatedPath::from_string(p))
            .collect();
    }

    /// Get valid directories only
    pub fn get_valid_directories(&self) -> Vec<PathBuf> {
        self.validated_directories
            .iter()
            .filter(|v| v.is_valid)
            .map(|v| v.expanded_path.clone())
            .collect()
    }

    /// Move to next step if possible
    /// Returns (advanced: bool, trigger_dep_check: bool)
    pub fn advance(&mut self) -> (bool, bool) {
        if self.current_step.can_advance(self) {
            if let Some(next) = self.current_step.next() {
                self.current_step = next;
                self.focus = OnboardingFocus::Content;
                self.error_message = None;

                // Auto-trigger dependency check when entering DependencyCheck step
                let trigger_dep_check = next == OnboardingStep::DependencyCheck
                    && self.dependency_status.is_none();

                return (true, trigger_dep_check);
            }
        }
        (false, false)
    }

    /// Move to previous step
    pub fn go_back(&mut self) -> bool {
        if let Some(prev) = self.current_step.previous() {
            self.current_step = prev;
            self.focus = OnboardingFocus::Content;
            self.error_message = None;
            return true;
        }
        false
    }

    /// Handle text input character
    pub fn input_char(&mut self, c: char) {
        if self.current_step == OnboardingStep::GitDirectories {
            self.git_directories_input.insert(self.cursor_position, c);
            self.cursor_position += 1;
            self.validate_git_directories();
        }
    }

    /// Handle backspace
    pub fn backspace(&mut self) {
        if self.current_step == OnboardingStep::GitDirectories && self.cursor_position > 0 {
            self.cursor_position -= 1;
            self.git_directories_input.remove(self.cursor_position);
            self.validate_git_directories();
        }
    }

    /// Handle delete key
    pub fn delete(&mut self) {
        if self.current_step == OnboardingStep::GitDirectories
            && self.cursor_position < self.git_directories_input.len()
        {
            self.git_directories_input.remove(self.cursor_position);
            self.validate_git_directories();
        }
    }

    /// Move cursor left
    pub fn cursor_left(&mut self) {
        if self.cursor_position > 0 {
            self.cursor_position -= 1;
        }
    }

    /// Move cursor right
    pub fn cursor_right(&mut self) {
        if self.cursor_position < self.git_directories_input.len() {
            self.cursor_position += 1;
        }
    }

    /// Move cursor to start
    pub fn cursor_home(&mut self) {
        self.cursor_position = 0;
    }

    /// Move cursor to end
    pub fn cursor_end(&mut self) {
        self.cursor_position = self.git_directories_input.len();
    }

    /// Toggle cursor visibility (for blinking)
    pub fn toggle_cursor(&mut self) {
        self.show_cursor = !self.show_cursor;
    }

    /// Check if we're on the final step
    pub fn is_final_step(&self) -> bool {
        self.current_step == OnboardingStep::Summary
    }

    /// Check if we can go back
    pub fn can_go_back(&self) -> bool {
        self.current_step.previous().is_some()
    }
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_navigation() {
        let step = OnboardingStep::Welcome;
        assert_eq!(step.next(), Some(OnboardingStep::DependencyCheck));
        assert_eq!(step.previous(), None);

        let step = OnboardingStep::Summary;
        assert_eq!(step.next(), None);
        assert_eq!(step.previous(), Some(OnboardingStep::EditorSelection));

        let step = OnboardingStep::EditorSelection;
        assert_eq!(step.next(), Some(OnboardingStep::Summary));
        assert_eq!(step.previous(), Some(OnboardingStep::Authentication));
    }

    #[test]
    fn test_step_numbers() {
        assert_eq!(OnboardingStep::Welcome.number(), 1);
        assert_eq!(OnboardingStep::EditorSelection.number(), 5);
        assert_eq!(OnboardingStep::Summary.number(), 6);
        assert_eq!(OnboardingStep::total(), 6);
    }

    #[test]
    fn test_path_validation_empty() {
        let result = ValidatedPath::from_string("");
        assert!(!result.is_valid);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_path_validation_tilde_expansion() {
        let result = ValidatedPath::from_string("~");
        // Should expand ~ to home dir
        assert_ne!(result.expanded_path.to_string_lossy(), "~");
    }

    #[test]
    fn test_state_initialization() {
        let state = OnboardingState::new();
        assert_eq!(state.current_step, OnboardingStep::Welcome);
        assert!(!state.is_factory_reset);
    }

    #[test]
    fn test_factory_reset_state() {
        let state = OnboardingState::for_factory_reset();
        assert!(state.is_factory_reset);
    }

    #[test]
    fn test_text_input() {
        let mut state = OnboardingState::new();
        state.current_step = OnboardingStep::GitDirectories;
        state.git_directories_input.clear();
        state.cursor_position = 0;

        state.input_char('a');
        state.input_char('b');

        assert_eq!(state.git_directories_input, "ab");
        assert_eq!(state.cursor_position, 2);

        state.backspace();
        assert_eq!(state.git_directories_input, "a");
        assert_eq!(state.cursor_position, 1);
    }
}
