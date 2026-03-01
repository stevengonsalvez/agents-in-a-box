// ABOUTME: Main onboarding wizard component
// Renders step-based wizard UI following premium TUI style guide

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use super::dependency_checker::DependencyChecker;
use super::state::{OnboardingState, OnboardingStep};

// Color palette from TUI style guide
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);
const ERROR_RED: Color = Color::Rgb(220, 80, 80);
const WARNING_YELLOW: Color = Color::Rgb(220, 180, 80);

/// The main onboarding wizard component
pub struct OnboardingComponent;

impl OnboardingComponent {
    pub fn new() -> Self {
        Self
    }

    /// Main render function
    pub fn render(&self, frame: &mut Frame, area: Rect, state: &OnboardingState) {
        // Clear background
        frame.render_widget(Clear, area);

        // Create main container with dark background
        let container = Block::default()
            .style(Style::default().bg(DARK_BG));
        frame.render_widget(container, area);

        // Main layout: header, content, footer
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),  // Header with progress
                Constraint::Min(15),    // Main content
                Constraint::Length(3),  // Navigation footer
            ])
            .split(area);

        self.render_header(frame, layout[0], state);
        self.render_step_content(frame, layout[1], state);
        self.render_navigation(frame, layout[2], state);
    }

    /// Render the header with step progress
    fn render_header(&self, frame: &mut Frame, area: Rect, state: &OnboardingState) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let header_layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(1), // Title
                Constraint::Length(1), // Progress indicator
            ])
            .split(inner);

        // Title
        let reset_indicator = if state.is_factory_reset {
            " (Reset)"
        } else {
            ""
        };

        let title = Paragraph::new(Line::from(vec![
            Span::styled("🛠️ ", Style::default()),
            Span::styled("AINB Setup Wizard", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(reset_indicator, Style::default().fg(WARNING_YELLOW)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(title, header_layout[0]);

        // Progress indicator
        self.render_progress(frame, header_layout[1], state);
    }

    /// Render step progress dots
    fn render_progress(&self, frame: &mut Frame, area: Rect, state: &OnboardingState) {
        let steps = OnboardingStep::all();
        let current_idx = state.current_step.number() - 1;

        let mut spans = vec![Span::styled("  ", Style::default())];

        for (idx, step) in steps.iter().enumerate() {
            let (icon, style) = if idx < current_idx {
                ("●", Style::default().fg(SELECTION_GREEN))
            } else if idx == current_idx {
                ("◉", Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
            } else {
                ("○", Style::default().fg(MUTED_GRAY))
            };

            spans.push(Span::styled(icon, style));
            spans.push(Span::styled(" ", Style::default()));
            spans.push(Span::styled(
                step.title(),
                if idx == current_idx {
                    Style::default().fg(SOFT_WHITE)
                } else {
                    Style::default().fg(MUTED_GRAY)
                },
            ));

            if idx < steps.len() - 1 {
                spans.push(Span::styled(" → ", Style::default().fg(SUBDUED_BORDER)));
            }
        }

        let progress = Paragraph::new(Line::from(spans)).alignment(Alignment::Center);
        frame.render_widget(progress, area);
    }

    /// Render the main step content
    fn render_step_content(&self, frame: &mut Frame, area: Rect, state: &OnboardingState) {
        match state.current_step {
            OnboardingStep::Welcome => self.render_welcome(frame, area, state),
            OnboardingStep::DependencyCheck => self.render_dependencies(frame, area, state),
            OnboardingStep::GitDirectories => self.render_git_directories(frame, area, state),
            OnboardingStep::Authentication => self.render_authentication(frame, area, state),
            OnboardingStep::EditorSelection => self.render_editor_selection(frame, area, state),
            OnboardingStep::Summary => self.render_summary(frame, area, state),
        }
    }

    /// Render welcome step
    fn render_welcome(&self, frame: &mut Frame, area: Rect, _state: &OnboardingState) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG))
            .title(" Welcome ")
            .title_style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let content_layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints([
                Constraint::Length(6),  // Mascot area
                Constraint::Length(3),  // Welcome text
                Constraint::Min(5),     // Description
            ])
            .split(inner);

        // ASCII art mascot (simple box character)
        let mascot = vec![
            "    ╭───────╮    ",
            "    │ ◉   ◉ │    ",
            "    │   ▽   │    ",
            "    │  ───  │    ",
            "    ╰───────╯    ",
        ];

        let mascot_text: Vec<Line> = mascot
            .iter()
            .map(|line| Line::from(Span::styled(*line, Style::default().fg(GOLD))))
            .collect();

        let mascot_widget = Paragraph::new(mascot_text).alignment(Alignment::Center);
        frame.render_widget(mascot_widget, content_layout[0]);

        // Welcome text
        let welcome = Paragraph::new(Line::from(vec![
            Span::styled("Welcome to ", Style::default().fg(SOFT_WHITE)),
            Span::styled("Agents in a Box", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled("!", Style::default().fg(SOFT_WHITE)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(welcome, content_layout[1]);

        // Description
        let description = vec![
            "",
            "This wizard will help you set up AINB by:",
            "",
            "  • Checking required dependencies",
            "  • Configuring your project directories",
            "  • Setting up authentication",
            "",
            "Press Enter or → to continue",
        ];

        let desc_lines: Vec<Line> = description
            .iter()
            .map(|line| {
                if let Some(rest) = line.strip_prefix("  • ") {
                    Line::from(vec![
                        Span::styled("  ", Style::default()),
                        Span::styled("• ", Style::default().fg(GOLD)),
                        Span::styled(rest, Style::default().fg(SOFT_WHITE)),
                    ])
                } else {
                    Line::from(Span::styled(*line, Style::default().fg(MUTED_GRAY)))
                }
            })
            .collect();

        let desc_widget = Paragraph::new(desc_lines).alignment(Alignment::Center);
        frame.render_widget(desc_widget, content_layout[2]);
    }

    /// Render dependency check step
    fn render_dependencies(&self, frame: &mut Frame, area: Rect, state: &OnboardingState) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG))
            .title(" Dependencies ")
            .title_style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if state.dependency_check_running {
            // Show loading state
            let loading = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("🔄 Checking dependencies...", Style::default().fg(GOLD))),
                Line::from(""),
                Line::from(Span::styled("Please wait", Style::default().fg(MUTED_GRAY))),
            ])
            .alignment(Alignment::Center);
            frame.render_widget(loading, inner);
            return;
        }

        let Some(status) = &state.dependency_status else {
            // No status yet - show initial message
            let msg = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("Press Enter to check dependencies", Style::default().fg(SOFT_WHITE))),
            ])
            .alignment(Alignment::Center);
            frame.render_widget(msg, inner);
            return;
        };

        // Show dependency results
        let content_layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(2),  // Status summary
                Constraint::Min(10),    // Dependency list
                Constraint::Length(2),  // Instructions
            ])
            .split(inner);

        // Status summary
        let (status_icon, status_text, status_color) = if status.mandatory_met {
            if status.recommended_met {
                ("✅", "All dependencies ready!", SELECTION_GREEN)
            } else {
                ("⚠️", "Core dependencies ready (some optional missing)", WARNING_YELLOW)
            }
        } else {
            ("❌", "Missing required dependencies", ERROR_RED)
        };

        let summary = Paragraph::new(Line::from(vec![
            Span::styled(status_icon, Style::default()),
            Span::styled(" ", Style::default()),
            Span::styled(status_text, Style::default().fg(status_color)),
            Span::styled(
                format!("  ({}/{})", status.installed_count(), status.total_count()),
                Style::default().fg(MUTED_GRAY),
            ),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(summary, content_layout[0]);

        // Dependency list by category
        let mut items: Vec<ListItem> = Vec::new();

        for category in DependencyChecker::categories() {
            let checks = status.by_category(category);
            if checks.is_empty() {
                continue;
            }

            // Category header
            items.push(ListItem::new(Line::from(vec![
                Span::styled("─── ", Style::default().fg(SUBDUED_BORDER)),
                Span::styled(category.label(), Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(" ───", Style::default().fg(SUBDUED_BORDER)),
            ])));

            // Dependencies in this category
            for check in checks {
                let (icon, icon_color) = if check.is_installed {
                    ("✓", SELECTION_GREEN)
                } else if check.dependency.is_mandatory {
                    ("✗", ERROR_RED)
                } else {
                    ("○", WARNING_YELLOW)
                };

                let version_text = check.version.as_ref()
                    .map(|v| format!(" ({})", v.chars().take(20).collect::<String>()))
                    .unwrap_or_default();

                let install_hint = if !check.is_installed {
                    format!(" → {}", check.dependency.install_hint)
                } else {
                    String::new()
                };

                items.push(ListItem::new(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(icon, Style::default().fg(icon_color)),
                    Span::styled(" ", Style::default()),
                    Span::styled(
                        check.dependency.name,
                        if check.is_installed {
                            Style::default().fg(SOFT_WHITE)
                        } else {
                            Style::default().fg(MUTED_GRAY)
                        },
                    ),
                    Span::styled(version_text, Style::default().fg(MUTED_GRAY)),
                    Span::styled(install_hint, Style::default().fg(CORNFLOWER_BLUE)),
                ])));
            }
        }

        let list = List::new(items).style(Style::default().bg(PANEL_BG));
        frame.render_widget(list, content_layout[1]);

        // Instructions - show "I" for install if tmux config is missing
        let has_missing_config = status.checks.iter().any(|c| {
            c.dependency.category == super::dependency_checker::DependencyCategory::Configuration
                && !c.is_installed
        });

        let instructions = if status.mandatory_met {
            if has_missing_config {
                "Press Enter to continue • I to install tmux config • R to re-check"
            } else {
                "Press Enter to continue"
            }
        } else if has_missing_config {
            "Install required dependencies • I to install tmux config • R to re-check"
        } else {
            "Install required dependencies and press R to re-check"
        };

        let instr_widget = Paragraph::new(Span::styled(instructions, Style::default().fg(MUTED_GRAY)))
            .alignment(Alignment::Center);
        frame.render_widget(instr_widget, content_layout[2]);
    }

    /// Render git directories step
    fn render_git_directories(&self, frame: &mut Frame, area: Rect, state: &OnboardingState) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG))
            .title(" Git Directories ")
            .title_style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let content_layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints([
                Constraint::Length(2),  // Description
                Constraint::Length(3),  // Input field
                Constraint::Length(1),  // Spacer
                Constraint::Min(5),     // Validation results
                Constraint::Length(2),  // Instructions
            ])
            .split(inner);

        // Description
        let desc = Paragraph::new(Line::from(vec![
            Span::styled("Enter paths to your git project directories ", Style::default().fg(SOFT_WHITE)),
            Span::styled("(comma-separated)", Style::default().fg(MUTED_GRAY)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(desc, content_layout[0]);

        // Input field
        let input_text = if state.show_cursor {
            let (before, after) = state.git_directories_input.split_at(state.cursor_position);
            format!("{}│{}", before, after)
        } else {
            state.git_directories_input.clone()
        };

        let input = Paragraph::new(input_text)
            .style(Style::default().fg(SOFT_WHITE))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(GOLD))
                    .style(Style::default().bg(DARK_BG)),
            );
        frame.render_widget(input, content_layout[1]);

        // Validation results
        if !state.validated_directories.is_empty() {
            let mut items: Vec<ListItem> = Vec::new();

            for validated in &state.validated_directories {
                let (icon, color) = if validated.is_valid {
                    ("✓", SELECTION_GREEN)
                } else {
                    ("✗", ERROR_RED)
                };

                let error_text = validated.error.as_ref()
                    .map(|e| format!(" - {}", e))
                    .unwrap_or_default();

                items.push(ListItem::new(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(icon, Style::default().fg(color)),
                    Span::styled(" ", Style::default()),
                    Span::styled(
                        validated.path.display().to_string(),
                        if validated.is_valid {
                            Style::default().fg(SOFT_WHITE)
                        } else {
                            Style::default().fg(MUTED_GRAY)
                        },
                    ),
                    Span::styled(error_text, Style::default().fg(ERROR_RED)),
                ])));
            }

            let list = List::new(items).style(Style::default().bg(PANEL_BG));
            frame.render_widget(list, content_layout[3]);
        }

        // Instructions
        let valid_count = state.validated_directories.iter().filter(|v| v.is_valid).count();
        let instructions = format!(
            "{} valid path(s) • Press Enter to continue",
            valid_count
        );

        let instr_widget = Paragraph::new(Span::styled(instructions, Style::default().fg(MUTED_GRAY)))
            .alignment(Alignment::Center);
        frame.render_widget(instr_widget, content_layout[4]);
    }

    /// Render authentication step
    fn render_authentication(&self, frame: &mut Frame, area: Rect, state: &OnboardingState) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG))
            .title(" Authentication ")
            .title_style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let content = if state.auth_completed {
            vec![
                Line::from(""),
                Line::from(Span::styled("✅ Authentication configured!", Style::default().fg(SELECTION_GREEN))),
                Line::from(""),
                Line::from(Span::styled(
                    format!("Method: {}", state.auth_method.as_deref().unwrap_or("Unknown")),
                    Style::default().fg(MUTED_GRAY),
                )),
                Line::from(""),
                Line::from(Span::styled("Press Enter to continue", Style::default().fg(MUTED_GRAY))),
            ]
        } else {
            vec![
                Line::from(""),
                Line::from(Span::styled("Set up Claude authentication", Style::default().fg(SOFT_WHITE))),
                Line::from(""),
                Line::from(Span::styled("This step will be handled by the existing auth flow.", Style::default().fg(MUTED_GRAY))),
                Line::from(Span::styled("You can skip this and configure later.", Style::default().fg(MUTED_GRAY))),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Press ", Style::default().fg(MUTED_GRAY)),
                    Span::styled("Enter", Style::default().fg(GOLD)),
                    Span::styled(" to configure auth, or ", Style::default().fg(MUTED_GRAY)),
                    Span::styled("S", Style::default().fg(GOLD)),
                    Span::styled(" to skip", Style::default().fg(MUTED_GRAY)),
                ]),
            ]
        };

        let text = Paragraph::new(content).alignment(Alignment::Center);
        frame.render_widget(text, inner);
    }

    /// Render editor selection step
    fn render_editor_selection(&self, frame: &mut Frame, area: Rect, state: &OnboardingState) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG))
            .title(" Editor Selection ")
            .title_style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let content_layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints([
                Constraint::Length(3),  // Description
                Constraint::Min(10),    // Editor list
                Constraint::Length(2),  // Instructions
            ])
            .split(inner);

        // Description
        let desc = Paragraph::new(vec![
            Line::from(Span::styled(
                "Choose your preferred editor for opening sessions",
                Style::default().fg(SOFT_WHITE),
            )),
            Line::from(Span::styled(
                "Use ↑/↓ to select, Enter to continue",
                Style::default().fg(MUTED_GRAY),
            )),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(desc, content_layout[0]);

        // Editor list
        if state.available_editors.is_empty() {
            let msg = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("No editors detected", Style::default().fg(MUTED_GRAY))),
                Line::from(Span::styled(
                    "Will fall back to $EDITOR or 'code' if available",
                    Style::default().fg(MUTED_GRAY),
                )),
            ])
            .alignment(Alignment::Center);
            frame.render_widget(msg, content_layout[1]);
        } else {
            let mut items: Vec<ListItem> = Vec::new();

            for (idx, editor) in state.available_editors.iter().enumerate() {
                let is_selected = idx == state.selected_editor_index;

                let (icon, icon_color) = if !editor.available {
                    ("○", MUTED_GRAY)
                } else if is_selected {
                    ("▶", SELECTION_GREEN)
                } else {
                    ("●", SOFT_WHITE)
                };

                let availability = if editor.available {
                    Span::styled(" ✓ installed", Style::default().fg(SELECTION_GREEN))
                } else {
                    Span::styled(" ✗ not found", Style::default().fg(MUTED_GRAY))
                };

                let name_style = if is_selected && editor.available {
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
                } else if editor.available {
                    Style::default().fg(SOFT_WHITE)
                } else {
                    Style::default().fg(MUTED_GRAY)
                };

                let bg_style = if is_selected {
                    Style::default().bg(Color::Rgb(40, 40, 60))
                } else {
                    Style::default()
                };

                items.push(ListItem::new(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(icon, Style::default().fg(icon_color)),
                    Span::styled(" ", Style::default()),
                    Span::styled(&editor.name, name_style),
                    Span::styled(
                        format!(" ({})", editor.command),
                        Style::default().fg(MUTED_GRAY),
                    ),
                    availability,
                ])).style(bg_style));
            }

            let list = List::new(items).style(Style::default().bg(PANEL_BG));
            frame.render_widget(list, content_layout[1]);
        }

        // Instructions
        let selected_editor = state.get_selected_editor();
        let instructions = if selected_editor.is_some() {
            format!("Selected: {} • Press Enter to continue, or skip to use defaults",
                    state.available_editors.get(state.selected_editor_index)
                        .map(|e| e.name.as_str())
                        .unwrap_or("None"))
        } else {
            "No available editor selected • Press Enter to use fallback (code → $EDITOR)".to_string()
        };

        let instr_widget = Paragraph::new(Span::styled(instructions, Style::default().fg(MUTED_GRAY)))
            .alignment(Alignment::Center);
        frame.render_widget(instr_widget, content_layout[2]);
    }

    /// Render summary step
    fn render_summary(&self, frame: &mut Frame, area: Rect, state: &OnboardingState) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG))
            .title(" Setup Complete ")
            .title_style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let content_layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints([
                Constraint::Length(4),  // Success message
                Constraint::Min(8),     // Summary items
                Constraint::Length(3),  // Finish button
            ])
            .split(inner);

        // Success message
        let success = vec![
            Line::from(Span::styled("🎉", Style::default())),
            Line::from(Span::styled("You're all set!", Style::default().fg(GOLD).add_modifier(Modifier::BOLD))),
        ];
        let success_widget = Paragraph::new(success).alignment(Alignment::Center);
        frame.render_widget(success_widget, content_layout[0]);

        // Summary items
        let mut summary_items = Vec::new();

        // Dependencies
        if let Some(status) = &state.dependency_status {
            summary_items.push(Line::from(vec![
                Span::styled("  ✓ ", Style::default().fg(SELECTION_GREEN)),
                Span::styled("Dependencies: ", Style::default().fg(SOFT_WHITE)),
                Span::styled(
                    format!("{}/{} installed", status.installed_count(), status.total_count()),
                    Style::default().fg(MUTED_GRAY),
                ),
            ]));
        }

        // Git directories
        let valid_dirs = state.get_valid_directories();
        summary_items.push(Line::from(vec![
            Span::styled("  ✓ ", Style::default().fg(SELECTION_GREEN)),
            Span::styled("Git directories: ", Style::default().fg(SOFT_WHITE)),
            Span::styled(
                format!("{} configured", valid_dirs.len()),
                Style::default().fg(MUTED_GRAY),
            ),
        ]));

        // Auth
        let auth_status = if state.auth_completed {
            format!("configured ({})", state.auth_method.as_deref().unwrap_or("unknown"))
        } else {
            "skipped".to_string()
        };
        summary_items.push(Line::from(vec![
            Span::styled(
                if state.auth_completed { "  ✓ " } else { "  ○ " },
                Style::default().fg(if state.auth_completed { SELECTION_GREEN } else { WARNING_YELLOW }),
            ),
            Span::styled("Authentication: ", Style::default().fg(SOFT_WHITE)),
            Span::styled(auth_status, Style::default().fg(MUTED_GRAY)),
        ]));

        // Editor
        let editor_status = state.get_selected_editor()
            .map(|cmd| {
                state.available_editors.iter()
                    .find(|e| e.command == cmd)
                    .map(|e| format!("{} ({})", e.name, e.command))
                    .unwrap_or(cmd)
            })
            .unwrap_or_else(|| "fallback (code → $EDITOR)".to_string());
        summary_items.push(Line::from(vec![
            Span::styled(
                if state.get_selected_editor().is_some() { "  ✓ " } else { "  ○ " },
                Style::default().fg(if state.get_selected_editor().is_some() { SELECTION_GREEN } else { WARNING_YELLOW }),
            ),
            Span::styled("Editor: ", Style::default().fg(SOFT_WHITE)),
            Span::styled(editor_status, Style::default().fg(MUTED_GRAY)),
        ]));

        let summary_widget = Paragraph::new(summary_items);
        frame.render_widget(summary_widget, content_layout[1]);

        // Finish button
        let finish = Paragraph::new(Line::from(vec![
            Span::styled("Press ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Enter", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" to finish and start using AINB", Style::default().fg(MUTED_GRAY)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(finish, content_layout[2]);
    }

    /// Render navigation footer
    fn render_navigation(&self, frame: &mut Frame, area: Rect, state: &OnboardingState) {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(SUBDUED_BORDER))
            .style(Style::default().bg(DARK_BG));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut spans = vec![Span::styled("  ", Style::default())];

        // Back button (↑ works in all steps, ← works in most but not text input)
        if state.can_go_back() {
            spans.push(Span::styled("[", Style::default().fg(SUBDUED_BORDER)));
            spans.push(Span::styled("↑/←", Style::default().fg(GOLD)));
            spans.push(Span::styled("]", Style::default().fg(SUBDUED_BORDER)));
            spans.push(Span::styled(" Back", Style::default().fg(MUTED_GRAY)));
            spans.push(Span::styled("  |  ", Style::default().fg(SUBDUED_BORDER)));
        }

        // Next/Finish button
        let can_advance = state.current_step.can_advance(state);
        let button_text = if state.is_final_step() { "Finish" } else { "Next" };

        spans.push(Span::styled("[", Style::default().fg(SUBDUED_BORDER)));
        spans.push(Span::styled(
            "Enter",
            if can_advance {
                Style::default().fg(GOLD)
            } else {
                Style::default().fg(MUTED_GRAY)
            },
        ));
        spans.push(Span::styled("]", Style::default().fg(SUBDUED_BORDER)));
        spans.push(Span::styled(
            format!(" {}", button_text),
            if can_advance {
                Style::default().fg(SOFT_WHITE)
            } else {
                Style::default().fg(MUTED_GRAY)
            },
        ));

        // Escape hint
        spans.push(Span::styled("  |  ", Style::default().fg(SUBDUED_BORDER)));
        spans.push(Span::styled("[", Style::default().fg(SUBDUED_BORDER)));
        spans.push(Span::styled("Esc", Style::default().fg(GOLD)));
        spans.push(Span::styled("]", Style::default().fg(SUBDUED_BORDER)));
        spans.push(Span::styled(" Cancel", Style::default().fg(MUTED_GRAY)));

        let nav = Paragraph::new(Line::from(spans)).alignment(Alignment::Center);
        frame.render_widget(nav, inner);
    }
}

impl Default for OnboardingComponent {
    fn default() -> Self {
        Self::new()
    }
}
