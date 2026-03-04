// ABOUTME: Dependency checker for onboarding wizard
// Checks for required and optional dependencies with install suggestions

use std::process::Command;
use std::fs;

/// Categories of dependencies
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyCategory {
    /// Core tools (mandatory)
    Core,
    /// Container runtimes
    Container,
    /// Session management
    Session,
    /// Toolkit dependencies
    Toolkit,
    /// GitHub integration
    GitHub,
    /// AI CLI tools
    AiCli,
    /// Configuration optimizations
    Configuration,
}

impl DependencyCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Core => "Core Tools",
            Self::Container => "Container Runtime",
            Self::Session => "Session Management",
            Self::Toolkit => "Toolkit",
            Self::GitHub => "GitHub",
            Self::AiCli => "AI CLIs",
            Self::Configuration => "Configuration",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Core => "Required for basic functionality",
            Self::Container => "For isolated development environments",
            Self::Session => "For managing terminal sessions",
            Self::Toolkit => "For agent toolkit installation",
            Self::GitHub => "For GitHub integration",
            Self::AiCli => "AI coding assistants",
            Self::Configuration => "Recommended optimizations",
        }
    }
}

/// Definition of a dependency to check
#[derive(Debug, Clone)]
pub struct Dependency {
    /// Display name
    pub name: &'static str,
    /// Command to check if installed
    pub check_cmd: &'static str,
    /// Arguments for the check command
    pub check_args: &'static [&'static str],
    /// Installation hint (brew/npm/etc)
    pub install_hint: &'static str,
    /// Whether this dependency is mandatory
    pub is_mandatory: bool,
    /// Category for grouping
    pub category: DependencyCategory,
    /// Description of what it's used for
    pub description: &'static str,
}

/// Result of checking a single dependency
#[derive(Debug, Clone)]
pub struct DependencyCheckResult {
    pub dependency: Dependency,
    pub is_installed: bool,
    pub version: Option<String>,
}

/// Overall status of all dependency checks
#[derive(Debug, Clone)]
pub struct DependencyStatus {
    pub checks: Vec<DependencyCheckResult>,
    pub mandatory_met: bool,
    pub recommended_met: bool,
}

impl DependencyStatus {
    /// Get all missing mandatory dependencies
    pub fn missing_mandatory(&self) -> Vec<&DependencyCheckResult> {
        self.checks
            .iter()
            .filter(|c| c.dependency.is_mandatory && !c.is_installed)
            .collect()
    }

    /// Get all missing recommended dependencies
    pub fn missing_recommended(&self) -> Vec<&DependencyCheckResult> {
        self.checks
            .iter()
            .filter(|c| !c.dependency.is_mandatory && !c.is_installed)
            .collect()
    }

    /// Get checks by category
    pub fn by_category(&self, category: DependencyCategory) -> Vec<&DependencyCheckResult> {
        self.checks
            .iter()
            .filter(|c| c.dependency.category == category)
            .collect()
    }

    /// Count of installed dependencies
    pub fn installed_count(&self) -> usize {
        self.checks.iter().filter(|c| c.is_installed).count()
    }

    /// Total count of dependencies
    pub fn total_count(&self) -> usize {
        self.checks.len()
    }
}

/// The dependency checker
pub struct DependencyChecker;

