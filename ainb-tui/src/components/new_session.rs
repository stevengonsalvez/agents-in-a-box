// ABOUTME: New session creation UI component with repository selection and branch input

use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::{
    AppState,
    state::{BranchCheckoutMode, NewSessionState, NewSessionStep, RepoSourceChoice, SshInputFocus},
};

pub struct NewSessionComponent {
    search_list_state: ListState,
    branch_list_state: ListState,
}

impl NewSessionComponent {
    pub fn new() -> Self {
        Self {
            search_list_state: ListState::default(),
            branch_list_state: ListState::default(),
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        if let Some(ref session_state) = state.new_session_state {
            // Create a centered popup
            let popup_area = self.centered_rect(80, 70, area);

            // Clear the background
            frame.render_widget(Clear, popup_area);

            match session_state.step {
                NewSessionStep::SelectSource => {
                    self.render_source_selection(frame, popup_area, session_state)
                }
                NewSessionStep::InputRepoSource => {
                    self.render_repo_source_input(frame, popup_area, session_state)
                }
                NewSessionStep::ValidatingRepo => {
                    self.render_validating_repo(frame, popup_area, session_state)
                }
                NewSessionStep::SelectBranch => {
                    self.render_branch_selection(frame, popup_area, session_state)
                }
                NewSessionStep::SelectRepo => {
                    if state.current_view == crate::app::state::View::SearchWorkspace {
                        self.render_search_workspace(frame, popup_area, session_state)
                    } else {
                        self.render_repo_selection(frame, popup_area, session_state)
                    }
                }
                NewSessionStep::SelectAgent => {
                    self.render_agent_selection(frame, popup_area, session_state)
                }
                NewSessionStep::ConfigureSsh => {
                    self.render_ssh_config(frame, popup_area, session_state)
                }
                NewSessionStep::InputBranch => {
                    self.render_branch_input(frame, popup_area, session_state)
                }
                NewSessionStep::SelectMode => {
                    self.render_mode_selection(frame, popup_area, session_state)
                }
                NewSessionStep::InputPrompt => {
                    self.render_prompt_input(frame, popup_area, session_state)
                }
                NewSessionStep::ConfigurePermissions => {
                    self.render_permissions_config(frame, popup_area, session_state)
                }
                NewSessionStep::Creating => self.render_creating(frame, popup_area),
            }
        }
    }

    /// Render the source selection screen (Local, Remote, or SSH)
    fn render_source_selection(
        &self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
    ) {
        // Draw outer border with modern styling
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(100, 149, 237))) // Cornflower blue
            .title(Span::styled(
                " 🚀 New Session ",
                Style::default()
                    .fg(Color::Rgb(255, 215, 0)) // Gold
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(Color::Rgb(25, 25, 35))); // Dark background
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(5), // Local option card
                Constraint::Length(1), // Spacer
                Constraint::Length(5), // Remote option card
                Constraint::Length(1), // Spacer
                Constraint::Length(5), // SSH option card
                Constraint::Length(1), // Spacer
                Constraint::Length(2), // Instructions
            ])
            .split(inner);

        let is_local = session_state.source_choice == RepoSourceChoice::Local;
        let is_remote = session_state.source_choice == RepoSourceChoice::Remote;
        let is_ssh = session_state.source_choice == RepoSourceChoice::Ssh;

        // Local option card
        let local_border_color = if is_local {
            Color::Rgb(100, 200, 100) // Green when selected
        } else {
            Color::Rgb(70, 70, 90) // Gray when not
        };

        let local_bg = if is_local {
            Color::Rgb(35, 45, 35) // Slightly green tint
        } else {
            Color::Rgb(30, 30, 40)
        };

        let local_text = vec![
            Line::from(vec![
                Span::styled(
                    if is_local { "  ▶ " } else { "    " },
                    Style::default().fg(Color::Rgb(100, 200, 100)),
                ),
                Span::styled("📁 ", Style::default()),
                Span::styled(
                    "[L] Local Repository",
                    Style::default()
                        .fg(if is_local { Color::Rgb(100, 200, 100) } else { Color::Rgb(200, 200, 200) })
                        .add_modifier(Modifier::BOLD),
                ),
                if is_local {
                    Span::styled("  ✓", Style::default().fg(Color::Rgb(100, 200, 100)))
                } else {
                    Span::raw("")
                },
            ]),
            Line::from(vec![
                Span::styled("      ", Style::default()),
                Span::styled("Browse and select from local repositories", Style::default().fg(Color::Rgb(180, 180, 180))),
            ]),
        ];

        let local_para = Paragraph::new(local_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(local_border_color))
                    .style(Style::default().bg(local_bg)),
            );
        frame.render_widget(local_para, chunks[0]);

        // Remote option card
        let remote_border_color = if is_remote {
            Color::Rgb(100, 149, 237) // Blue when selected
        } else {
            Color::Rgb(70, 70, 90) // Gray when not
        };

        let remote_bg = if is_remote {
            Color::Rgb(30, 35, 45) // Slightly blue tint
        } else {
            Color::Rgb(30, 30, 40)
        };

        let remote_text = vec![
            Line::from(vec![
                Span::styled(
                    if is_remote { "  ▶ " } else { "    " },
                    Style::default().fg(Color::Rgb(100, 149, 237)),
                ),
                Span::styled("🌐 ", Style::default()),
                Span::styled(
                    "[R] Remote URL",
                    Style::default()
                        .fg(if is_remote { Color::Rgb(100, 149, 237) } else { Color::Rgb(200, 200, 200) })
                        .add_modifier(Modifier::BOLD),
                ),
                if is_remote {
                    Span::styled("  ✓", Style::default().fg(Color::Rgb(100, 149, 237)))
                } else {
                    Span::raw("")
                },
            ]),
            Line::from(vec![
                Span::styled("      ", Style::default()),
                Span::styled("Clone from GitHub, GitLab, or any Git URL", Style::default().fg(Color::Rgb(180, 180, 180))),
            ]),
        ];

        let remote_para = Paragraph::new(remote_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(remote_border_color))
                    .style(Style::default().bg(remote_bg)),
            );
        frame.render_widget(remote_para, chunks[2]);

        // SSH option card
        let ssh_border_color = if is_ssh {
            Color::Rgb(200, 150, 100) // Warm orange when selected
        } else {
            Color::Rgb(70, 70, 90) // Gray when not
        };

        let ssh_bg = if is_ssh {
            Color::Rgb(40, 35, 30) // Slightly warm tint
        } else {
            Color::Rgb(30, 30, 40)
        };

        let ssh_text = vec![
            Line::from(vec![
                Span::styled(
                    if is_ssh { "  ▶ " } else { "    " },
                    Style::default().fg(Color::Rgb(200, 150, 100)),
                ),
                Span::styled("🔐 ", Style::default()),
                Span::styled(
                    "[S] SSH Connection",
                    Style::default()
                        .fg(if is_ssh { Color::Rgb(200, 150, 100) } else { Color::Rgb(200, 200, 200) })
                        .add_modifier(Modifier::BOLD),
                ),
                if is_ssh {
                    Span::styled("  ✓", Style::default().fg(Color::Rgb(200, 150, 100)))
                } else {
                    Span::raw("")
                },
            ]),
            Line::from(vec![
                Span::styled("      ", Style::default()),
                Span::styled("Connect to a remote server via SSH", Style::default().fg(Color::Rgb(180, 180, 180))),
            ]),
        ];

        let ssh_para = Paragraph::new(ssh_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(ssh_border_color))
                    .style(Style::default().bg(ssh_bg)),
            );
        frame.render_widget(ssh_para, chunks[4]);

        // Styled instructions footer
        let instructions = Line::from(vec![
            Span::styled("  ↑↓ ", Style::default().fg(Color::Rgb(255, 215, 0))),
            Span::styled("Navigate  ", Style::default().fg(Color::Rgb(128, 128, 128))),
            Span::styled("│", Style::default().fg(Color::Rgb(70, 70, 90))),
            Span::styled("  L/R/S ", Style::default().fg(Color::Rgb(255, 215, 0))),
            Span::styled("Quick  ", Style::default().fg(Color::Rgb(128, 128, 128))),
            Span::styled("│", Style::default().fg(Color::Rgb(70, 70, 90))),
            Span::styled("  ⏎ ", Style::default().fg(Color::Rgb(100, 200, 100))),
            Span::styled("Select  ", Style::default().fg(Color::Rgb(128, 128, 128))),
            Span::styled("│", Style::default().fg(Color::Rgb(70, 70, 90))),
            Span::styled("  Esc ", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::styled("Cancel", Style::default().fg(Color::Rgb(128, 128, 128))),
        ]);

        let instructions_widget = Paragraph::new(instructions)
            .alignment(Alignment::Center)
            .style(Style::default().bg(Color::Rgb(25, 25, 35)));
        frame.render_widget(instructions_widget, chunks[6]);
    }

    /// Render the repo source input screen (URL or local path)
    fn render_repo_source_input(
        &self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
    ) {
        // Modern color palette
        let cornflower_blue = Color::Rgb(100, 149, 237);
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let soft_white = Color::Rgb(220, 220, 230);
        let muted_gray = Color::Rgb(120, 120, 140);
        let selection_green = Color::Rgb(100, 200, 100);
        let subdued_border = Color::Rgb(60, 60, 80);

        // Clear background
        let background = Block::default().style(Style::default().bg(dark_bg));
        frame.render_widget(background, area);

        // Main dialog with rounded border
        let title_line = Line::from(vec![
            Span::styled(" 🌐 ", Style::default().fg(gold)),
            Span::styled("New Session", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" ", Style::default()),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(title_line)
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        // Use taller box when showing error to accommodate wrapped text
        let hints_height = if session_state.repo_validation_error.is_some() { 8 } else { 6 };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints(vec![
                Constraint::Length(2), // Subtitle
                Constraint::Length(1), // Spacer
                Constraint::Length(3), // Input field
                Constraint::Length(1), // Spacer
                Constraint::Length(hints_height), // Examples/hints (taller for errors)
                Constraint::Length(1), // Spacer
                Constraint::Min(0),    // Recent repos (if any)
                Constraint::Length(2), // Footer
            ])
            .split(inner);

        // Subtitle
        let subtitle = Paragraph::new(Line::from(vec![
            Span::styled("Enter a repository URL, GitHub shorthand, or local path", Style::default().fg(muted_gray)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(subtitle, chunks[0]);

        // Input field with cursor
        let input_text = if session_state.repo_input.is_empty() {
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    "github.com/owner/repo or /path/to/repo...",
                    Style::default().fg(muted_gray).add_modifier(Modifier::ITALIC),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    &session_state.repo_input,
                    Style::default().fg(soft_white).add_modifier(Modifier::BOLD),
                ),
                Span::styled("█", Style::default().fg(selection_green)),
            ])
        };

        // Show error if present
        let input_border_color = if session_state.repo_validation_error.is_some() {
            Color::Rgb(255, 100, 100) // Red for error
        } else {
            selection_green
        };

        let input = Paragraph::new(input_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(input_border_color))
                    .title(Span::styled(" Repository ", Style::default().fg(input_border_color)))
                    .style(Style::default().bg(Color::Rgb(35, 35, 45))),
            );
        frame.render_widget(input, chunks[2]);

        // Examples/hints box
        let example_lines = if let Some(ref error) = session_state.repo_validation_error {
            vec![
                Line::from(vec![
                    Span::styled("  ❌ ", Style::default().fg(Color::Rgb(255, 100, 100))),
                    Span::styled("Error", Style::default().fg(Color::Rgb(255, 100, 100)).add_modifier(Modifier::BOLD)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("     ", Style::default()),
                    Span::styled(error, Style::default().fg(Color::Rgb(255, 150, 150))),
                ]),
            ]
        } else {
            vec![
                Line::from(vec![
                    Span::styled("  💡 ", Style::default().fg(cornflower_blue)),
                    Span::styled("Supported formats:", Style::default().fg(cornflower_blue)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  • ", Style::default().fg(muted_gray)),
                    Span::styled("owner/repo", Style::default().fg(gold)),
                    Span::styled(" → GitHub shorthand", Style::default().fg(muted_gray)),
                ]),
                Line::from(vec![
                    Span::styled("  • ", Style::default().fg(muted_gray)),
                    Span::styled("https://github.com/owner/repo", Style::default().fg(gold)),
                    Span::styled(" → HTTPS URL", Style::default().fg(muted_gray)),
                ]),
                Line::from(vec![
                    Span::styled("  • ", Style::default().fg(muted_gray)),
                    Span::styled("git@github.com:owner/repo.git", Style::default().fg(gold)),
                    Span::styled(" → SSH URL", Style::default().fg(muted_gray)),
                ]),
                Line::from(vec![
                    Span::styled("  • ", Style::default().fg(muted_gray)),
                    Span::styled("/path/to/local/repo", Style::default().fg(gold)),
                    Span::styled(" → Local path", Style::default().fg(muted_gray)),
                ]),
            ]
        };

        let examples = Paragraph::new(example_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(subdued_border))
                    .style(Style::default().bg(dark_bg)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(examples, chunks[4]);

        // Recent repos section (if any)
        if !session_state.recent_repos.is_empty() {
            let recent_items: Vec<Line> = session_state
                .recent_repos
                .iter()
                .take(5)
                .enumerate()
                .map(|(idx, repo)| {
                    Line::from(vec![
                        Span::styled(format!("  {} ", idx + 1), Style::default().fg(muted_gray)),
                        Span::styled(&repo.owner, Style::default().fg(soft_white)),
                        Span::styled("/", Style::default().fg(muted_gray)),
                        Span::styled(&repo.repo_name, Style::default().fg(cornflower_blue)),
                    ])
                })
                .collect();

            let recent_para = Paragraph::new(recent_items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(subdued_border))
                        .title(Span::styled(" 📜 Recent Repos ", Style::default().fg(muted_gray)))
                        .style(Style::default().bg(dark_bg)),
                );
            frame.render_widget(recent_para, chunks[6]);
        }

        // Footer
        let footer_spans = vec![
            Span::styled("Type", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Input", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(subdued_border)),
            Span::styled("Enter", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Submit", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(subdued_border)),
            Span::styled("Esc", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::styled(" Cancel", Style::default().fg(muted_gray)),
        ];

        let footer = Paragraph::new(Line::from(footer_spans))
            .alignment(Alignment::Center);
        frame.render_widget(footer, chunks[7]);
    }

    /// Render the validating/cloning progress screen
    fn render_validating_repo(
        &self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
    ) {
        // Modern color palette
        let cornflower_blue = Color::Rgb(100, 149, 237);
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let soft_white = Color::Rgb(220, 220, 230);
        let muted_gray = Color::Rgb(120, 120, 140);
        let progress_cyan = Color::Rgb(100, 200, 230);

        // Clear background
        let background = Block::default().style(Style::default().bg(dark_bg));
        frame.render_widget(background, area);

        // Main dialog with rounded border
        let title_line = Line::from(vec![
            Span::styled(" ⏳ ", Style::default().fg(progress_cyan)),
            Span::styled("Validating Repository", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" ", Style::default()),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(title_line)
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(2), // Subtitle
                Constraint::Min(0),    // Progress content
                Constraint::Length(2), // Footer
            ])
            .split(inner);

        // Subtitle - show what we're checking
        let repo_display = session_state.repo_source
            .as_ref()
            .map(|s| s.display_name())
            .unwrap_or_else(|| session_state.repo_input.clone());

        let subtitle = Paragraph::new(Line::from(vec![
            Span::styled("Checking ", Style::default().fg(muted_gray)),
            Span::styled(&repo_display, Style::default().fg(cornflower_blue).add_modifier(Modifier::BOLD)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(subtitle, chunks[0]);

        // Progress content
        let progress_lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  🔄 ", Style::default().fg(progress_cyan)),
                Span::styled("Connecting to repository...", Style::default().fg(soft_white)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  📡 ", Style::default().fg(cornflower_blue)),
                Span::styled("Fetching branch information...", Style::default().fg(soft_white)),
            ]),
            Line::from(""),
            Line::from(""),
            Line::from(vec![
                Span::styled("       This may take a moment...", Style::default().fg(muted_gray).add_modifier(Modifier::ITALIC)),
            ]),
        ];

        let progress = Paragraph::new(progress_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Rgb(60, 60, 80)))
                    .style(Style::default().bg(dark_bg)),
            );
        frame.render_widget(progress, chunks[1]);

        // Footer
        let footer = Paragraph::new(Line::from(vec![
            Span::styled("⏳ ", Style::default().fg(progress_cyan)),
            Span::styled("Please wait", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 60, 80))),
            Span::styled("Esc", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Cancel", Style::default().fg(muted_gray)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(footer, chunks[2]);
    }

    /// Render the branch selection screen
    fn render_branch_selection(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
    ) {
        // Modern color palette
        let cornflower_blue = Color::Rgb(100, 149, 237);
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let soft_white = Color::Rgb(220, 220, 230);
        let muted_gray = Color::Rgb(120, 120, 140);
        let selection_green = Color::Rgb(100, 200, 100);
        let subdued_border = Color::Rgb(60, 60, 80);
        let list_highlight_bg = Color::Rgb(40, 40, 60);

        // Clear background
        let background = Block::default().style(Style::default().bg(dark_bg));
        frame.render_widget(background, area);

        // Main dialog with rounded border
        let repo_display = session_state.repo_source
            .as_ref()
            .map(|s| s.display_name())
            .unwrap_or_default();

        let title_line = Line::from(vec![
            Span::styled(" 🌿 ", Style::default().fg(gold)),
            Span::styled("Select Branch", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" ", Style::default()),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(title_line)
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints(vec![
                Constraint::Length(2), // Repo info
                Constraint::Length(4), // Mode toggle box (radio options)
                Constraint::Length(3), // Filter input
                Constraint::Min(0),    // Branch list
                Constraint::Length(2), // Footer
            ])
            .split(inner);

        // Repo info with mode-specific guidance
        let guidance_text = match session_state.branch_checkout_mode {
            BranchCheckoutMode::CreateNew => "Choose a base branch to create worktree from",
            BranchCheckoutMode::CheckoutExisting => "Choose a remote branch to checkout directly",
        };
        let repo_info = Paragraph::new(Line::from(vec![
            Span::styled("📁 ", Style::default()),
            Span::styled(&repo_display, Style::default().fg(cornflower_blue).add_modifier(Modifier::BOLD)),
            Span::styled(" → ", Style::default().fg(muted_gray)),
            Span::styled(guidance_text, Style::default().fg(muted_gray)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(repo_info, chunks[0]);

        // Mode toggle - radio-style options (both visible)
        let create_selected = matches!(session_state.branch_checkout_mode, BranchCheckoutMode::CreateNew);
        let existing_selected = !create_selected;

        let create_indicator = if create_selected { "◉" } else { "○" };
        let existing_indicator = if existing_selected { "◉" } else { "○" };

        let create_style = if create_selected {
            Style::default().fg(selection_green).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(soft_white)
        };
        let existing_style = if existing_selected {
            Style::default().fg(cornflower_blue).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(soft_white)
        };

        let mode_lines = vec![
            Line::from(vec![
                Span::styled(format!(" {} ", create_indicator), Style::default().fg(if create_selected { selection_green } else { muted_gray })),
                Span::styled("🌱 ", Style::default().fg(selection_green)),
                Span::styled("Create new branch", create_style),
                Span::styled(" — ", Style::default().fg(muted_gray)),
                Span::styled("create worktree from base", Style::default().fg(muted_gray)),
            ]),
            Line::from(vec![
                Span::styled(format!(" {} ", existing_indicator), Style::default().fg(if existing_selected { cornflower_blue } else { muted_gray })),
                Span::styled("📥 ", Style::default().fg(cornflower_blue)),
                Span::styled("Checkout existing", existing_style),
                Span::styled(" — ", Style::default().fg(muted_gray)),
                Span::styled("use remote branch as-is", Style::default().fg(muted_gray)),
            ]),
            Line::from(vec![
                Span::styled("Tab", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
                Span::styled(" to switch mode", Style::default().fg(muted_gray)),
            ]),
        ];

        let mode_title = Line::from(vec![
            Span::styled(" 🔀 ", Style::default().fg(gold)),
            Span::styled("Branch Checkout Mode", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" ", Style::default()),
        ]);

        let mode_widget = Paragraph::new(mode_lines)
            .alignment(Alignment::Left)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(subdued_border))
                    .title(mode_title)
                    .style(Style::default().bg(dark_bg)),
            );
        frame.render_widget(mode_widget, chunks[1]);

        // Filter input
        let filter_text = if session_state.branch_filter_text.is_empty() {
            Span::styled("Type to filter branches...", Style::default().fg(muted_gray).add_modifier(Modifier::ITALIC))
        } else {
            Span::styled(&session_state.branch_filter_text, Style::default().fg(soft_white))
        };
        let filter_input = Paragraph::new(Line::from(vec![
            Span::styled("🔍 ", Style::default()),
            filter_text,
            Span::styled("│", Style::default().fg(cornflower_blue)), // Cursor
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(subdued_border))
                .style(Style::default().bg(dark_bg)),
        );
        frame.render_widget(filter_input, chunks[2]);

        // Branch list - use filtered_branches
        let branch_items: Vec<ListItem> = session_state
            .filtered_branches
            .iter()
            .enumerate()
            .map(|(display_idx, (_orig_idx, branch))| {
                let is_selected = display_idx == session_state.selected_branch_index;

                let mut spans = vec![];

                // Selection indicator
                if is_selected {
                    spans.push(Span::styled("  ▶ ", Style::default().fg(selection_green)));
                } else {
                    spans.push(Span::raw("    "));
                }

                // Branch icon
                let branch_icon = if branch.is_default { "★" } else { "○" };
                spans.push(Span::styled(
                    format!("{} ", branch_icon),
                    Style::default().fg(if branch.is_default { gold } else { muted_gray }),
                ));

                // Branch name
                let name_style = if is_selected {
                    Style::default().fg(selection_green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(soft_white)
                };
                spans.push(Span::styled(&branch.name, name_style));

                // Default badge
                if branch.is_default {
                    spans.push(Span::styled(
                        " [default]",
                        Style::default().fg(gold).add_modifier(Modifier::ITALIC),
                    ));
                }

                // Commit hash (truncated)
                spans.push(Span::styled(
                    format!("  {}", &branch.commit_hash[..7.min(branch.commit_hash.len())]),
                    Style::default().fg(muted_gray),
                ));

                ListItem::new(Line::from(spans))
            })
            .collect();

        let total_count = session_state.remote_branches.len();
        let filtered_count = session_state.filtered_branches.len();
        let count_text = if session_state.branch_filter_text.is_empty() {
            format!("Branches ({})", total_count)
        } else {
            format!("Branches ({}/{})", filtered_count, total_count)
        };
        let list_title = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(count_text, Style::default().fg(cornflower_blue)),
            Span::styled(" ", Style::default()),
        ]);

        let branch_list = List::new(branch_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(subdued_border))
                    .title(list_title)
                    .style(Style::default().bg(dark_bg)),
            )
            .highlight_style(Style::default().bg(list_highlight_bg));

        // Update list state for proper scrolling
        self.branch_list_state.select(Some(session_state.selected_branch_index));
        frame.render_stateful_widget(branch_list, chunks[3], &mut self.branch_list_state);

        // Footer
        let footer_spans = vec![
            Span::styled("↑↓", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Nav", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(subdued_border)),
            Span::styled("Tab", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Mode", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(subdued_border)),
            Span::styled("Type", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Filter", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(subdued_border)),
            Span::styled("Enter", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Select", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(subdued_border)),
            Span::styled("Esc", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::styled(" Back", Style::default().fg(muted_gray)),
        ];

        let footer = Paragraph::new(Line::from(footer_spans))
            .alignment(Alignment::Center);
        frame.render_widget(footer, chunks[4]);
    }

    fn render_repo_selection(
        &self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
    ) {
        // Modern color palette
        let cornflower_blue = Color::Rgb(100, 149, 237);
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let soft_white = Color::Rgb(220, 220, 230);
        let muted_gray = Color::Rgb(120, 120, 140);
        let selection_green = Color::Rgb(100, 200, 100);
        let subdued_border = Color::Rgb(60, 60, 80);
        let list_highlight_bg = Color::Rgb(40, 40, 60);

        // Clear background
        let background = Block::default().style(Style::default().bg(dark_bg));
        frame.render_widget(background, area);

        // Main dialog with rounded border
        let title_line = Line::from(vec![
            Span::styled(" 📁 ", Style::default().fg(gold)),
            Span::styled("Select Repository", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" ", Style::default()),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(title_line)
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints(vec![
                Constraint::Length(2), // Subtitle
                Constraint::Min(0),    // Repository list
                Constraint::Length(1), // Spacer
                Constraint::Length(2), // Footer
            ])
            .split(inner);

        // Subtitle
        let subtitle = Paragraph::new(Line::from(vec![
            Span::styled("Choose a repository to create a new session", Style::default().fg(muted_gray)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(subtitle, chunks[0]);

        // Repository list with modern styling
        let repos: Vec<ListItem> = if session_state.filtered_repos.is_empty() {
            vec![
                ListItem::new(Line::from(vec![
                    Span::styled("  ⚠️  ", Style::default().fg(gold)),
                    Span::styled("No repositories found in default paths", Style::default().fg(muted_gray)),
                ])),
                ListItem::new(Line::from(vec![
                    Span::styled("     Try searching in common directories like:", Style::default().fg(muted_gray)),
                ])),
                ListItem::new(Line::from(vec![
                    Span::styled("     ~/projects, ~/code, ~/dev, ~/src", Style::default().fg(cornflower_blue)),
                ])),
                ListItem::new(Line::from(vec![
                    Span::styled("  💡 ", Style::default().fg(gold)),
                    Span::styled("Type to filter or add custom paths", Style::default().fg(gold)),
                ])),
            ]
        } else {
            session_state
                .filtered_repos
                .iter()
                .enumerate()
                .map(|(display_idx, (_, repo))| {
                    let repo_name = repo.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");

                    if Some(display_idx) == session_state.selected_repo_index {
                        ListItem::new(Line::from(vec![
                            Span::styled("  ▶ ", Style::default().fg(selection_green)),
                            Span::styled(repo_name, Style::default().fg(selection_green).add_modifier(Modifier::BOLD)),
                        ]))
                    } else {
                        ListItem::new(Line::from(vec![
                            Span::styled("    ", Style::default()),
                            Span::styled(repo_name, Style::default().fg(soft_white)),
                        ]))
                    }
                })
                .collect()
        };

        let repo_count = session_state.filtered_repos.len();
        let list_title = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(format!("Repositories ({})", repo_count), Style::default().fg(cornflower_blue)),
            Span::styled(" ", Style::default()),
        ]);

        let repo_list = List::new(repos)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(subdued_border))
                    .title(list_title)
                    .style(Style::default().bg(dark_bg)),
            )
            .highlight_style(Style::default().bg(list_highlight_bg));

        frame.render_widget(repo_list, chunks[1]);

        // Modern footer with keyboard hints (simplified - no agent/model here)
        let footer_spans = vec![
            Span::styled("↑↓", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Repos", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(subdued_border)),
            Span::styled("Enter", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Select", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(subdued_border)),
            Span::styled("Esc", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::styled(" Cancel", Style::default().fg(muted_gray)),
        ];

        let footer = Paragraph::new(Line::from(footer_spans))
            .alignment(Alignment::Center);
        frame.render_widget(footer, chunks[3]);
    }

    fn render_search_workspace(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
    ) {
        // Color palette
        let cornflower_blue = Color::Rgb(100, 149, 237);
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let muted_gray = Color::Rgb(128, 128, 128);
        let selection_green = Color::Rgb(100, 200, 100);
        let subdued_border = Color::Rgb(70, 70, 90);
        let list_highlight_bg = Color::Rgb(45, 45, 60);

        // Draw outer border with gradient-like effect using rounded corners
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(Span::styled(
                " 🔍 Search Repositories ",
                Style::default()
                    .fg(gold)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints(vec![
                Constraint::Length(3), // Search input
                Constraint::Length(1), // Spacer
                Constraint::Min(0),    // Repository list
                Constraint::Length(1), // Spacer
                Constraint::Length(2), // Instructions
            ])
            .split(inner);

        // Search input with icon and styled placeholder
        let search_text = if session_state.filter_text.is_empty() {
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    "Type to search repositories...",
                    Style::default().fg(muted_gray).add_modifier(Modifier::ITALIC),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled("  ", Style::default().fg(selection_green)),
                Span::styled(
                    &session_state.filter_text,
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
                Span::styled("█", Style::default().fg(selection_green)), // Cursor
            ])
        };

        let search_input = Paragraph::new(search_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(selection_green))
                    .style(Style::default().bg(Color::Rgb(35, 35, 45))),
            );
        frame.render_widget(search_input, chunks[0]);

        // Repository list with enhanced styling
        let total_repos = session_state.available_repos.len();
        let filtered_count = session_state.filtered_repos.len();

        let repos: Vec<ListItem> = session_state
            .filtered_repos
            .iter()
            .enumerate()
            .map(|(display_idx, (_, repo))| {
                let repo_name = repo.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
                let parent_path = repo.parent()
                    .and_then(|p| p.to_str())
                    .map(|s| {
                        // Truncate long paths
                        if s.len() > 50 {
                            format!("...{}", &s[s.len()-47..])
                        } else {
                            s.to_string()
                        }
                    })
                    .unwrap_or_default();

                let is_selected = Some(display_idx) == session_state.selected_repo_index;

                if is_selected {
                    // Selected item - highlighted with arrow and full styling
                    let lines = vec![
                        Line::from(vec![
                            Span::styled("  ▶ ", Style::default().fg(gold)),
                            Span::styled("📁 ", Style::default()),
                            Span::styled(
                                repo_name,
                                Style::default()
                                    .fg(gold)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                        Line::from(vec![
                            Span::styled("      ", Style::default()),
                            Span::styled(
                                parent_path,
                                Style::default().fg(Color::Rgb(150, 150, 150)).add_modifier(Modifier::ITALIC),
                            ),
                        ]),
                    ];
                    ListItem::new(lines).style(Style::default().bg(list_highlight_bg))
                } else {
                    // Non-selected item
                    let lines = vec![
                        Line::from(vec![
                            Span::styled("    ", Style::default()),
                            Span::styled("📂 ", Style::default()),
                            Span::styled(
                                repo_name,
                                Style::default().fg(Color::Rgb(200, 200, 200)),
                            ),
                        ]),
                        Line::from(vec![
                            Span::styled("      ", Style::default()),
                            Span::styled(
                                parent_path,
                                Style::default().fg(Color::Rgb(100, 100, 100)),
                            ),
                        ]),
                    ];
                    ListItem::new(lines)
                }
            })
            .collect();

        // Title with count badge
        let count_style = if filtered_count < total_repos {
            Style::default().fg(Color::Rgb(255, 165, 0)) // Orange when filtered
        } else {
            Style::default().fg(selection_green) // Green when showing all
        };

        let title_spans = vec![
            Span::styled(" Repositories ", Style::default().fg(Color::Rgb(200, 200, 200))),
            Span::styled(
                format!("({}/{})", filtered_count, total_repos),
                count_style.add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ];

        let repo_list = List::new(repos)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(subdued_border))
                    .title(Line::from(title_spans))
                    .style(Style::default().bg(Color::Rgb(30, 30, 40))),
            );

        // Update the list state to match the current selection
        self.search_list_state.select(session_state.selected_repo_index);

        frame.render_stateful_widget(repo_list, chunks[2], &mut self.search_list_state);

        // Styled instructions footer (simplified - no agent/model here)
        let instruction_spans = vec![
            Span::styled("  ⌨️  ", Style::default()),
            Span::styled("Type", Style::default().fg(selection_green)),
            Span::styled(" filter  ", Style::default().fg(muted_gray)),
            Span::styled("│", Style::default().fg(subdued_border)),
            Span::styled("  ↑↓ ", Style::default().fg(selection_green)),
            Span::styled("Repos  ", Style::default().fg(muted_gray)),
            Span::styled("│", Style::default().fg(subdued_border)),
            Span::styled("  ⏎ ", Style::default().fg(selection_green)),
            Span::styled("Select  ", Style::default().fg(muted_gray)),
            Span::styled("│", Style::default().fg(subdued_border)),
            Span::styled("  Esc ", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::styled("Cancel", Style::default().fg(muted_gray)),
        ];

        let instructions = Line::from(instruction_spans);
        let instructions_widget = Paragraph::new(instructions)
            .alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(instructions_widget, chunks[4]);
    }

    /// Render a compact inline agent selector (single line)
    fn render_inline_agent_selector(
        &self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
        colors: &[Color],
    ) {
        let [gold, soft_white, muted_gray, selection_green, _list_highlight_bg] = colors[..] else {
            return;
        };

        let mut spans = vec![
            Span::styled("Agent: ", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
        ];

        for (idx, option) in session_state.agent_options.iter().enumerate() {
            let is_selected = idx == session_state.selected_agent_index;
            let agent = option.agent_type;
            let is_available = agent.is_available();

            if idx > 0 {
                spans.push(Span::styled("  │  ", Style::default().fg(muted_gray)));
            }

            if is_selected {
                spans.push(Span::styled("▶ ", Style::default().fg(selection_green)));
                spans.push(Span::styled(
                    format!("{} ", agent.icon()),
                    Style::default().fg(soft_white),
                ));
                spans.push(Span::styled(
                    agent.name(),
                    Style::default().fg(selection_green).add_modifier(Modifier::BOLD),
                ));
            } else if is_available {
                spans.push(Span::styled(
                    format!("{} {}", agent.icon(), agent.name()),
                    Style::default().fg(soft_white),
                ));
            } else {
                spans.push(Span::styled(
                    format!("{} {} [Soon]", agent.icon(), agent.name()),
                    Style::default().fg(Color::Rgb(80, 80, 100)),
                ));
            }
        }

        let agent_line = Paragraph::new(Line::from(spans))
            .alignment(Alignment::Center);
        frame.render_widget(agent_line, area);
    }

    /// Render a compact inline model selector (single line)
    fn render_inline_model_selector(
        &self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
        colors: &[Color],
    ) {
        let [gold, soft_white, muted_gray, selection_green, _list_highlight_bg] = colors[..] else {
            return;
        };

        let mut spans = vec![
            Span::styled("Model: ", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
        ];

        for (idx, model) in session_state.model_options.iter().enumerate() {
            let is_selected = idx == session_state.selected_model_index;

            if idx > 0 {
                spans.push(Span::styled("  │  ", Style::default().fg(muted_gray)));
            }

            if is_selected {
                spans.push(Span::styled("◀ ", Style::default().fg(selection_green)));
                spans.push(Span::styled(
                    format!("{} ", model.icon()),
                    Style::default().fg(soft_white),
                ));
                spans.push(Span::styled(
                    model.display_name(),
                    Style::default().fg(selection_green).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(" ▶", Style::default().fg(selection_green)));
            } else {
                spans.push(Span::styled(
                    format!("{} {}", model.icon(), model.display_name()),
                    Style::default().fg(soft_white),
                ));
            }
        }

        let model_line = Paragraph::new(Line::from(spans))
            .alignment(Alignment::Center);
        frame.render_widget(model_line, area);
    }

    /// Render the SSH connection configuration screen
    fn render_ssh_config(
        &self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
    ) {
        // Modern color palette (from TUI style guide)
        let cornflower_blue = Color::Rgb(100, 149, 237);
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let soft_white = Color::Rgb(220, 220, 230);
        let muted_gray = Color::Rgb(120, 120, 140);
        let selection_green = Color::Rgb(100, 200, 100);
        let subdued_border = Color::Rgb(60, 60, 80);
        let input_bg = Color::Rgb(35, 35, 50);
        let focus_border = Color::Rgb(100, 200, 100);

        // Draw outer border with modern styling
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(Span::styled(
                " 🔐 SSH Connection ",
                Style::default()
                    .fg(gold)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(1), // Header text (reduced)
                Constraint::Length(1), // Spacer
                Constraint::Length(3), // Host input
                Constraint::Length(3), // Port input (no spacer before)
                Constraint::Length(3), // User input (no spacer before)
                Constraint::Length(3), // Identity file input (no spacer before)
                Constraint::Length(1), // Spacer
                Constraint::Length(3), // Command preview
                Constraint::Length(1), // Spacer
                Constraint::Length(1), // Instructions (reduced)
            ])
            .split(inner);

        // Header text
        let header = Paragraph::new(Line::from(vec![
            Span::styled("Configure SSH connection details", Style::default().fg(soft_white)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(header, chunks[0]);

        // Helper to create input field
        let render_input_field = |frame: &mut Frame, area: Rect, label: &str, value: &str, is_focused: bool, is_required: bool| {
            let border_color = if is_focused { focus_border } else { subdued_border };
            let label_suffix = if is_required { " *" } else { "" };

            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border_color))
                .title(Span::styled(
                    format!(" {}{} ", label, label_suffix),
                    Style::default().fg(if is_focused { selection_green } else { muted_gray }),
                ))
                .style(Style::default().bg(input_bg));

            let display_value = if value.is_empty() && !is_focused {
                match label {
                    "Host" => "(e.g., server.example.com)".to_string(),
                    "Port" => "22".to_string(),
                    "User" => "(optional)".to_string(),
                    "Identity File" => "(optional, e.g., ~/.ssh/id_rsa)".to_string(),
                    _ => String::new(),
                }
            } else {
                value.to_string()
            };

            let text_style = if value.is_empty() && !is_focused {
                Style::default().fg(muted_gray).add_modifier(Modifier::ITALIC)
            } else {
                Style::default().fg(soft_white)
            };

            // Add cursor if focused
            let display_with_cursor = if is_focused {
                format!("{}▎", display_value)
            } else {
                display_value
            };

            let input = Paragraph::new(display_with_cursor)
                .style(text_style)
                .block(input_block);

            frame.render_widget(input, area);
        };

        // Host input (required)
        render_input_field(
            frame,
            chunks[2],
            "Host",
            &session_state.ssh_host,
            session_state.ssh_input_focus == SshInputFocus::Host,
            true,
        );

        // Port input
        render_input_field(
            frame,
            chunks[3],
            "Port",
            &session_state.ssh_port,
            session_state.ssh_input_focus == SshInputFocus::Port,
            false,
        );

        // User input
        render_input_field(
            frame,
            chunks[4],
            "User",
            &session_state.ssh_user,
            session_state.ssh_input_focus == SshInputFocus::User,
            false,
        );

        // Identity file input
        render_input_field(
            frame,
            chunks[5],
            "Identity File",
            &session_state.ssh_identity_file,
            session_state.ssh_input_focus == SshInputFocus::IdentityFile,
            false,
        );

        // Command preview
        let preview_cmd = self.build_ssh_preview(session_state);
        let preview_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(subdued_border))
            .title(Span::styled(
                " Preview ",
                Style::default().fg(muted_gray),
            ))
            .style(Style::default().bg(dark_bg));

        let preview = Paragraph::new(preview_cmd)
            .style(Style::default().fg(Color::Rgb(150, 200, 150)))
            .block(preview_block);
        frame.render_widget(preview, chunks[7]);

        // Instructions
        let instruction_spans = vec![
            Span::styled(" Tab ", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled("Next field  ", Style::default().fg(muted_gray)),
            Span::styled("│", Style::default().fg(subdued_border)),
            Span::styled(" Enter ", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled("Connect  ", Style::default().fg(muted_gray)),
            Span::styled("│", Style::default().fg(subdued_border)),
            Span::styled(" Esc ", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::styled("Back", Style::default().fg(muted_gray)),
        ];

        let instructions = Line::from(instruction_spans);
        let instructions_widget = Paragraph::new(instructions)
            .alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(instructions_widget, chunks[9]);
    }

    /// Build SSH command preview string for display
    fn build_ssh_preview(&self, session_state: &NewSessionState) -> String {
        let mut cmd = String::from("ssh");

        let port: u16 = session_state.ssh_port.parse().unwrap_or(22);
        if port != 22 && !session_state.ssh_port.is_empty() {
            cmd.push_str(&format!(" -p {}", port));
        }

        if !session_state.ssh_identity_file.is_empty() {
            cmd.push_str(&format!(" -i {}", session_state.ssh_identity_file));
        }

        if !session_state.ssh_user.is_empty() {
            cmd.push_str(&format!(" {}@{}", session_state.ssh_user,
                if session_state.ssh_host.is_empty() { "<host>" } else { &session_state.ssh_host }));
        } else if !session_state.ssh_host.is_empty() {
            cmd.push_str(&format!(" {}", session_state.ssh_host));
        } else {
            cmd.push_str(" <host>");
        }

        cmd
    }

    fn render_agent_selection(
        &self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
    ) {
        // Modern color palette (from TUI style guide)
        let cornflower_blue = Color::Rgb(100, 149, 237);
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let soft_white = Color::Rgb(220, 220, 230);
        let muted_gray = Color::Rgb(120, 120, 140);
        let selection_green = Color::Rgb(100, 200, 100);
        let subdued_border = Color::Rgb(60, 60, 80);
        let coming_soon_gray = Color::Rgb(80, 80, 100);
        let list_highlight_bg = Color::Rgb(40, 40, 60);

        // Draw outer border with modern styling
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(Span::styled(
                " 🤖 Select Agent ",
                Style::default()
                    .fg(gold)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        // Determine if model selection should be shown
        let show_model = session_state.should_show_model_selection();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints(if show_model {
                vec![
                    Constraint::Length(2), // Header text
                    Constraint::Length(1), // Spacer
                    Constraint::Min(0),    // Agent list
                    Constraint::Length(1), // Spacer
                    Constraint::Length(3), // Model selection bar
                    Constraint::Length(1), // Spacer
                    Constraint::Length(2), // Instructions
                ]
            } else {
                vec![
                    Constraint::Length(2), // Header text
                    Constraint::Length(1), // Spacer
                    Constraint::Min(0),    // Agent list
                    Constraint::Length(1), // Spacer
                    Constraint::Length(2), // Instructions
                ]
            })
            .split(inner);

        // Header text
        let header = Paragraph::new(Line::from(vec![
            Span::styled("Choose an AI agent for this session", Style::default().fg(soft_white)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(header, chunks[0]);

        // Agent list
        let agent_area = chunks[2];
        self.render_agent_list(frame, agent_area, session_state, &[
            cornflower_blue, dark_bg, gold, soft_white, muted_gray, selection_green,
            subdued_border, coming_soon_gray, list_highlight_bg,
        ]);

        // Model selection bar (if Claude is selected)
        if show_model {
            self.render_model_selector(frame, chunks[4], session_state, &[
                cornflower_blue, dark_bg, gold, soft_white, muted_gray, selection_green,
                subdued_border, list_highlight_bg,
            ]);
        }

        // Instructions
        let instruction_area = if show_model { chunks[6] } else { chunks[4] };
        let mut instruction_spans = vec![
            Span::styled(" ↑↓ ", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled("Agent  ", Style::default().fg(muted_gray)),
        ];

        if show_model {
            instruction_spans.extend(vec![
                Span::styled("│", Style::default().fg(subdued_border)),
                Span::styled(" ←→ ", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
                Span::styled("Model  ", Style::default().fg(muted_gray)),
            ]);
        }

        instruction_spans.extend(vec![
            Span::styled("│", Style::default().fg(subdued_border)),
            Span::styled(" Enter ", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled("Confirm  ", Style::default().fg(muted_gray)),
            Span::styled("│", Style::default().fg(subdued_border)),
            Span::styled(" Esc ", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::styled("Cancel", Style::default().fg(muted_gray)),
        ]);

        let instructions = Line::from(instruction_spans);
        let instructions_widget = Paragraph::new(instructions)
            .alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(instructions_widget, instruction_area);
    }

    fn render_agent_list(
        &self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
        colors: &[Color],
    ) {
        let [_cornflower_blue, dark_bg, _gold, soft_white, muted_gray, selection_green,
            subdued_border, coming_soon_gray, list_highlight_bg] = colors[..] else {
            return;
        };

        // Build agent list items
        let items: Vec<ListItem> = session_state
            .agent_options
            .iter()
            .enumerate()
            .map(|(idx, option)| {
                let is_selected = idx == session_state.selected_agent_index;
                let agent = option.agent_type;
                let is_available = agent.is_available();

                let mut spans = vec![];

                // Selection indicator
                if is_selected {
                    spans.push(Span::styled("▶ ", Style::default().fg(selection_green)));
                } else {
                    spans.push(Span::raw("  "));
                }

                // Icon
                spans.push(Span::styled(
                    format!("{} ", agent.icon()),
                    Style::default().fg(if is_available { soft_white } else { coming_soon_gray }),
                ));

                // Name
                let name_style = if is_selected && is_available {
                    Style::default().fg(selection_green).add_modifier(Modifier::BOLD)
                } else if is_available {
                    Style::default().fg(soft_white)
                } else {
                    Style::default().fg(coming_soon_gray)
                };
                spans.push(Span::styled(agent.name(), name_style));

                // Coming Soon badge
                if !is_available {
                    spans.push(Span::styled(
                        " [Soon]",
                        Style::default().fg(coming_soon_gray).add_modifier(Modifier::ITALIC),
                    ));
                }

                // Current agent indicator
                if option.is_current && is_available {
                    spans.push(Span::styled(
                        " [Current]",
                        Style::default().fg(selection_green),
                    ));
                }

                // Description on a new line for selected items
                let base_style = if is_selected {
                    Style::default().bg(list_highlight_bg)
                } else {
                    Style::default()
                };

                let lines = if is_selected {
                    vec![
                        Line::from(spans),
                        Line::from(vec![
                            Span::raw("     "),
                            Span::styled(
                                agent.description(),
                                Style::default().fg(muted_gray).add_modifier(Modifier::ITALIC),
                            ),
                        ]),
                    ]
                } else {
                    vec![Line::from(spans)]
                };

                ListItem::new(lines).style(base_style)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(subdued_border))
                    .style(Style::default().bg(dark_bg)),
            )
            .highlight_style(Style::default().bg(list_highlight_bg));

        frame.render_widget(list, area);
    }

    /// Render a compact horizontal model selector bar
    fn render_model_selector(
        &self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
        colors: &[Color],
    ) {
        let [cornflower_blue, dark_bg, gold, soft_white, muted_gray, selection_green,
            _subdued_border, list_highlight_bg] = colors[..] else {
            return;
        };

        // Create the model selector block
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(Span::styled(" Model ", Style::default().fg(gold)))
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        let inner = block.inner(area);

        // Build horizontal model options
        let mut spans = vec![Span::raw(" ")];

        for (idx, model) in session_state.model_options.iter().enumerate() {
            let is_selected = idx == session_state.selected_model_index;

            if idx > 0 {
                spans.push(Span::styled("  │  ", Style::default().fg(muted_gray)));
            }

            // Selection bracket and icon
            if is_selected {
                spans.push(Span::styled("◀ ", Style::default().fg(selection_green)));
                spans.push(Span::styled(
                    format!("{} ", model.icon()),
                    Style::default().fg(soft_white),
                ));
                spans.push(Span::styled(
                    model.display_name(),
                    Style::default().fg(selection_green).add_modifier(Modifier::BOLD).bg(list_highlight_bg),
                ));
                spans.push(Span::styled(" ▶", Style::default().fg(selection_green)));
            } else {
                spans.push(Span::styled(
                    format!("{} ", model.icon()),
                    Style::default().fg(muted_gray),
                ));
                spans.push(Span::styled(
                    model.display_name(),
                    Style::default().fg(soft_white),
                ));
            }
        }

        let model_line = Paragraph::new(Line::from(spans))
            .alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(model_line, inner);
    }

    fn render_branch_input(&self, frame: &mut Frame, area: Rect, session_state: &NewSessionState) {
        // Modern color palette
        let cornflower_blue = Color::Rgb(100, 149, 237);
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let soft_white = Color::Rgb(220, 220, 230);
        let muted_gray = Color::Rgb(128, 128, 128);
        let selection_green = Color::Rgb(100, 200, 100);
        let subdued_border = Color::Rgb(70, 70, 90);
        let list_highlight_bg = Color::Rgb(40, 40, 60);

        // Draw outer border with modern styling
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(Span::styled(
                " 🌿 New Session ",
                Style::default()
                    .fg(gold)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        // Check if model selector should be shown
        let show_model = session_state.should_show_model_selection();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints(if show_model {
                vec![
                    Constraint::Length(6), // Repository info card
                    Constraint::Length(1), // Spacer
                    Constraint::Length(3), // Branch input
                    Constraint::Length(1), // Spacer
                    Constraint::Length(1), // Agent selector
                    Constraint::Length(1), // Model selector
                    Constraint::Length(1), // Spacer
                    Constraint::Length(2), // Instructions
                ]
            } else {
                vec![
                    Constraint::Length(6), // Repository info card
                    Constraint::Length(1), // Spacer
                    Constraint::Length(3), // Branch input
                    Constraint::Length(1), // Spacer
                    Constraint::Length(1), // Agent selector
                    Constraint::Length(1), // Spacer
                    Constraint::Length(2), // Instructions
                ]
            })
            .split(inner);

        // Repository info card with icon
        let (repo_name, repo_path) = if let Some(selected_idx) = session_state.selected_repo_index {
            if let Some((_, repo)) = session_state.filtered_repos.get(selected_idx) {
                let name = repo.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
                let path = repo.to_string_lossy().to_string();
                // Truncate long paths
                let display_path = if path.len() > 60 {
                    format!("...{}", &path[path.len()-57..])
                } else {
                    path
                };
                (name.to_string(), display_path)
            } else {
                ("Unknown".to_string(), "".to_string())
            }
        } else {
            ("None selected".to_string(), "".to_string())
        };

        // Build repo info lines with optional current branch
        let repo_lines = vec![
            Line::from(vec![
                Span::styled("  📁 ", Style::default()),
                Span::styled("Repository", Style::default().fg(Color::Rgb(150, 150, 150))),
            ]),
            Line::from(vec![
                Span::styled("     ", Style::default()),
                Span::styled(
                    &repo_name,
                    Style::default()
                        .fg(Color::Rgb(100, 200, 255)) // Light blue
                        .add_modifier(Modifier::BOLD),
                ),
                // Show current branch next to repo name
                if let Some(ref branch) = session_state.current_repo_branch {
                    Span::styled(
                        format!("  ({})", branch),
                        Style::default()
                            .fg(Color::Rgb(100, 200, 100)) // Green for branch
                            .add_modifier(Modifier::ITALIC),
                    )
                } else {
                    Span::raw("")
                },
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  📍 ", Style::default()),
                Span::styled("Path", Style::default().fg(Color::Rgb(150, 150, 150))),
            ]),
            Line::from(vec![
                Span::styled("     ", Style::default()),
                Span::styled(
                    repo_path,
                    Style::default().fg(Color::Rgb(180, 180, 180)).add_modifier(Modifier::ITALIC),
                ),
            ]),
        ];

        let repo_display = Paragraph::new(repo_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(subdued_border))
                    .style(Style::default().bg(Color::Rgb(30, 30, 40))),
            );
        frame.render_widget(repo_display, chunks[0]);

        // Branch input with icon and cursor
        let branch_text = if session_state.branch_name.is_empty() {
            Line::from(vec![
                Span::styled("  🔀 ", Style::default().fg(selection_green)),
                Span::styled(
                    "ainb/",
                    Style::default().fg(muted_gray).add_modifier(Modifier::ITALIC),
                ),
                Span::styled("█", Style::default().fg(selection_green)),
            ])
        } else {
            Line::from(vec![
                Span::styled("  🔀 ", Style::default().fg(selection_green)),
                Span::styled(
                    &session_state.branch_name,
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
                Span::styled("█", Style::default().fg(selection_green)),
            ])
        };

        let branch_input = Paragraph::new(branch_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(selection_green))
                    .title(Span::styled(
                        " Branch Name ",
                        Style::default().fg(selection_green),
                    ))
                    .style(Style::default().bg(Color::Rgb(35, 35, 45))),
            );
        frame.render_widget(branch_input, chunks[2]);

        // Agent selector bar
        let agent_area = chunks[4];
        self.render_inline_agent_selector(frame, agent_area, session_state, &[
            gold, soft_white, muted_gray, selection_green, list_highlight_bg,
        ]);

        // Model selector bar (only if Claude is selected)
        let instruction_area_idx = if show_model {
            self.render_inline_model_selector(frame, chunks[5], session_state, &[
                gold, soft_white, muted_gray, selection_green, list_highlight_bg,
            ]);
            7 // Instructions at index 7
        } else {
            6 // Instructions at index 6
        };

        // Styled instructions footer
        let mut instruction_spans = vec![
            Span::styled("  ⌨️  ", Style::default()),
            Span::styled("Type", Style::default().fg(selection_green)),
            Span::styled(" branch  ", Style::default().fg(muted_gray)),
            Span::styled("│", Style::default().fg(subdued_border)),
            Span::styled("  Tab ", Style::default().fg(gold)),
            Span::styled("Agent  ", Style::default().fg(muted_gray)),
        ];

        if show_model {
            instruction_spans.extend(vec![
                Span::styled("│", Style::default().fg(subdued_border)),
                Span::styled("  ←→ ", Style::default().fg(gold)),
                Span::styled("Model  ", Style::default().fg(muted_gray)),
            ]);
        }

        instruction_spans.extend(vec![
            Span::styled("│", Style::default().fg(subdued_border)),
            Span::styled("  ⏎ ", Style::default().fg(selection_green)),
            Span::styled("Create  ", Style::default().fg(muted_gray)),
            Span::styled("│", Style::default().fg(subdued_border)),
            Span::styled("  Esc ", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::styled("Cancel", Style::default().fg(muted_gray)),
        ]);

        let instructions = Line::from(instruction_spans);
        let instructions_widget = Paragraph::new(instructions)
            .alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(instructions_widget, chunks[instruction_area_idx]);
    }

    fn render_permissions_config(
        &self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
    ) {
        // Modern color palette
        let cornflower_blue = Color::Rgb(100, 149, 237);
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let soft_white = Color::Rgb(220, 220, 230);
        let muted_gray = Color::Rgb(120, 120, 140);
        let selection_green = Color::Rgb(100, 200, 100);
        let warning_orange = Color::Rgb(255, 165, 0);

        // Clear background
        let background = Block::default().style(Style::default().bg(dark_bg));
        frame.render_widget(background, area);

        // Main dialog with rounded border
        let title_line = Line::from(vec![
            Span::styled(" 🔐 ", Style::default().fg(gold)),
            Span::styled("Permission Settings", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" ", Style::default()),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(title_line)
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(2), // Subtitle
                Constraint::Length(6), // Description
                Constraint::Length(7), // Option cards
                Constraint::Length(2), // Footer
            ])
            .split(inner);

        // Subtitle
        let subtitle = Paragraph::new(Line::from(vec![
            Span::styled("Configure how agents handle command execution", Style::default().fg(muted_gray)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(subtitle, chunks[0]);

        // Description with info box
        let desc_lines = vec![
            Line::from(vec![
                Span::styled("  ℹ️  ", Style::default().fg(cornflower_blue)),
                Span::styled("About Permission Prompts", Style::default().fg(cornflower_blue).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  • ", Style::default().fg(muted_gray)),
                Span::styled("With prompts: ", Style::default().fg(soft_white)),
                Span::styled("Agents will ask before running commands", Style::default().fg(muted_gray)),
            ]),
            Line::from(vec![
                Span::styled("  • ", Style::default().fg(muted_gray)),
                Span::styled("Without prompts: ", Style::default().fg(soft_white)),
                Span::styled("Agents run commands immediately (faster)", Style::default().fg(muted_gray)),
            ]),
        ];

        let description = Paragraph::new(desc_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Rgb(60, 60, 80)))
                    .style(Style::default().bg(dark_bg)),
            );
        frame.render_widget(description, chunks[1]);

        // Options with visual selection
        let (option_icon, option_color, option_title, option_desc, option_flag) = if session_state.skip_permissions {
            (
                "🚀",
                warning_orange,
                "Skip Permission Prompts",
                "Agents will execute commands without asking",
                "--dangerously-skip-permissions",
            )
        } else {
            (
                "🛡️",
                selection_green,
                "Keep Permission Prompts",
                "Agents will ask before executing commands",
                "default",
            )
        };

        let option_lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled(format!("    {}  ", option_icon), Style::default().fg(option_color)),
                Span::styled(option_title, Style::default().fg(option_color).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("       ", Style::default()),
                Span::styled(option_desc, Style::default().fg(soft_white)),
            ]),
            Line::from(vec![
                Span::styled("       Flag: ", Style::default().fg(muted_gray)),
                Span::styled(option_flag, Style::default().fg(cornflower_blue).add_modifier(Modifier::ITALIC)),
            ]),
        ];

        let option_title_line = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled("Current Selection", Style::default().fg(option_color)),
            Span::styled(" ", Style::default()),
        ]);

        let options = Paragraph::new(option_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(option_color))
                    .title(option_title_line)
                    .style(Style::default().bg(dark_bg)),
            );
        frame.render_widget(options, chunks[2]);

        // Modern footer with keyboard hints
        let footer = Paragraph::new(Line::from(vec![
            Span::styled("Space", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Toggle", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 60, 80))),
            Span::styled("Enter", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Continue", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 60, 80))),
            Span::styled("Esc", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Cancel", Style::default().fg(muted_gray)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(footer, chunks[3]);
    }

    fn render_creating(&self, frame: &mut Frame, area: Rect) {
        // Modern color palette
        let cornflower_blue = Color::Rgb(100, 149, 237);
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let soft_white = Color::Rgb(220, 220, 230);
        let muted_gray = Color::Rgb(120, 120, 140);
        let progress_cyan = Color::Rgb(100, 200, 230);

        // Clear background
        let background = Block::default().style(Style::default().bg(dark_bg));
        frame.render_widget(background, area);

        // Main dialog with rounded border
        let title_line = Line::from(vec![
            Span::styled(" ⚙️  ", Style::default().fg(progress_cyan)),
            Span::styled("Creating Session", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" ", Style::default()),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(title_line)
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(2), // Subtitle
                Constraint::Min(0),    // Progress content
                Constraint::Length(2), // Footer
            ])
            .split(inner);

        // Subtitle
        let subtitle = Paragraph::new(Line::from(vec![
            Span::styled("Setting up your development environment...", Style::default().fg(muted_gray)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(subtitle, chunks[0]);

        // Progress content with animated-style spinner dots
        let progress_lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  🔄 ", Style::default().fg(progress_cyan)),
                Span::styled("Creating Git worktree", Style::default().fg(soft_white)),
                Span::styled(" ...", Style::default().fg(progress_cyan)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  🐳 ", Style::default().fg(cornflower_blue)),
                Span::styled("Initializing Docker container", Style::default().fg(soft_white)),
                Span::styled(" ...", Style::default().fg(cornflower_blue)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  📦 ", Style::default().fg(gold)),
                Span::styled("Mounting volumes and configuring environment", Style::default().fg(soft_white)),
            ]),
            Line::from(""),
            Line::from(""),
            Line::from(vec![
                Span::styled("       This may take a moment...", Style::default().fg(muted_gray).add_modifier(Modifier::ITALIC)),
            ]),
        ];

        let progress = Paragraph::new(progress_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Rgb(60, 60, 80)))
                    .style(Style::default().bg(dark_bg)),
            );
        frame.render_widget(progress, chunks[1]);

        // Modern footer
        let footer = Paragraph::new(Line::from(vec![
            Span::styled("⏳ ", Style::default().fg(progress_cyan)),
            Span::styled("Please wait", Style::default().fg(muted_gray)),
            Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 60, 80))),
            Span::styled("Esc", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" Cancel", Style::default().fg(muted_gray)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(footer, chunks[2]);
    }

    fn render_mode_selection(
        &self,
        frame: &mut Frame,
        area: Rect,
        session_state: &NewSessionState,
    ) {
        use crate::models::SessionMode;

        // Draw outer border with modern styling
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(100, 149, 237))) // Cornflower blue
            .title(Span::styled(
                " 🎯 Choose Session Mode ",
                Style::default()
                    .fg(Color::Rgb(255, 215, 0)) // Gold
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(Color::Rgb(25, 25, 35))); // Dark background
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(8), // Interactive mode card
                Constraint::Length(1), // Spacer
                Constraint::Length(8), // Boss mode card
                Constraint::Length(1), // Spacer
                Constraint::Length(2), // Instructions
            ])
            .split(inner);

        let boss_enabled = session_state.selected_agent == crate::models::SessionAgentType::Claude;
        let is_boss = boss_enabled && session_state.mode == SessionMode::Boss;
        let is_interactive = session_state.mode == SessionMode::Interactive || !boss_enabled;

        // Interactive mode card
        let interactive_border_color = if is_interactive {
            Color::Rgb(100, 200, 100) // Green when selected
        } else {
            Color::Rgb(70, 70, 90) // Gray when not
        };

        let interactive_bg = if is_interactive {
            Color::Rgb(35, 45, 35) // Slightly green tint
        } else {
            Color::Rgb(30, 30, 40)
        };

        let interactive_text = vec![
            Line::from(vec![
                Span::styled(
                    if is_interactive { "  ▶ " } else { "    " },
                    Style::default().fg(Color::Rgb(100, 200, 100)),
                ),
                Span::styled("🖥️  ", Style::default()),
                Span::styled(
                    "Interactive Mode",
                    Style::default()
                        .fg(if is_interactive { Color::Rgb(100, 200, 100) } else { Color::Rgb(200, 200, 200) })
                        .add_modifier(Modifier::BOLD),
                ),
                if is_interactive {
                    Span::styled("  ✓", Style::default().fg(Color::Rgb(100, 200, 100)))
                } else {
                    Span::raw("")
                },
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("      ", Style::default()),
                Span::styled("•", Style::default().fg(Color::Rgb(100, 149, 237))),
                Span::styled(" Traditional development with shell access", Style::default().fg(Color::Rgb(180, 180, 180))),
            ]),
            Line::from(vec![
                Span::styled("      ", Style::default()),
                Span::styled("•", Style::default().fg(Color::Rgb(100, 149, 237))),
                Span::styled(" Full Claude CLI features and MCP servers", Style::default().fg(Color::Rgb(180, 180, 180))),
            ]),
            Line::from(vec![
                Span::styled("      ", Style::default()),
                Span::styled("•", Style::default().fg(Color::Rgb(100, 149, 237))),
                Span::styled(" Attach to container for development", Style::default().fg(Color::Rgb(180, 180, 180))),
            ]),
        ];

        let interactive_para = Paragraph::new(interactive_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(interactive_border_color))
                    .style(Style::default().bg(interactive_bg)),
            );
        frame.render_widget(interactive_para, chunks[0]);

        // Boss mode card
        let boss_border_color = if !boss_enabled {
            Color::Rgb(60, 60, 80) // Disabled
        } else if is_boss {
            Color::Rgb(255, 165, 0) // Orange when selected
        } else {
            Color::Rgb(70, 70, 90) // Gray when not
        };

        let boss_bg = if !boss_enabled {
            Color::Rgb(30, 30, 40)
        } else if is_boss {
            Color::Rgb(45, 40, 30) // Slightly orange tint
        } else {
            Color::Rgb(30, 30, 40)
        };

        let boss_title_color = if !boss_enabled {
            Color::Rgb(140, 140, 160)
        } else if is_boss {
            Color::Rgb(255, 165, 0)
        } else {
            Color::Rgb(200, 200, 200)
        };

        let boss_text = vec![
            Line::from(vec![
                Span::styled(
                    if is_boss { "  ▶ " } else { "    " },
                    Style::default().fg(if boss_enabled { Color::Rgb(255, 165, 0) } else { Color::Rgb(80, 80, 100) }),
                ),
                Span::styled("🤖 ", Style::default()),
                Span::styled(
                    "Boss Mode",
                    Style::default()
                        .fg(boss_title_color)
                        .add_modifier(Modifier::BOLD),
                ),
                if is_boss {
                    Span::styled("  ✓", Style::default().fg(Color::Rgb(255, 165, 0)))
                } else {
                    Span::raw("")
                },
                Span::styled(
                    "  [ALPHA]",
                    Style::default()
                        .fg(Color::Rgb(255, 165, 0))
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("      ", Style::default()),
                Span::styled("ALPHA stage", Style::default().fg(Color::Rgb(255, 165, 0)).add_modifier(Modifier::BOLD)),
                Span::styled(" — works only for Claude", Style::default().fg(Color::Rgb(150, 150, 170)).add_modifier(Modifier::ITALIC)),
            ]),
            Line::from(vec![
                Span::styled("      ", Style::default()),
                Span::styled("•", Style::default().fg(if boss_enabled { Color::Rgb(255, 165, 0) } else { Color::Rgb(80, 80, 100) })),
                Span::styled(
                    " Non-interactive task execution",
                    Style::default().fg(if boss_enabled { Color::Rgb(180, 180, 180) } else { Color::Rgb(130, 130, 150) }),
                ),
            ]),
            Line::from(vec![
                Span::styled("      ", Style::default()),
                Span::styled("•", Style::default().fg(if boss_enabled { Color::Rgb(255, 165, 0) } else { Color::Rgb(80, 80, 100) })),
                Span::styled(
                    " Direct prompt execution with text output",
                    Style::default().fg(if boss_enabled { Color::Rgb(180, 180, 180) } else { Color::Rgb(130, 130, 150) }),
                ),
            ]),
            Line::from(vec![
                Span::styled("      ", Style::default()),
                Span::styled("•", Style::default().fg(if boss_enabled { Color::Rgb(255, 165, 0) } else { Color::Rgb(80, 80, 100) })),
                Span::styled(
                    " Results streamed to TUI logs",
                    Style::default().fg(if boss_enabled { Color::Rgb(180, 180, 180) } else { Color::Rgb(130, 130, 150) }),
                ),
            ]),
        ];

        let boss_para = Paragraph::new(boss_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(boss_border_color))
                    .style(Style::default().bg(boss_bg)),
            );
        frame.render_widget(boss_para, chunks[2]);

        // Styled instructions footer
        let instructions = Line::from(vec![
            Span::styled("  ↑↓ ", Style::default().fg(Color::Rgb(100, 200, 100))),
            Span::styled("Switch Mode  ", Style::default().fg(Color::Rgb(128, 128, 128))),
            Span::styled("│", Style::default().fg(Color::Rgb(70, 70, 90))),
            Span::styled("  ⏎ ", Style::default().fg(Color::Rgb(100, 200, 100))),
            Span::styled("Continue  ", Style::default().fg(Color::Rgb(128, 128, 128))),
            Span::styled("│", Style::default().fg(Color::Rgb(70, 70, 90))),
            Span::styled("  Esc ", Style::default().fg(Color::Rgb(255, 100, 100))),
            Span::styled("Cancel  ", Style::default().fg(Color::Rgb(128, 128, 128))),
        ]);

        let instructions_widget = Paragraph::new(instructions)
            .alignment(Alignment::Center)
            .style(Style::default().bg(Color::Rgb(25, 25, 35)));
        frame.render_widget(instructions_widget, chunks[4]);
    }

    fn render_prompt_input(&self, frame: &mut Frame, area: Rect, session_state: &NewSessionState) {
        // Modern color palette
        let cornflower_blue = Color::Rgb(100, 149, 237);
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let soft_white = Color::Rgb(220, 220, 230);
        let muted_gray = Color::Rgb(120, 120, 140);
        let file_finder_yellow = Color::Rgb(255, 200, 100);

        // Clear background
        let background = Block::default().style(Style::default().bg(dark_bg));
        frame.render_widget(background, area);

        // Main dialog with rounded border
        let title_line = Line::from(vec![
            Span::styled(" 💬 ", Style::default().fg(gold)),
            Span::styled("Task Prompt", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
            Span::styled(" ", Style::default()),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(cornflower_blue))
            .title(title_line)
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(dark_bg));
        frame.render_widget(block.clone(), area);

        // Inner area for content
        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(2), // Subtitle
                Constraint::Length(6), // Instructions
                Constraint::Min(0),    // Prompt input area
                Constraint::Length(2), // Controls
            ])
            .split(inner);

        // Subtitle
        let subtitle_text = if session_state.file_finder.is_active {
            "File finder active - search for files to reference"
        } else {
            "Enter the task or prompt for Claude to execute"
        };
        let subtitle = Paragraph::new(Line::from(vec![
            Span::styled(subtitle_text, Style::default().fg(muted_gray)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(subtitle, chunks[0]);

        // Instructions - update to mention @ symbol for file finder
        let instructions_lines = if session_state.file_finder.is_active {
            vec![
                Line::from(vec![
                    Span::styled("  🔍 ", Style::default().fg(file_finder_yellow)),
                    Span::styled("File Finder Active", Style::default().fg(file_finder_yellow).add_modifier(Modifier::BOLD)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  • ", Style::default().fg(muted_gray)),
                    Span::styled("↑/↓", Style::default().fg(gold)),
                    Span::styled(" to navigate files", Style::default().fg(soft_white)),
                ]),
                Line::from(vec![
                    Span::styled("  • ", Style::default().fg(muted_gray)),
                    Span::styled("Enter", Style::default().fg(gold)),
                    Span::styled(" to select file  •  ", Style::default().fg(soft_white)),
                    Span::styled("Esc", Style::default().fg(gold)),
                    Span::styled(" to cancel", Style::default().fg(soft_white)),
                ]),
            ]
        } else {
            vec![
                Line::from(vec![
                    Span::styled("  💡 ", Style::default().fg(cornflower_blue)),
                    Span::styled("Example prompts:", Style::default().fg(cornflower_blue)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  • ", Style::default().fg(muted_gray)),
                    Span::styled("\"Analyze this codebase and suggest improvements\"", Style::default().fg(soft_white).add_modifier(Modifier::ITALIC)),
                ]),
                Line::from(vec![
                    Span::styled("  • ", Style::default().fg(muted_gray)),
                    Span::styled("\"Review the file ", Style::default().fg(soft_white).add_modifier(Modifier::ITALIC)),
                    Span::styled("@src/main.rs", Style::default().fg(file_finder_yellow)),
                    Span::styled("\" (type @ for file finder)", Style::default().fg(muted_gray)),
                ]),
                Line::from(vec![
                    Span::styled("  • ", Style::default().fg(muted_gray)),
                    Span::styled("\"Fix issue #123\"", Style::default().fg(soft_white).add_modifier(Modifier::ITALIC)),
                ]),
            ]
        };

        let instructions_border = if session_state.file_finder.is_active {
            file_finder_yellow
        } else {
            Color::Rgb(60, 60, 80)
        };

        let instructions = Paragraph::new(instructions_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(instructions_border))
                    .style(Style::default().bg(dark_bg)),
            );
        frame.render_widget(instructions, chunks[1]);

        // Split the prompt input area if file finder is active
        if session_state.file_finder.is_active {
            let input_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(50), // Prompt
                    Constraint::Percentage(50), // File finder
                ])
                .split(chunks[2]);

            // Render prompt on the left
            self.render_text_editor(frame, input_chunks[0], &session_state.boss_prompt, "Prompt");

            // Render file finder on the right
            self.render_file_finder(frame, input_chunks[1], session_state);
        } else {
            // Normal full-width prompt input
            self.render_text_editor(frame, chunks[2], &session_state.boss_prompt, "Prompt");
        }

        // Modern footer with keyboard hints
        let controls = if session_state.file_finder.is_active {
            Paragraph::new(Line::from(vec![
                Span::styled("↑↓", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
                Span::styled(" Navigate", Style::default().fg(muted_gray)),
                Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 60, 80))),
                Span::styled("Enter", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
                Span::styled(" Select", Style::default().fg(muted_gray)),
                Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 60, 80))),
                Span::styled("Type", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
                Span::styled(" Filter", Style::default().fg(muted_gray)),
                Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 60, 80))),
                Span::styled("Esc", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
                Span::styled(" Cancel", Style::default().fg(muted_gray)),
            ]))
        } else {
            Paragraph::new(Line::from(vec![
                Span::styled("Type", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
                Span::styled(" Input", Style::default().fg(muted_gray)),
                Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 60, 80))),
                Span::styled("Ctrl+J", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
                Span::styled(" Newline", Style::default().fg(muted_gray)),
                Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 60, 80))),
                Span::styled("@", Style::default().fg(file_finder_yellow).add_modifier(Modifier::BOLD)),
                Span::styled(" Files", Style::default().fg(muted_gray)),
                Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 60, 80))),
                Span::styled("Enter", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
                Span::styled(" Continue", Style::default().fg(muted_gray)),
                Span::styled("  │  ", Style::default().fg(Color::Rgb(60, 60, 80))),
                Span::styled("Esc", Style::default().fg(gold).add_modifier(Modifier::BOLD)),
                Span::styled(" Cancel", Style::default().fg(muted_gray)),
            ]))
        };
        frame.render_widget(controls.alignment(Alignment::Center), chunks[3]);
    }

    fn render_file_finder(&self, frame: &mut Frame, area: Rect, session_state: &NewSessionState) {
        // Modern color palette
        let dark_bg = Color::Rgb(25, 25, 35);
        let gold = Color::Rgb(255, 215, 0);
        let soft_white = Color::Rgb(220, 220, 230);
        let muted_gray = Color::Rgb(120, 120, 140);
        let file_finder_yellow = Color::Rgb(255, 200, 100);
        let selection_bg = Color::Rgb(80, 70, 40);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Query input
                Constraint::Min(0),    // File list
            ])
            .split(area);

        // Query input with modern styling
        let query_display = format!("  @{}", session_state.file_finder.query);
        let query_title = Line::from(vec![
            Span::styled(" 🔍 ", Style::default().fg(file_finder_yellow)),
            Span::styled("Filter", Style::default().fg(file_finder_yellow)),
            Span::styled(" ", Style::default()),
        ]);

        let query_input = Paragraph::new(Line::from(vec![
            Span::styled(query_display, Style::default().fg(file_finder_yellow)),
            Span::styled("▋", Style::default().fg(gold)), // Cursor indicator
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(file_finder_yellow))
                .title(query_title)
                .style(Style::default().bg(dark_bg)),
        );
        frame.render_widget(query_input, chunks[0]);

        // File list with modern styling
        let file_items: Vec<ListItem> = session_state
            .file_finder
            .matches
            .iter()
            .enumerate()
            .map(|(idx, file_match)| {
                if idx == session_state.file_finder.selected_index {
                    ListItem::new(Line::from(vec![
                        Span::styled("  ▶ ", Style::default().fg(gold)),
                        Span::styled(&file_match.relative_path, Style::default().fg(gold).add_modifier(Modifier::BOLD)),
                    ]))
                    .style(Style::default().bg(selection_bg))
                } else {
                    ListItem::new(Line::from(vec![
                        Span::styled("    ", Style::default()),
                        Span::styled(&file_match.relative_path, Style::default().fg(soft_white)),
                    ]))
                }
            })
            .collect();

        let match_count = session_state.file_finder.matches.len();
        let list_title = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled("📄 ", Style::default().fg(muted_gray)),
            Span::styled(format!("{} matches", match_count), Style::default().fg(muted_gray)),
            Span::styled(" ", Style::default()),
        ]);

        let file_list = List::new(file_items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(60, 60, 80)))
                .title(list_title)
                .style(Style::default().bg(dark_bg)),
        );

        frame.render_widget(file_list, chunks[1]);
    }

    fn centered_rect(&self, percent_x: u16, percent_y: u16, r: Rect) -> Rect {
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ])
            .split(r);

        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ])
            .split(popup_layout[1])[1]
    }

    fn render_text_editor(
        &self,
        frame: &mut Frame,
        area: Rect,
        editor: &crate::app::state::TextEditor,
        title: &str,
    ) {
        use ratatui::layout::Alignment;
        use ratatui::style::{Color, Style};
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Paragraph};

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green))
            .title(title);

        let inner_area = block.inner(area);
        frame.render_widget(block, area);

        if editor.is_empty() {
            // Show placeholder text
            let placeholder = Paragraph::new("Type your prompt here...")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Left);
            frame.render_widget(placeholder, inner_area);
        } else {
            // Render text with cursor
            let (cursor_line, cursor_col) = editor.get_cursor_position();
            let lines = editor.get_lines();

            let rendered_lines: Vec<Line> = lines
                .iter()
                .enumerate()
                .map(|(line_idx, line_text)| {
                    if line_idx == cursor_line {
                        // This line contains the cursor
                        let mut spans = Vec::new();

                        if cursor_col == 0 {
                            // Cursor at beginning of line
                            spans.push(Span::styled(
                                "█",
                                Style::default().fg(Color::White).bg(Color::Green),
                            ));
                            if !line_text.is_empty() {
                                spans.push(Span::styled(
                                    line_text,
                                    Style::default().fg(Color::White),
                                ));
                            }
                        } else if cursor_col >= line_text.len() {
                            // Cursor at end of line
                            spans.push(Span::styled(line_text, Style::default().fg(Color::White)));
                            spans.push(Span::styled(
                                "█",
                                Style::default().fg(Color::White).bg(Color::Green),
                            ));
                        } else {
                            // Cursor in middle of line
                            let (before, rest) = line_text.split_at(cursor_col);
                            let (cursor_char, after) = if rest.len() > 1 {
                                rest.split_at(1)
                            } else {
                                (rest, "")
                            };

                            if !before.is_empty() {
                                spans.push(Span::styled(before, Style::default().fg(Color::White)));
                            }
                            spans.push(Span::styled(
                                cursor_char,
                                Style::default().fg(Color::White).bg(Color::Green),
                            ));
                            if !after.is_empty() {
                                spans.push(Span::styled(after, Style::default().fg(Color::White)));
                            }
                        }

                        Line::from(spans)
                    } else {
                        // Normal line without cursor
                        Line::from(Span::styled(line_text, Style::default().fg(Color::White)))
                    }
                })
                .collect();

            let paragraph = Paragraph::new(rendered_lines)
                .alignment(Alignment::Left)
                .wrap(ratatui::widgets::Wrap { trim: false }); // Don't trim to preserve exact formatting

            frame.render_widget(paragraph, inner_area);
        }
    }
}

impl Default for NewSessionComponent {
    fn default() -> Self {
        Self::new()
    }
}
