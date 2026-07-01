// ABOUTME: Main onboarding wizard component
// Renders step-based wizard UI following premium TUI style guide

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph},
};

use super::state::{DepInstall, OnboardingState, OnboardingStep};
use crate::setup::{DepReport, DepState, Tier, TopicReport};
use std::collections::HashMap;

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
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);

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
        let container = Block::default().style(Style::default().bg(DARK_BG));
        frame.render_widget(container, area);

        // Main layout: header, hint band, content, footer
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5), // Header with progress
                Constraint::Length(3), // Per-step hint ("what this does")
                Constraint::Min(12),   // Main content
                Constraint::Length(3), // Navigation footer
            ])
            .split(area);

        self.render_header(frame, layout[0], state);
        self.render_hint(frame, layout[1], state);
        self.render_step_content(frame, layout[2], state);
        self.render_navigation(frame, layout[3], state);
    }

    /// Render the per-step hint band — a one-liner explaining what the current
    /// step actually does (fed by `OnboardingStep::hint()`).
    fn render_hint(&self, frame: &mut Frame, area: Rect, state: &OnboardingState) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(SUBDUED_BORDER))
            .style(Style::default().bg(DARK_BG));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let hint = Paragraph::new(Line::from(vec![
            Span::styled("💡 ", Style::default().fg(GOLD)),
            Span::styled(state.current_step.hint(), Style::default().fg(MUTED_GRAY)),
        ]))
        .wrap(ratatui::widgets::Wrap { trim: true })
        .alignment(Alignment::Center);
        frame.render_widget(hint, inner);
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
            Span::styled(
                "AINB Setup Wizard",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
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
            OnboardingStep::OtelSetup => self.render_otel_setup(frame, area, state),
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
                Constraint::Length(6), // Mascot area
                Constraint::Length(3), // Welcome text
                Constraint::Min(5),    // Description
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
            Span::styled(
                "Agents in a Box",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("!", Style::default().fg(SOFT_WHITE)),
        ]))
        .alignment(Alignment::Center);
        frame.render_widget(welcome, content_layout[1]);

        // Description + clear calls to action.
        let blank = || Line::from("");
        let bullet = |text: &str| {
            Line::from(vec![
                Span::styled("• ", Style::default().fg(GOLD)),
                Span::styled(text.to_string(), Style::default().fg(SOFT_WHITE)),
            ])
        };

        let mut desc_lines: Vec<Line> = vec![
            blank(),
            Line::from(Span::styled(
                "Let's get you set up — just a few quick steps:",
                Style::default().fg(MUTED_GRAY),
            )),
            blank(),
            bullet("Check required dependencies"),
            bullet("Point AINB at your git projects"),
            bullet("Configure agent authentication"),
            bullet("Pick your preferred editor"),
            blank(),
            blank(),
        ];

        // Primary CTA — filled gold "button" for the main action.
        desc_lines.push(Line::from(vec![
            Span::styled(
                " Enter ",
                Style::default().fg(DARK_BG).bg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  or  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("→", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled("    Get started", Style::default().fg(SOFT_WHITE)),
        ]));
        // Secondary CTA — Esc backs out to the Setup menu.
        desc_lines.push(Line::from(vec![
            Span::styled("[", Style::default().fg(SUBDUED_BORDER)),
            Span::styled("Esc", Style::default().fg(GOLD)),
            Span::styled("]", Style::default().fg(SUBDUED_BORDER)),
            Span::styled("    Open the Setup menu", Style::default().fg(MUTED_GRAY)),
        ]));

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
                Line::from(Span::styled(
                    "🔄 Checking dependencies...",
                    Style::default().fg(GOLD),
                )),
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
                Line::from(Span::styled(
                    "Press Enter to check dependencies",
                    Style::default().fg(SOFT_WHITE),
                )),
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
                Constraint::Length(3), // Status summary + one-liner legend
                Constraint::Min(8),    // Dependency columns
                Constraint::Length(4), // Focused-dep detail band (docs link + install)
                Constraint::Length(2), // Instructions
            ])
            .split(inner);

        // Status summary
        let (status_icon, status_text, status_color) = if status.required_met() {
            if status.recommended_met() {
                ("✅", "All dependencies ready!", SELECTION_GREEN)
            } else {
                (
                    "⚠️",
                    "Core dependencies ready (some recommended missing)",
                    WARNING_YELLOW,
                )
            }
        } else {
            ("❌", "Missing required dependencies", ERROR_RED)
        };

        // Summary line + a one-liner explaining what this screen is, with the
        // icon legend so a first-time user knows why each row is here.
        let summary = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(status_icon, Style::default()),
                Span::styled(" ", Style::default()),
                Span::styled(status_text, Style::default().fg(status_color)),
                Span::styled(
                    format!("  ({}/{})", status.satisfied_count(), status.total_count()),
                    Style::default().fg(MUTED_GRAY),
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    "First-run setup — the tools ainb, your agents, plugins & memory need.   ",
                    Style::default().fg(MUTED_GRAY),
                ),
                Span::styled("[✓]", Style::default().fg(SELECTION_GREEN)),
                Span::styled(" ready   ", Style::default().fg(MUTED_GRAY)),
                Span::styled("[ ]", Style::default().fg(ERROR_RED)),
                Span::styled(" required   ", Style::default().fg(MUTED_GRAY)),
                Span::styled("[ ]", Style::default().fg(WARNING_YELLOW)),
                Span::styled(" recommended   ", Style::default().fg(MUTED_GRAY)),
                Span::styled("[ ]", Style::default().fg(MUTED_GRAY)),
                Span::styled(" optional / suggested", Style::default().fg(MUTED_GRAY)),
            ]),
            // Sample of what the Claude Code statusline looks like once wired —
            // so the value is visible right on the setup screen.
            Line::from(vec![
                Span::styled(
                    "Claude statusline preview:  ",
                    Style::default().fg(MUTED_GRAY),
                ),
                Span::styled(
                    " 5h 42% ",
                    Style::default().fg(DARK_BG).bg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " 7d 18% ",
                    Style::default().fg(DARK_BG).bg(WARNING_YELLOW).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " $2.10 ",
                    Style::default().fg(SOFT_WHITE).bg(SUBDUED_BORDER),
                ),
                Span::styled(
                    " ctx 61% ",
                    Style::default().fg(DARK_BG).bg(CORNFLOWER_BLUE).add_modifier(Modifier::BOLD),
                ),
            ]),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(summary, content_layout[0]);

        // Distribute topics across two columns, balanced by rendered height, so
        // the whole width is used instead of one tall narrow list. The focused
        // dep is highlighted; its full docs link + install action live in the
        // detail band below (full width, so long URLs never truncate).
        let focused_id = state.focused_dep().map(|d| d.id);
        let mut col_items: [Vec<ListItem>; 2] = [Vec::new(), Vec::new()];
        let mut col_lines = [0usize, 0usize];
        for topic in &status.topics {
            let target = if col_lines[0] <= col_lines[1] { 0 } else { 1 };
            let before = col_items[target].len();
            push_topic_items(
                &mut col_items[target],
                topic,
                focused_id,
                &state.install_states,
            );
            col_lines[target] += col_items[target].len() - before;
        }

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(content_layout[1]);
        for (i, area) in cols.iter().enumerate() {
            let list =
                List::new(std::mem::take(&mut col_items[i])).style(Style::default().bg(PANEL_BG));
            frame.render_widget(list, *area);
        }

        // Focused-dep detail band — the full-width home for the docs link and
        // install action (the deps grid is too narrow for either).
        let detail = focused_detail_lines(state.focused_dep(), &state.install_states);
        frame.render_widget(Paragraph::new(detail), content_layout[2]);

        // Footer priority: agent picker (after G) > last action result (error >
        // success) > key hints.
        let instr_span = if state.agent_pick_open {
            Span::styled(
                "Generate installer for:   [c] Claude    [x] Codex    [p] Copilot       Esc cancel",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            )
        } else if let Some(err) = &state.error_message {
            Span::styled(
                err.clone(),
                Style::default().fg(ERROR_RED).add_modifier(Modifier::BOLD),
            )
        } else if let Some(msg) = &state.status_message {
            Span::styled(
                msg.clone(),
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                "↑↓←→ focus • i install • Enter next • Esc back • r recheck • t tmux",
                Style::default().fg(MUTED_GRAY),
            )
        };

        let instr_widget = Paragraph::new(instr_span).alignment(Alignment::Center);
        frame.render_widget(instr_widget, content_layout[3]);
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
                Constraint::Length(2), // Description
                Constraint::Length(3), // Input field
                Constraint::Length(1), // Spacer
                Constraint::Min(5),    // Validation results
                Constraint::Length(2), // Instructions
            ])
            .split(inner);

        // Description
        let desc = Paragraph::new(Line::from(vec![
            Span::styled(
                "Enter paths to your git project directories ",
                Style::default().fg(SOFT_WHITE),
            ),
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

        let input = Paragraph::new(input_text).style(Style::default().fg(SOFT_WHITE)).block(
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

                let error_text =
                    validated.error.as_ref().map(|e| format!(" - {}", e)).unwrap_or_default();

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
        let instructions = format!("{} valid path(s) • Press Enter to continue", valid_count);

        let instr_widget =
            Paragraph::new(Span::styled(instructions, Style::default().fg(MUTED_GRAY)))
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

        use crate::components::onboarding::state::{AuthMethodKind, AuthPane};

        let content: Vec<Line> = match &state.auth_pane {
            // ── Inline API-key entry for the chosen agent ────────────────────
            AuthPane::KeyEntry { agent, buf } => {
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("Enter your {}", agent.key_label()),
                        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        format!(
                            "Stored in the system keychain; injected as {} when a session starts.",
                            agent.env_var()
                        ),
                        Style::default().fg(MUTED_GRAY),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("  {}_", buf),
                        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("Enter ", Style::default().fg(GOLD)),
                        Span::styled("save   ", Style::default().fg(MUTED_GRAY)),
                        Span::styled("Esc ", Style::default().fg(GOLD)),
                        Span::styled("back", Style::default().fg(MUTED_GRAY)),
                    ]),
                ]
            }
            // ── Method picker for the chosen agent ───────────────────────────
            AuthPane::MethodPicker { agent, cursor } => {
                let mut lines: Vec<Line> = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("{} — choose auth method", agent.label()),
                        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                ];
                let rows = [agent.login_label(), "API key", "Back"];
                for (i, label) in rows.iter().enumerate() {
                    let selected = i == *cursor;
                    let marker = if selected { "\u{25b6} " } else { "  " };
                    let style = if selected {
                        Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(GOLD)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(marker, Style::default().fg(SELECTION_GREEN)),
                        Span::styled(*label, style),
                    ]));
                }
                lines.push(Line::from(""));
                // What each method actually does + the vendor auth guide.
                lines.push(Line::from(Span::styled(
                    format!("{}: {}", agent.login_label(), agent.login_hint()),
                    Style::default().fg(MUTED_GRAY),
                )));
                lines.push(Line::from(Span::styled(
                    format!(
                        "API key: stored in your keychain, injected as {} when a session starts.",
                        agent.env_var()
                    ),
                    Style::default().fg(MUTED_GRAY),
                )));
                lines.push(Line::from(Span::styled(
                    format!("Guide: {}", agent.doc_url()),
                    Style::default().fg(CORNFLOWER_BLUE),
                )));
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("\u{2191}\u{2193} ", Style::default().fg(GOLD)),
                    Span::styled("select   ", Style::default().fg(MUTED_GRAY)),
                    Span::styled("Enter ", Style::default().fg(GOLD)),
                    Span::styled("choose   ", Style::default().fg(MUTED_GRAY)),
                    Span::styled("Esc ", Style::default().fg(GOLD)),
                    Span::styled("back", Style::default().fg(MUTED_GRAY)),
                ]));
                lines
            }
            // ── Per-agent list (default) ─────────────────────────────────────
            AuthPane::AgentList => {
                let mut lines: Vec<Line> = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "Configure agent authentication (change anytime)",
                        Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                ];
                for (i, st) in state.auth_statuses.iter().enumerate() {
                    let selected = i == state.auth_agent_cursor;
                    let marker = if selected { "\u{25b6} " } else { "  " };
                    let agent_style = if selected {
                        Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(GOLD)
                    };
                    // Login rows show the harness-specific label ("System-wide
                    // auth", "Sign in with GitHub", …); key rows just say "API key".
                    let method_label = match st.method {
                        AuthMethodKind::Login => st.agent.login_label(),
                        AuthMethodKind::ApiKey => "API key",
                    };
                    let mut spans = vec![
                        Span::styled(marker, Style::default().fg(SELECTION_GREEN)),
                        Span::styled(format!("{:<9}", st.agent.label()), agent_style),
                        Span::styled(method_label, Style::default().fg(SOFT_WHITE)),
                    ];
                    if let Some(ref masked) = st.key_masked {
                        spans.push(Span::styled(
                            format!("  {}", masked),
                            Style::default().fg(MUTED_GRAY),
                        ));
                    }
                    lines.push(Line::from(spans));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "System-wide auth = you set it up outside ainb. API key = ainb stores it \
                     in your keychain and injects it when a session starts.",
                    Style::default().fg(MUTED_GRAY),
                )));
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("\u{2191}\u{2193} ", Style::default().fg(GOLD)),
                    Span::styled("select   ", Style::default().fg(MUTED_GRAY)),
                    Span::styled("Enter ", Style::default().fg(GOLD)),
                    Span::styled("change   ", Style::default().fg(MUTED_GRAY)),
                    Span::styled("\u{2192} ", Style::default().fg(GOLD)),
                    Span::styled("next   ", Style::default().fg(MUTED_GRAY)),
                    Span::styled("s ", Style::default().fg(GOLD)),
                    Span::styled("skip", Style::default().fg(MUTED_GRAY)),
                ]));
                lines
            }
        };

        let text = Paragraph::new(content)
            .alignment(Alignment::Center)
            .wrap(ratatui::widgets::Wrap { trim: true });
        frame.render_widget(text, inner);
    }

    /// Render the OpenTelemetry (Grafana Cloud) setup step
    fn render_otel_setup(&self, frame: &mut Frame, area: Rect, state: &OnboardingState) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(CORNFLOWER_BLUE))
            .style(Style::default().bg(PANEL_BG))
            .title(" OpenTelemetry → Grafana Cloud ")
            .title_style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let content_layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(6), // What + how-to-get + docs link
                Constraint::Length(3), // endpoint field
                Constraint::Length(3), // instance id field
                Constraint::Length(3), // token field
                Constraint::Min(2),    // instructions
            ])
            .split(inner);

        // What this does + where to get the creds.
        let intro = Paragraph::new(vec![
            Line::from(Span::styled(
                "Optional: ship Claude Code metrics/logs/traces to Grafana Cloud",
                Style::default().fg(SOFT_WHITE),
            )),
            Line::from(Span::styled(
                "via a local Grafana Alloy collector (started for you).",
                Style::default().fg(MUTED_GRAY),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("Get creds: ", Style::default().fg(GOLD)),
                Span::styled(
                    "Grafana Cloud → Connections → \"OpenTelemetry (OTLP)\"",
                    Style::default().fg(MUTED_GRAY),
                ),
            ]),
            // Bare URL on its own line so it never truncates and terminals
            // auto-linkify it (Cmd/Ctrl-click) — it's also mouse-selectable.
            Line::from(Span::styled(
                "Docs & example dashboards:",
                Style::default().fg(GOLD),
            )),
            Line::from(Span::styled(
                crate::docs::OTEL,
                Style::default().fg(CORNFLOWER_BLUE),
            )),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(intro, content_layout[0]);

        // Field renderer with focus + token masking.
        let field =
            |frame: &mut Frame, area: Rect, idx: usize, label: &str, value: &str, mask: bool| {
                let focused = state.otel_field == idx && !state.otel_skip;
                let shown = if mask {
                    "•".repeat(value.chars().count())
                } else {
                    value.to_string()
                };
                let display = if focused && state.show_cursor {
                    format!("{shown}│")
                } else {
                    shown
                };
                let border = if focused { GOLD } else { SUBDUED_BORDER };
                let para = Paragraph::new(display).style(Style::default().fg(SOFT_WHITE)).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(border))
                        .title(format!(" {label} "))
                        .title_style(Style::default().fg(if focused { GOLD } else { MUTED_GRAY }))
                        .style(Style::default().bg(DARK_BG)),
                );
                frame.render_widget(para, area);
            };

        field(
            frame,
            content_layout[1],
            0,
            "OTLP endpoint (…/otlp)",
            &state.otel_otlp_endpoint,
            false,
        );
        field(
            frame,
            content_layout[2],
            1,
            "Instance ID",
            &state.otel_instance_id,
            false,
        );
        field(
            frame,
            content_layout[3],
            2,
            "API token",
            &state.otel_api_token,
            true,
        );

        // Instructions / status line.
        let status = if state.otel_skip {
            Line::from(vec![
                Span::styled("Optional. ", Style::default().fg(WARNING_YELLOW)),
                Span::styled("Type to configure · ", Style::default().fg(MUTED_GRAY)),
                Span::styled("Tab", Style::default().fg(GOLD)),
                Span::styled(" next field · ", Style::default().fg(MUTED_GRAY)),
                Span::styled("Enter", Style::default().fg(GOLD)),
                Span::styled(" skip", Style::default().fg(MUTED_GRAY)),
            ])
        } else if state.otel_creds_complete() {
            Line::from(vec![
                Span::styled("✓ ready ", Style::default().fg(SELECTION_GREEN)),
                Span::styled("· Tab next field · ", Style::default().fg(MUTED_GRAY)),
                Span::styled("Enter", Style::default().fg(GOLD)),
                Span::styled(" set up & continue", Style::default().fg(MUTED_GRAY)),
            ])
        } else {
            Line::from(vec![
                Span::styled("Tab", Style::default().fg(GOLD)),
                Span::styled(
                    " next field · fill all 3 to enable · ",
                    Style::default().fg(MUTED_GRAY),
                ),
                Span::styled("Enter", Style::default().fg(GOLD)),
                Span::styled(" skips", Style::default().fg(MUTED_GRAY)),
            ])
        };
        let instr = Paragraph::new(status).alignment(Alignment::Center);
        frame.render_widget(instr, content_layout[4]);
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
                Constraint::Length(3), // Description
                Constraint::Min(10),   // Editor list
                Constraint::Length(2), // Instructions
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
                Line::from(Span::styled(
                    "No editors detected",
                    Style::default().fg(MUTED_GRAY),
                )),
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

                items.push(
                    ListItem::new(Line::from(vec![
                        Span::styled("  ", Style::default()),
                        Span::styled(icon, Style::default().fg(icon_color)),
                        Span::styled(" ", Style::default()),
                        Span::styled(&editor.name, name_style),
                        Span::styled(
                            format!(" ({})", editor.command),
                            Style::default().fg(MUTED_GRAY),
                        ),
                        availability,
                    ]))
                    .style(bg_style),
                );
            }

            let list = List::new(items).style(Style::default().bg(PANEL_BG));
            frame.render_widget(list, content_layout[1]);
        }

        // Instructions
        let selected_editor = state.get_selected_editor();
        let instructions = if selected_editor.is_some() {
            format!(
                "Selected: {} • Press Enter to continue, or skip to use defaults",
                state
                    .available_editors
                    .get(state.selected_editor_index)
                    .map(|e| e.name.as_str())
                    .unwrap_or("None")
            )
        } else {
            "No available editor selected • Press Enter to use fallback (code → $EDITOR)"
                .to_string()
        };

        let instr_widget =
            Paragraph::new(Span::styled(instructions, Style::default().fg(MUTED_GRAY)))
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
                Constraint::Length(4), // Success message
                Constraint::Min(8),    // Summary items
                Constraint::Length(3), // Finish button
            ])
            .split(inner);

        // Success message
        let success = vec![
            Line::from(Span::styled("🎉", Style::default())),
            Line::from(Span::styled(
                "You're all set!",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            )),
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
                    format!(
                        "{}/{} installed",
                        status.satisfied_count(),
                        status.total_count()
                    ),
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

        // Auth — per-agent summary (e.g. "Claude login • Codex api key")
        let auth_status = state
            .auth_method
            .clone()
            .unwrap_or_else(|| "not configured".to_string());
        summary_items.push(Line::from(vec![
            Span::styled(
                if state.auth_completed {
                    "  ✓ "
                } else {
                    "  ○ "
                },
                Style::default().fg(if state.auth_completed {
                    SELECTION_GREEN
                } else {
                    WARNING_YELLOW
                }),
            ),
            Span::styled("Authentication: ", Style::default().fg(SOFT_WHITE)),
            Span::styled(auth_status, Style::default().fg(MUTED_GRAY)),
        ]));

        // Telemetry (OTEL -> Grafana Cloud)
        let otel_on = state.otel_should_setup();
        summary_items.push(Line::from(vec![
            Span::styled(
                if otel_on { "  ✓ " } else { "  ○ " },
                Style::default().fg(if otel_on {
                    SELECTION_GREEN
                } else {
                    WARNING_YELLOW
                }),
            ),
            Span::styled("Telemetry: ", Style::default().fg(SOFT_WHITE)),
            Span::styled(
                if otel_on {
                    "Grafana Cloud (Alloy)".to_string()
                } else {
                    "skipped (run `ainb otel setup` later)".to_string()
                },
                Style::default().fg(MUTED_GRAY),
            ),
        ]));

        // Editor
        let editor_status = state
            .get_selected_editor()
            .map(|cmd| {
                state
                    .available_editors
                    .iter()
                    .find(|e| e.command == cmd)
                    .map(|e| format!("{} ({})", e.name, e.command))
                    .unwrap_or(cmd)
            })
            .unwrap_or_else(|| "fallback (code → $EDITOR)".to_string());
        summary_items.push(Line::from(vec![
            Span::styled(
                if state.get_selected_editor().is_some() {
                    "  ✓ "
                } else {
                    "  ○ "
                },
                Style::default().fg(if state.get_selected_editor().is_some() {
                    SELECTION_GREEN
                } else {
                    WARNING_YELLOW
                }),
            ),
            Span::styled("Editor: ", Style::default().fg(SOFT_WHITE)),
            Span::styled(editor_status, Style::default().fg(MUTED_GRAY)),
        ]));

        let summary_widget = Paragraph::new(summary_items);
        frame.render_widget(summary_widget, content_layout[1]);

        // Finish button
        let finish = Paragraph::new(Line::from(vec![
            Span::styled("Press ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "Enter",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " to finish and start using AINB",
                Style::default().fg(MUTED_GRAY),
            ),
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

        // On the dependency step every arrow is navigation (move the focused-dep
        // cursor), so Back is Esc there — not ↑/←. Other steps keep ↑/← Back.
        let deps_step = state.current_step == OnboardingStep::DependencyCheck;

        // Back button (↑ works in all steps, ← works in most but not text input)
        if state.can_go_back() {
            spans.push(Span::styled("[", Style::default().fg(SUBDUED_BORDER)));
            spans.push(Span::styled(
                if deps_step { "Esc" } else { "↑/←" },
                Style::default().fg(GOLD),
            ));
            spans.push(Span::styled("]", Style::default().fg(SUBDUED_BORDER)));
            spans.push(Span::styled(" Back", Style::default().fg(MUTED_GRAY)));
            spans.push(Span::styled("  |  ", Style::default().fg(SUBDUED_BORDER)));
        }

        // Next/Finish button
        let can_advance = state.current_step.can_advance(state);
        let button_text = if state.is_final_step() {
            "Finish"
        } else {
            "Next"
        };

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

        // Third hint: deps step shows arrows = navigate (Esc already shown as
        // Back above); other steps show Esc = Menu.
        spans.push(Span::styled("  |  ", Style::default().fg(SUBDUED_BORDER)));
        if deps_step {
            spans.push(Span::styled("[", Style::default().fg(SUBDUED_BORDER)));
            spans.push(Span::styled("↑↓←→", Style::default().fg(GOLD)));
            spans.push(Span::styled("]", Style::default().fg(SUBDUED_BORDER)));
            spans.push(Span::styled(" navigate", Style::default().fg(MUTED_GRAY)));
        } else {
            spans.push(Span::styled("[", Style::default().fg(SUBDUED_BORDER)));
            spans.push(Span::styled("Esc", Style::default().fg(GOLD)));
            spans.push(Span::styled("]", Style::default().fg(SUBDUED_BORDER)));
            spans.push(Span::styled(" Menu", Style::default().fg(MUTED_GRAY)));
        }

        let nav = Paragraph::new(Line::from(spans)).alignment(Alignment::Center);
        frame.render_widget(nav, inner);
    }
}

impl Default for OnboardingComponent {
    fn default() -> Self {
        Self::new()
    }
}

/// Render one topic (header + its deps) into a column's item list. Each dep
/// shows its icon, name, detected detail, tier tag and a dimmed one-liner "why",
/// plus an indented install hint when it's missing.
fn push_topic_items(
    items: &mut Vec<ListItem<'static>>,
    topic: &TopicReport,
    focused_id: Option<&str>,
    install_states: &HashMap<String, DepInstall>,
) {
    items.push(ListItem::new(Line::from(vec![
        Span::styled("─── ", Style::default().fg(SUBDUED_BORDER)),
        Span::styled(
            topic.label,
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ───", Style::default().fg(SUBDUED_BORDER)),
    ])));
    for d in &topic.deps {
        let (checkbox, box_color) = dep_checkbox(d);
        let tier_tag = if d.satisfied {
            String::new()
        } else {
            format!(" [{}]", d.tier.label())
        };
        let is_focused = focused_id == Some(d.id);
        // A live install marker so a row reflects its background install even
        // when it isn't the focused row (the detail band carries the detail).
        let (marker, marker_color) = match install_states.get(d.id) {
            Some(DepInstall::Installing) => ("⟳ ", WARNING_YELLOW),
            Some(DepInstall::Done) => ("✓ ", SELECTION_GREEN),
            Some(DepInstall::Error(_)) => ("✗ ", ERROR_RED),
            None => ("", MUTED_GRAY),
        };
        // The install command no longer rides each row (it truncated in the
        // narrow column) — it lives full-width in the focused-dep detail band.
        let line = Line::from(vec![
            Span::styled(
                if is_focused { "▶ " } else { "  " },
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                checkbox,
                Style::default().fg(box_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default()),
            Span::styled(marker, Style::default().fg(marker_color)),
            Span::styled(
                d.name,
                if d.satisfied {
                    Style::default().fg(SOFT_WHITE)
                } else {
                    Style::default().fg(MUTED_GRAY)
                },
            ),
            Span::styled(dep_state_detail(&d.state), Style::default().fg(MUTED_GRAY)),
            Span::styled(tier_tag, Style::default().fg(MUTED_GRAY)),
            Span::styled(format!("  {}", d.why), Style::default().fg(SUBDUED_BORDER)),
        ]);
        let item = ListItem::new(line);
        items.push(if is_focused {
            item.style(Style::default().bg(LIST_HIGHLIGHT_BG))
        } else {
            item
        });
    }
    // Blank spacer line between sections for visual separation.
    items.push(ListItem::new(Line::from("")));
}

/// The full-width detail band under the dep columns: the focused dep's docs
/// link (full bare URL — auto-linkified, copyable, untruncated) and its install
/// action / status. The grid is too narrow for either, so they live here.
fn focused_detail_lines(
    dep: Option<&DepReport>,
    install_states: &HashMap<String, DepInstall>,
) -> Vec<Line<'static>> {
    let Some(d) = dep else {
        return vec![Line::from(Span::styled(
            "↑/↓ to focus a dependency",
            Style::default().fg(MUTED_GRAY),
        ))];
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "▶ ",
            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            d.name,
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", d.why), Style::default().fg(MUTED_GRAY)),
    ])];
    // Bare docs URL on its own line so it never truncates and terminals
    // auto-linkify it (Cmd/Ctrl-click) — also mouse-selectable.
    if let Some(url) = crate::docs::docs_url_for(d.id) {
        lines.push(Line::from(Span::styled(
            url,
            Style::default().fg(CORNFLOWER_BLUE),
        )));
    }
    // Install action / status.
    match install_states.get(d.id) {
        Some(DepInstall::Installing) => lines.push(Line::from(Span::styled(
            format!("⟳ installing {}…", d.name),
            Style::default().fg(WARNING_YELLOW),
        ))),
        Some(DepInstall::Done) => lines.push(Line::from(Span::styled(
            "✓ installed — press r to re-check",
            Style::default().fg(SELECTION_GREEN),
        ))),
        Some(DepInstall::Error(e)) => {
            lines.push(Line::from(Span::styled(
                format!("✗ {e}"),
                Style::default().fg(ERROR_RED),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "try manually: {}",
                    d.install_hint.lines().next().unwrap_or("")
                ),
                Style::default().fg(MUTED_GRAY),
            )));
        }
        None if !d.satisfied => lines.push(Line::from(vec![
            Span::styled("press i to install: ", Style::default().fg(GOLD)),
            Span::styled(
                d.install_hint.lines().next().unwrap_or("").to_string(),
                Style::default().fg(CORNFLOWER_BLUE),
            ),
        ])),
        None => {}
    }
    lines
}