impl DependencyChecker {
    /// Get all dependencies to check
    pub fn all_dependencies() -> Vec<Dependency> {
        vec![
            // Core (mandatory)
            Dependency {
                name: "git",
                check_cmd: "git",
                check_args: &["--version"],
                install_hint: "brew install git",
                is_mandatory: true,
                category: DependencyCategory::Core,
                description: "Version control system",
            },
            // Container runtime (recommended - at least one)
            Dependency {
                name: "Docker",
                check_cmd: "docker",
                check_args: &["--version"],
                install_hint: "Install Docker Desktop: https://docker.com/products/docker-desktop",
                is_mandatory: false,
                category: DependencyCategory::Container,
                description: "Container runtime for isolated environments",
            },
            Dependency {
                name: "Colima",
                check_cmd: "colima",
                check_args: &["version"],
                install_hint: "brew install colima",
                is_mandatory: false,
                category: DependencyCategory::Container,
                description: "Lightweight Docker alternative for macOS",
            },
            // Session management (recommended)
            Dependency {
                name: "tmux",
                check_cmd: "tmux",
                check_args: &["-V"],
                install_hint: "brew install tmux",
                is_mandatory: false,
                category: DependencyCategory::Session,
                description: "Terminal multiplexer for session management",
            },
            // macOS-only: Restores user namespace access in tmux
            // Note: This tool has no --version flag, so we test with "echo test"
            #[cfg(target_os = "macos")]
            Dependency {
                name: "reattach-to-user-namespace",
                check_cmd: "reattach-to-user-namespace",
                check_args: &["echo", "test"],
                install_hint: "brew install reattach-to-user-namespace",
                is_mandatory: false,
                category: DependencyCategory::Session,
                description: "Enables say/audio, notifications, keychain in tmux",
            },
            // Toolkit (recommended for full features)
            Dependency {
                name: "Node.js",
                check_cmd: "node",
                check_args: &["--version"],
                install_hint: "brew install node",
                is_mandatory: false,
                category: DependencyCategory::Toolkit,
                description: "JavaScript runtime for toolkit",
            },
            Dependency {
                name: "npm",
                check_cmd: "npm",
                check_args: &["--version"],
                install_hint: "Comes with Node.js",
                is_mandatory: false,
                category: DependencyCategory::Toolkit,
                description: "Package manager for toolkit installation",
            },
            // GitHub integration (optional)
            Dependency {
                name: "GitHub CLI",
                check_cmd: "gh",
                check_args: &["--version"],
                install_hint: "brew install gh",
                is_mandatory: false,
                category: DependencyCategory::GitHub,
                description: "GitHub CLI for issue management",
            },
            // AI CLIs (optional)
            Dependency {
                name: "Claude CLI",
                check_cmd: "claude",
                check_args: &["--version"],
                install_hint: "npm install -g @anthropic-ai/claude-code",
                is_mandatory: false,
                category: DependencyCategory::AiCli,
                description: "Anthropic's Claude Code CLI",
            },
            Dependency {
                name: "Gemini CLI",
                check_cmd: "gemini",
                check_args: &["--version"],
                install_hint: "npm install -g @anthropic-ai/gemini-cli",
                is_mandatory: false,
                category: DependencyCategory::AiCli,
                description: "Google's Gemini CLI",
            },
            Dependency {
                name: "Codex",
                check_cmd: "codex",
                check_args: &["--version"],
                install_hint: "npm install -g @openai/codex",
                is_mandatory: false,
                category: DependencyCategory::AiCli,
                description: "OpenAI's Codex CLI",
            },
            Dependency {
                name: "GitHub Copilot CLI",
                check_cmd: "copilot",
                check_args: &["--version"],
                install_hint: "npm install -g @github/copilot-cli",
                is_mandatory: false,
                category: DependencyCategory::AiCli,
                description: "GitHub's AI coding agent (uses gh OAuth auth)",
            },
            // Configuration (recommended for best experience)
            Dependency {
                name: "tmux optimized config",
                check_cmd: "sh",
                check_args: &["-c", "grep -q 'set-clipboard on' ~/.tmux.conf 2>/dev/null && grep -q 'escape-time 0' ~/.tmux.conf 2>/dev/null"],
                install_hint: "Press 'I' to install recommended tmux.conf",
                is_mandatory: false,
                category: DependencyCategory::Configuration,
                description: "Anti-flicker + clipboard integration for tmux",
            },
        ]
    }

    /// Check if a single dependency is installed
    pub fn check_dependency(dep: &Dependency) -> DependencyCheckResult {
        let output = Command::new(dep.check_cmd)
            .args(dep.check_args)
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let version = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .next()
                    .map(|s| s.trim().to_string());

                DependencyCheckResult {
                    dependency: dep.clone(),
                    is_installed: true,
                    version,
                }
            }
            _ => DependencyCheckResult {
                dependency: dep.clone(),
                is_installed: false,
                version: None,
            },
        }
    }

    /// Check all dependencies synchronously
    pub fn check_all() -> DependencyStatus {
        let dependencies = Self::all_dependencies();
        let checks: Vec<DependencyCheckResult> = dependencies
            .iter()
            .map(Self::check_dependency)
            .collect();

        let mandatory_met = checks
            .iter()
            .filter(|c| c.dependency.is_mandatory)
            .all(|c| c.is_installed);

        // For container runtime, at least one should be installed
        let container_checks: Vec<_> = checks
            .iter()
            .filter(|c| c.dependency.category == DependencyCategory::Container)
            .collect();

        let has_container = container_checks.iter().any(|c| c.is_installed);

        // Recommended = mandatory + at least one container runtime + tmux
        let has_tmux = checks
            .iter()
            .any(|c| c.dependency.name == "tmux" && c.is_installed);

        let recommended_met = mandatory_met && has_container && has_tmux;

        DependencyStatus {
            checks,
            mandatory_met,
            recommended_met,
        }
    }

    /// Generate install commands for missing dependencies
    pub fn get_install_commands(status: &DependencyStatus, include_optional: bool) -> Vec<String> {
        let mut commands = Vec::new();
        let mut brew_packages = Vec::new();
        let mut npm_packages = Vec::new();

        for check in &status.checks {
            if check.is_installed {
                continue;
            }

            if !include_optional && !check.dependency.is_mandatory {
                continue;
            }

            let hint = check.dependency.install_hint;

            if hint.starts_with("brew install ") {
                let pkg = hint.strip_prefix("brew install ").unwrap();
                brew_packages.push(pkg.to_string());
            } else if hint.starts_with("npm install -g ") {
                let pkg = hint.strip_prefix("npm install -g ").unwrap();
                npm_packages.push(pkg.to_string());
            } else if !hint.is_empty() && !hint.starts_with("http") && !hint.starts_with("Comes with") {
                commands.push(hint.to_string());
            }
        }

        // Combine brew packages into single command
        if !brew_packages.is_empty() {
            commands.insert(0, format!("brew install {}", brew_packages.join(" ")));
        }

        // Combine npm packages into single command
        if !npm_packages.is_empty() {
            commands.push(format!("npm install -g {}", npm_packages.join(" ")));
        }

        commands
    }

    /// Get categories that have dependencies
    pub fn categories() -> Vec<DependencyCategory> {
        vec![
            DependencyCategory::Core,
            DependencyCategory::Container,
            DependencyCategory::Session,
            DependencyCategory::Toolkit,
            DependencyCategory::GitHub,
            DependencyCategory::AiCli,
            DependencyCategory::Configuration,
        ]
    }

    /// Install the recommended tmux configuration
    /// Returns Ok(()) on success, Err with message on failure
    pub fn install_tmux_config() -> Result<(), String> {
        let home = dirs::home_dir().ok_or("Could not determine home directory")?;
        let tmux_conf_path = home.join(".tmux.conf");

        // Embedded tmux config content (from ainb-tui/config/tmux.conf)
        let tmux_config = include_str!("../../../config/tmux.conf");

        // Backup existing config if it exists
        if tmux_conf_path.exists() {
            let backup_path = home.join(".tmux.conf.backup");
            fs::copy(&tmux_conf_path, &backup_path)
                .map_err(|e| format!("Failed to backup existing config: {}", e))?;
        }

        // Write new config
        fs::write(&tmux_conf_path, tmux_config)
            .map_err(|e| format!("Failed to write tmux.conf: {}", e))?;

        // Try to reload tmux config if tmux is running
        let _ = Command::new("tmux")
            .args(["source-file", &tmux_conf_path.to_string_lossy()])
            .output();

        Ok(())
    }

    /// Check if a dependency can be auto-installed
    pub fn can_auto_install(dep_name: &str) -> bool {
        matches!(dep_name, "tmux optimized config")
    }

    /// Auto-install a dependency by name
    pub fn auto_install(dep_name: &str) -> Result<(), String> {
        match dep_name {
            "tmux optimized config" => Self::install_tmux_config(),
            _ => Err(format!("No auto-install available for: {}", dep_name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_dependencies_defined() {
        let deps = DependencyChecker::all_dependencies();
        assert!(!deps.is_empty());

        // Ensure at least git is mandatory
        let git = deps.iter().find(|d| d.name == "git");
        assert!(git.is_some());
        assert!(git.unwrap().is_mandatory);
    }

    #[test]
    fn test_check_git() {
        // git should be installed on most dev machines
        let deps = DependencyChecker::all_dependencies();
        let git = deps.iter().find(|d| d.name == "git").unwrap();

        let result = DependencyChecker::check_dependency(git);
        // Don't assert is_installed since CI might not have git
        assert_eq!(result.dependency.name, "git");
    }

    #[test]
    fn test_dependency_categories() {
        let categories = DependencyChecker::categories();
        assert_eq!(categories.len(), 7);
    }

    #[test]
    fn test_install_commands_grouping() {
        let mut status = DependencyStatus {
            checks: vec![
                DependencyCheckResult {
                    dependency: Dependency {
                        name: "git",
                        check_cmd: "git",
                        check_args: &["--version"],
                        install_hint: "brew install git",
                        is_mandatory: true,
                        category: DependencyCategory::Core,
                        description: "Version control",
                    },
                    is_installed: false,
                    version: None,
                },
                DependencyCheckResult {
                    dependency: Dependency {
                        name: "tmux",
                        check_cmd: "tmux",
                        check_args: &["-V"],
                        install_hint: "brew install tmux",
                        is_mandatory: false,
                        category: DependencyCategory::Session,
                        description: "Terminal multiplexer",
                    },
                    is_installed: false,
                    version: None,
                },
            ],
            mandatory_met: false,
            recommended_met: false,
        };

        let commands = DependencyChecker::get_install_commands(&status, true);
        // Should combine into single brew command
        assert!(commands.iter().any(|c| c.contains("brew install") && c.contains("git") && c.contains("tmux")));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_macos_has_reattach_dependency() {
        let deps = DependencyChecker::all_dependencies();
        assert!(
            deps.iter().any(|d| d.name == "reattach-to-user-namespace"),
            "macOS should have reattach-to-user-namespace dependency"
        );

        // Verify it's in Session category and optional
        let reattach = deps.iter().find(|d| d.name == "reattach-to-user-namespace").unwrap();
        assert_eq!(reattach.category, DependencyCategory::Session);
        assert!(!reattach.is_mandatory, "reattach-to-user-namespace should be optional");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn test_non_macos_no_reattach_dependency() {
        let deps = DependencyChecker::all_dependencies();
        assert!(
            !deps.iter().any(|d| d.name == "reattach-to-user-namespace"),
            "Non-macOS should not have reattach-to-user-namespace dependency"
        );
    }
}