/// Checkbox + color for a dependency report: `[✓]` (green) when satisfied,
/// otherwise an empty `[ ]` coloured by tier urgency (required=red,
/// recommended=yellow, optional/suggested=gray).
fn dep_checkbox(d: &DepReport) -> (&'static str, Color) {
    if d.satisfied {
        return ("[✓]", SELECTION_GREEN);
    }
    match d.tier {
        Tier::Required => ("[ ]", ERROR_RED),
        Tier::Recommended => ("[ ]", WARNING_YELLOW),
        Tier::Optional | Tier::Suggested => ("[ ]", MUTED_GRAY),
    }
}

/// Short trailing detail for a dependency's detected state.
fn dep_state_detail(state: &DepState) -> String {
    match state {
        DepState::Ok(Some(d)) => format!(" ({})", d.chars().take(24).collect::<String>()),
        DepState::Ok(None) => String::new(),
        DepState::Alt(d) => format!(" (via {d})"),
        DepState::TooOld(d) => format!(" — {d}"),
        DepState::Missing => String::new(),
        DepState::Unknown => " (?)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::onboarding::state::{OnboardingState, OnboardingStep};
    use ratatui::{Terminal, backend::TestBackend};

    fn otel_step_text(width: u16) -> String {
        let comp = OnboardingComponent::new();
        let mut state = OnboardingState::new();
        state.current_step = OnboardingStep::OtelSetup;
        // Height accounts for the header + per-step hint band + footer chrome.
        let mut terminal = Terminal::new(TestBackend::new(width, 34)).unwrap();
        terminal.draw(|f| comp.render(f, f.size(), &state)).unwrap();
        terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    /// The OTEL onboarding step must show the docsite URL in full — the bare
    /// URL line is what terminals auto-linkify and the user copies, so any
    /// truncation breaks the link. Assert the whole URL survives at the 80-col
    /// minimum and a wide terminal.
    #[test]
    fn otel_step_shows_full_docs_url_without_truncation() {
        for w in [80u16, 140] {
            let text = otel_step_text(w);
            assert!(
                text.contains(crate::docs::OTEL),
                "otel docs URL truncated/missing at {w} cols"
            );
        }
    }

    use crate::components::onboarding::state::DepInstall;
    use crate::setup::{DepReport, DepState, SetupStatus, Tier, TopicReport};

    fn witr_report(satisfied: bool) -> DepReport {
        DepReport {
            id: "witr",
            name: "witr",
            why: "process causality tracing",
            tier: Tier::Optional,
            consumers: vec![],
            install_hint: "brew install witr".to_string(),
            auto_installable: true,
            state: if satisfied {
                DepState::Ok(None)
            } else {
                DepState::Missing
            },
            satisfied,
        }
    }

    fn deps_state_focused_on_witr() -> OnboardingState {
        let mut state = OnboardingState::new();
        state.current_step = OnboardingStep::DependencyCheck;
        state.dependency_status = Some(SetupStatus {
            topics: vec![TopicReport {
                id: "plugin-bins",
                label: "Plugin binaries",
                description: "",
                deps: vec![witr_report(false)],
            }],
        });
        state.dep_cursor = 0; // focuses witr
        state
    }

    fn deps_step_text(state: &OnboardingState, width: u16) -> String {
        let comp = OnboardingComponent::new();
        // Height accounts for the header + per-step hint band + footer chrome.
        let mut terminal = Terminal::new(TestBackend::new(width, 34)).unwrap();
        terminal.draw(|f| comp.render(f, f.size(), state)).unwrap();
        terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    /// Focusing a dep shows its full docsite URL + install hint in the detail
    /// band, untruncated at the 80-col minimum and wide. This is the link the
    /// user couldn't find before.
    #[test]
    fn focused_dep_detail_band_shows_full_docs_link() {
        let state = deps_state_focused_on_witr();
        for w in [80u16, 140] {
            let text = deps_step_text(&state, w);
            assert!(
                text.contains(crate::docs::WITR),
                "focused dep docs URL truncated/missing at {w} cols"
            );
            assert!(
                text.contains("press i to install"),
                "missing install affordance at {w} cols"
            );
        }
    }

    /// An install error shows inline with a 'try manually' suggestion.
    #[test]
    fn focused_dep_install_error_shows_inline_with_manual_hint() {
        let mut state = deps_state_focused_on_witr();
        state.install_states.insert(
            "witr".to_string(),
            DepInstall::Error("brew not found".to_string()),
        );
        let text = deps_step_text(&state, 100);
        assert!(text.contains("brew not found"), "error message missing");
        assert!(text.contains("try manually"), "manual-retry hint missing");
    }

    /// The deps screen shows a sample of the Claude Code statusline so its value
    /// is visible right on the setup screen.
    #[test]
    fn deps_screen_shows_statusline_preview() {
        let state = deps_state_focused_on_witr();
        let text = deps_step_text(&state, 120);
        assert!(
            text.contains("Claude statusline preview"),
            "deps screen missing the statusline preview:\n{text}"
        );
    }

    /// The dep cursor never escapes the dep list bounds.
    #[test]
    fn dep_cursor_clamps_to_bounds() {
        let mut state = deps_state_focused_on_witr(); // 1 dep
        state.move_dep_cursor(5);
        assert_eq!(state.dep_cursor, 0, "cursor past end should clamp");
        state.move_dep_cursor(-5);
        assert_eq!(state.dep_cursor, 0, "cursor before start should clamp");
        // No deps at all → cursor pinned at 0.
        let mut empty = OnboardingState::new();
        empty.move_dep_cursor(3);
        assert_eq!(empty.dep_cursor, 0);
    }
}
