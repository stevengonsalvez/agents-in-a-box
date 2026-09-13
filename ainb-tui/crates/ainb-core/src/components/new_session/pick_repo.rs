// ABOUTME: Screen 1 of the new-session redesign — the unified repo picker.
// Renders favorites + recents + local scans in a single fuzzy-filtered list,
// smart-parses raw input on Enter, and persists screen-1 last-selection via
// `SessionDefaults`. Phase 4 of `plans/new-session-redesign-spec.md`.
//
// The screen is host-owned (no plugin involvement). Its key handler returns a
// `PickRepoOutcome` that the central event dispatcher translates into the
// appropriate next action (advance to `Configure`, kick off a clone, return
// to home, etc.). See `app/events.rs::handle_new_session_keys` for the wire.

pub use ainb_app::components::new_session::pick_repo::*;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::config::favorites_store::{Favorite, SourceType};
use crate::config::session_defaults::SessionDefaults;
use crate::git::repo_source::{RealFs, RepoSource, parse_with};

// Palette — matches `components/layout.rs` style guide. Kept module-local so
// the picker doesn't reach into layout internals.
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);

/// A dimmed locator shown after the row name so identically-named repos are
/// distinguishable. Derived from the row's `RepoSource`: a `~`-abbreviated path
/// for local repos, the URL for remotes, `owner/repo` for shorthand. `Filter`
/// rows carry no real source, so they get nothing.
fn row_detail(source: &RepoSource) -> Option<Cow<'_, str>> {
    match source {
        RepoSource::LocalPath(p) => Some(Cow::Owned(abbreviate_home(p))),
        RepoSource::HttpsUrl(u) | RepoSource::SshUrl(u) | RepoSource::SshSession(u) => {
            Some(Cow::Borrowed(u))
        }
        RepoSource::GithubShorthand { owner, repo } => Some(Cow::Owned(format!("{owner}/{repo}"))),
        RepoSource::Filter(_) => None,
    }
}

/// Collapse the home-directory prefix to `~` for display. Returns `~` for the
/// home dir itself and the unchanged full path when it lies outside home or
/// when `dirs::home_dir()` is unavailable. The home lookup is cached for the
/// process so it stays off the per-frame render path.
fn abbreviate_home(path: &Path) -> String {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    let home = HOME.get_or_init(dirs::home_dir);
    if let Some(home) = home {
        if let Ok(rest) = path.strip_prefix(home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            // `Path::join` so the platform's native separator is used.
            return Path::new("~").join(rest).display().to_string();
        }
    }
    path.display().to_string()
}

/// Render the picker into `area`. Layout: title bar → filter prompt → list →
/// help bar at the bottom. `BorderType::Rounded` everywhere.
#[allow(clippy::too_many_lines)]
pub fn render(f: &mut Frame, state: &PickRepoState, area: Rect) {
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .title(Span::styled(
            " New Session ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center)
        .style(Style::default().bg(DARK_BG));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // filter prompt
            Constraint::Min(3),    // list
            Constraint::Length(2), // help bar
        ])
        .split(inner);

    // Filter prompt — smart-parse accepts: free text (filter), owner/repo
    // (GitHub clone), https://… or git@host:… (clone), ssh://user@host
    // (SSH session), or a local path. Empty filter shows a greyed-out
    // hint so users discover the typed-input affordance (Stevie 2026-05-22
    // reported he didn't realise typing was supported).
    let prompt_line = if state.filter.is_empty() {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(
                "type to filter, or paste owner/repo · https://… · ssh://user@host · /local/path",
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(state.filter.clone(), Style::default().fg(SOFT_WHITE)),
        ])
    };
    let prompt = Paragraph::new(prompt_line).alignment(Alignment::Left);
    f.render_widget(prompt, chunks[0]);

    // List
    let items: Vec<ListItem> = state
        .filtered_indices
        .iter()
        .enumerate()
        .filter_map(|(visible_idx, &row_idx)| {
            let row = state.rows.get(row_idx)?;
            let is_selected = visible_idx == state.selected;
            let arrow = if is_selected {
                Span::styled(
                    "\u{25b8} ",
                    Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("  ")
            };
            let marker = Span::styled(
                format!("{} ", row.kind.marker()),
                Style::default().fg(match row.kind {
                    RowKind::Favorite => GOLD,
                    RowKind::Recent => MUTED_GRAY,
                    RowKind::Local => CORNFLOWER_BLUE,
                }),
            );
            let label_style = if is_selected {
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            };
            let mut spans = vec![arrow, marker, Span::styled(row.label.clone(), label_style)];
            // Dimmed locator after the name so identically-named repos are
            // distinguishable (e.g. two `Rosetta` rows in different folders).
            if let Some(detail) = row_detail(&row.source) {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(detail, Style::default().fg(MUTED_GRAY)));
            }

            // Inline status shown directly under the highlighted row
            // by appending extra lines. Works inside ListItem::new(vec![…]).
            let mut lines = vec![Line::from(spans)];
            // Auth status (Checking / NotAuthenticated) renders as a standalone
            // modal after the list (see `render_git_auth_modal`) so it stays
            // visible even when the filter matches no rows — a typed
            // `owner/repo` clone has no local row to attach an inline message
            // to. Only clone progress stays inline on the selected row.
            if is_selected {
                if let Some(progress) = &state.clone_progress {
                    if let Some(err) = &progress.error {
                        lines.push(Line::from(vec![
                            Span::raw("    "),
                            Span::styled(
                                format!("\u{2715} {err}"),
                                Style::default().fg(Color::Rgb(230, 100, 100)),
                            ),
                        ]));
                    } else {
                        let pct = progress
                            .bytes_done
                            .checked_mul(100)
                            .and_then(|n| n.checked_div(progress.bytes_total))
                            .map_or(0, |p| p.min(100));
                        lines.push(Line::from(vec![
                            Span::raw("    "),
                            Span::styled(
                                format!("\u{2299} cloning {} ({pct}%)", progress.url),
                                Style::default().fg(MUTED_GRAY),
                            ),
                        ]));
                    }
                }
            }
            Some(ListItem::new(lines))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE).style(Style::default().bg(DARK_BG)))
        .highlight_style(Style::default().bg(LIST_HIGHLIGHT_BG));

    let mut list_state = ListState::default();
    list_state.select(if state.filtered_indices.is_empty() {
        None
    } else {
        Some(state.selected)
    });
    f.render_stateful_widget(list, chunks[1], &mut list_state);

    // Auth pre-check feedback as a centered overlay on the list area. Drawn
    // here (not inline under a row) so a typed `owner/repo` with no local
    // match — empty list, no selection — still shows the spinner / the exact
    // failure + CTA instead of silently hanging.
    render_git_auth_modal(f, state, chunks[1]);

    // Help bar — gold keys + muted descriptions, single line.
    let help = Line::from(vec![
        Span::styled(
            "Enter",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled("=Select  ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            "Esc",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled("=Quit  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("^R", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        Span::styled("=Reset  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("^F", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        Span::styled("=Favorite", Style::default().fg(MUTED_GRAY)),
    ]);
    let help_p = Paragraph::new(help).alignment(Alignment::Center);
    f.render_widget(help_p, chunks[2]);
}

/// Centered overlay reporting GitHub auth pre-check state. Renders for
/// `Checking` (spinner) and `NotAuthenticated` (the EXACT `gh auth status`
/// output + the fix command + CTAs); `Authenticated` is transient and draws
/// nothing. Drawn over the list area so it is visible even when the filter
/// matches no rows — the case that previously hung silently. The overlay is
/// persistent: it clears only on an explicit key (Enter/s/Esc), never on a
/// timer.
fn render_git_auth_modal(f: &mut Frame, state: &PickRepoState, area: Rect) {
    let Some(status) = &state.git_auth_status else {
        return;
    };
    let amber = Color::Rgb(255, 191, 0);
    let red = Color::Rgb(230, 100, 100);

    let (title, border) = match status {
        GitAuthStatus::Checking => (" Checking GitHub auth ", Color::Rgb(100, 200, 230)),
        GitAuthStatus::NotAuthenticated => (" GitHub auth required ", amber),
        // Transient — auto-advances to Configure, no overlay needed.
        GitAuthStatus::Authenticated => return,
    };

    // Body holds everything above the CTA. The CTA is rendered into a pinned
    // 1-row footer below, so Dismiss/Retry stays visible no matter how long
    // the error is — a chatty `gh` must never push the CTA off-screen (the
    // very symptom this modal exists to fix).
    const MAX_ERR_LINES: usize = 8;
    let mut body: Vec<Line> = Vec::new();
    match status {
        GitAuthStatus::Checking => {
            body.push(Line::from(Span::styled(
                "\u{1f504} Running `gh auth status`\u{2026}",
                Style::default().fg(Color::Rgb(100, 200, 230)),
            )));
        }
        GitAuthStatus::NotAuthenticated => {
            body.push(Line::from(Span::styled(
                "\u{1f511} Can't clone this repo without GitHub auth.",
                Style::default().fg(amber),
            )));
            // The verbatim probe output — the specific error, not a generic
            // "auth failed". Capped to keep the modal bounded.
            if let Some(err) =
                state.git_auth_error.as_deref().map(str::trim).filter(|s| !s.is_empty())
            {
                body.push(Line::from(""));
                body.push(Line::from(Span::styled(
                    "gh auth status:",
                    Style::default().fg(MUTED_GRAY),
                )));
                let err_lines: Vec<&str> = err.lines().collect();
                for l in err_lines.iter().take(MAX_ERR_LINES) {
                    body.push(Line::from(Span::styled(
                        (*l).to_string(),
                        Style::default().fg(red),
                    )));
                }
                if err_lines.len() > MAX_ERR_LINES {
                    body.push(Line::from(Span::styled(
                        format!("\u{2026} ({} more lines)", err_lines.len() - MAX_ERR_LINES),
                        Style::default().fg(MUTED_GRAY),
                    )));
                }
            }
            body.push(Line::from(""));
            body.push(Line::from(Span::styled(
                "Fix \u{2014} in another terminal run:",
                Style::default().fg(MUTED_GRAY),
            )));
            body.push(Line::from(Span::styled(
                "  gh auth login && gh auth setup-git",
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            )));
        }
        // Returned early above; kept exhaustive without a panic landmine.
        GitAuthStatus::Authenticated => return,
    }
    body.push(Line::from("")); // gap before the pinned CTA footer

    let cta = match status {
        GitAuthStatus::Checking => Line::from(vec![
            Span::styled(
                "Esc",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Cancel", Style::default().fg(MUTED_GRAY)),
        ]),
        _ => Line::from(vec![
            Span::styled(
                "Enter",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Retry   ", Style::default().fg(MUTED_GRAY)),
            Span::styled("s", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" Skip auth   ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "Esc",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Dismiss", Style::default().fg(MUTED_GRAY)),
        ]),
    };

    // Size to content: body + 1 CTA row + 2 border rows, clamped to the area.
    // Width never exceeds the area (centred when it fits).
    let height = (body.len() as u16 + 3).min(area.height.max(3));
    let width = area.width.saturating_sub(4).clamp(20, 76).min(area.width);
    let modal = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            title,
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center)
        .style(Style::default().bg(DARK_BG));
    let inner = block.inner(modal);
    f.render_widget(Clear, modal);
    f.render_widget(block, modal);

    // Body takes all rows but the last; the CTA is pinned to the final row so
    // it survives even when the body is clipped on a short terminal.
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    f.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(SOFT_WHITE)),
        parts[0],
    );
    f.render_widget(Paragraph::new(cta).alignment(Alignment::Center), parts[1]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::keymap::{Chord, Key, Mods};
    use crate::config::favorites_store::FavoritesStore;
    use crate::config::favorites_store::SourceType;

    fn mk_state_with_rows(rows: Vec<PickRepoRow>) -> PickRepoState {
        let filtered_indices: Vec<usize> = (0..rows.len()).collect();
        PickRepoState {
            filter: String::new(),
            rows,
            filtered_indices,
            selected: 0,
            clone_progress: None,
            git_auth_status: None,
            pending_clone_source: None,
            git_auth_error: None,
            defaults: SessionDefaults::default(),
            favorites: FavoritesStore::default(),
        }
    }

    #[test]
    fn refilter_preserves_highlight_when_row_still_matches() {
        let mut s = mk_state_with_rows(vec![
            PickRepoRow {
                id: "ainb-tui".into(),
                label: "ainb-tui".into(),
                source: RepoSource::Filter("ainb-tui".into()),
                kind: RowKind::Favorite,
            },
            PickRepoRow {
                id: "agents".into(),
                label: "agents".into(),
                source: RepoSource::Filter("agents".into()),
                kind: RowKind::Favorite,
            },
        ]);
        s.selected = 1; // highlight agents
        s.filter = "agent".into();
        s.refilter();
        // After filter, only one row visible — that one is highlighted.
        assert_eq!(s.filtered_indices.len(), 1);
        assert_eq!(s.highlighted().unwrap().id, "agents");
    }

    #[test]
    fn resolve_outcome_dispatches_correctly() {
        assert_eq!(
            resolve_outcome(RepoSource::LocalPath(PathBuf::from("/x"))),
            PickRepoOutcome::AdvanceTo(RepoSource::LocalPath(PathBuf::from("/x")))
        );
        assert_eq!(
            resolve_outcome(RepoSource::Filter("x".into())),
            PickRepoOutcome::Stay
        );
        assert!(matches!(
            resolve_outcome(RepoSource::GithubShorthand {
                owner: "foo".into(),
                repo: "bar".into()
            }),
            PickRepoOutcome::StartClone(_)
        ));
        assert!(matches!(
            resolve_outcome(RepoSource::SshSession("ssh://x".into())),
            PickRepoOutcome::AdvanceTo(_)
        ));
    }

    #[test]
    fn build_rows_orders_favorites_then_recents_then_locals() {
        use chrono::Utc;
        let mut favorites = FavoritesStore::default();
        favorites
            .add(Favorite::new(
                "fav-a".into(),
                "owner/fav-a".into(),
                SourceType::GithubShorthand,
            ))
            .unwrap();
        let mut defaults = SessionDefaults::default();
        defaults.per_repo.insert(
            "recent-1".into(),
            crate::config::session_defaults::PerRepoDefaults {
                last_used_at: Utc::now(),
                ..Default::default()
            },
        );
        let locals = vec![PathBuf::from("/tmp/local-1")];

        let rows = build_rows(&favorites, &defaults, &locals);
        assert_eq!(rows[0].kind, RowKind::Favorite);
        assert_eq!(rows[0].id, "fav-a");
        assert_eq!(rows[1].kind, RowKind::Recent);
        assert_eq!(rows[1].id, "recent-1");
        assert_eq!(rows[2].kind, RowKind::Local);
    }

    #[test]
    fn pick_default_selection_finds_last_repo() {
        let rows = vec![
            PickRepoRow {
                id: "a".into(),
                label: "a".into(),
                source: RepoSource::Filter("a".into()),
                kind: RowKind::Favorite,
            },
            PickRepoRow {
                id: "b".into(),
                label: "b".into(),
                source: RepoSource::Filter("b".into()),
                kind: RowKind::Favorite,
            },
        ];
        let filtered = vec![0, 1];
        let defaults = SessionDefaults {
            last_repo: Some("b".into()),
            ..Default::default()
        };
        assert_eq!(pick_default_selection(&rows, &filtered, &defaults), 1);
    }

    #[test]
    fn pick_default_selection_falls_back_to_zero() {
        let rows = vec![PickRepoRow {
            id: "a".into(),
            label: "a".into(),
            source: RepoSource::Filter("a".into()),
            kind: RowKind::Favorite,
        }];
        let filtered = vec![0];
        let defaults = SessionDefaults {
            last_repo: Some("nonexistent".into()),
            ..Default::default()
        };
        assert_eq!(pick_default_selection(&rows, &filtered, &defaults), 0);
    }

    #[test]
    fn row_detail_renders_locator_per_source() {
        // A path outside home is shown unchanged (home collapsing is covered
        // separately so this stays independent of the test environment).
        assert_eq!(
            row_detail(&RepoSource::LocalPath(PathBuf::from("/opt/repos/Rosetta"))).as_deref(),
            Some("/opt/repos/Rosetta")
        );
        assert_eq!(
            row_detail(&RepoSource::HttpsUrl("https://github.com/o/r.git".into())).as_deref(),
            Some("https://github.com/o/r.git")
        );
        assert_eq!(
            row_detail(&RepoSource::SshUrl("git@github.com:o/r.git".into())).as_deref(),
            Some("git@github.com:o/r.git")
        );
        assert_eq!(
            row_detail(&RepoSource::SshSession("ssh://deploy@prod-1".into())).as_deref(),
            Some("ssh://deploy@prod-1")
        );
        let shorthand = RepoSource::GithubShorthand {
            owner: "o".into(),
            repo: "r".into(),
        };
        assert_eq!(row_detail(&shorthand).as_deref(), Some("o/r"));
        // Unparseable filter text carries no real source — nothing to show.
        assert_eq!(
            row_detail(&RepoSource::Filter("rose".into())).as_deref(),
            None
        );
    }

    #[test]
    fn abbreviate_home_collapses_home_prefix() {
        let Some(home) = dirs::home_dir() else {
            return; // No home dir in this environment — nothing to assert.
        };
        let nested = home.join("Code-Zero").join("Rosetta");
        let expected = Path::new("~").join("Code-Zero").join("Rosetta");
        assert_eq!(abbreviate_home(&nested), expected.display().to_string());
        assert_eq!(abbreviate_home(&home), "~");
        let outside = Path::new("/opt/elsewhere/Rosetta");
        assert_eq!(abbreviate_home(outside), outside.display().to_string());
    }

    fn one_row() -> Vec<PickRepoRow> {
        vec![PickRepoRow {
            id: "shotclubhouse".into(),
            label: "shotclubhouse".into(),
            source: RepoSource::Filter("shotclubhouse".into()),
            kind: RowKind::Favorite,
        }]
    }

    #[test]
    fn ctrl_v_requests_clipboard_paste() {
        let mut s = mk_state_with_rows(one_row());
        let ctrl_v = Chord::new(Key::Char('v'), Mods::CTRL);
        assert_eq!(
            handle_key(&mut s, &ctrl_v),
            PickRepoOutcome::PasteFromClipboard
        );
        // The keystroke must NOT also type a literal 'v' into the filter.
        assert_eq!(s.filter, "");
    }

    #[test]
    fn plain_v_still_types_into_filter() {
        let mut s = mk_state_with_rows(one_row());
        let v = Chord::new(Key::Char('v'), Mods::NONE);
        assert_eq!(handle_key(&mut s, &v), PickRepoOutcome::Stay);
        assert_eq!(s.filter, "v");
    }

    #[test]
    fn append_filter_strips_control_chars_and_refilters() {
        let mut s = mk_state_with_rows(one_row());
        // Simulate a pasted "owner/repo" with a trailing newline.
        s.append_filter("shotclub\n");
        assert_eq!(s.filter, "shotclub");
        // The single row still matches the prefix, so it stays visible.
        assert_eq!(s.filtered_indices.len(), 1);
        // A non-matching paste empties the filtered list.
        s.append_filter("zzz");
        assert_eq!(s.filter, "shotclubzzz");
        assert_eq!(s.filtered_indices.len(), 0);
    }

    // ── GitHub auth pre-check: prompt UI + key FSM ──────────────────────────
    //
    // These exercise the inline "auth required" prompt entirely from in-memory
    // state — no `gh`, no network, no real GitHub credentials. That's the
    // sandbox harness for verifying the prompt renders and reacts correctly
    // when `gh auth status` would have failed (the dispatcher sets
    // `NotAuthenticated`; here we set it directly and assert behaviour).

    fn github_source() -> RepoSource {
        RepoSource::GithubShorthand {
            owner: "stevengonsalvez".into(),
            repo: "agents-in-a-box".into(),
        }
    }

    /// Render the picker and flatten the terminal buffer to a single string so
    /// tests can assert on visible copy without a real terminal.
    fn render_to_string(state: &PickRepoState, w: u16, h: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.size();
                render(f, state, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.get(x, y).symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn auth_prompt_renders_instructions_without_invoking_gh() {
        let mut s = mk_state_with_rows(one_row());
        s.git_auth_status = Some(GitAuthStatus::NotAuthenticated);
        s.pending_clone_source = Some(github_source());

        let text = render_to_string(&s, 100, 24);

        // The amber instruction line + the exact remediation command.
        assert!(
            text.contains("auth required"),
            "missing auth prompt in:\n{text}"
        );
        assert!(
            text.contains("gh auth login"),
            "missing gh remediation command in:\n{text}"
        );
        // The three key hints the user can act on.
        assert!(text.contains("Retry"), "missing Retry hint in:\n{text}");
        assert!(text.contains("Skip"), "missing Skip hint in:\n{text}");
        assert!(text.contains("Dismiss"), "missing Dismiss hint in:\n{text}");
    }

    #[test]
    fn not_authenticated_modal_shows_exact_error_even_with_no_rows() {
        // The regression: a typed `owner/repo` with no local match leaves the
        // filtered list empty (no selected row). The auth failure must still
        // render — with the EXACT gh output — instead of hanging silently.
        let mut s = mk_state_with_rows(vec![]);
        s.git_auth_status = Some(GitAuthStatus::NotAuthenticated);
        s.pending_clone_source = Some(github_source());
        s.git_auth_error =
            Some("You are not logged into any GitHub hosts. To log in, run: gh auth login".into());

        let text = render_to_string(&s, 100, 24);

        assert!(
            text.contains("auth required"),
            "modal not shown on empty list:\n{text}"
        );
        assert!(
            text.contains("not logged into any GitHub hosts"),
            "exact gh error not surfaced:\n{text}"
        );
        assert!(text.contains("Dismiss"), "missing Dismiss CTA:\n{text}");
    }

    #[test]
    fn modal_keeps_dismiss_cta_visible_with_a_long_error() {
        // A chatty `gh` must not push the CTA off the bottom of the modal — the
        // footer is pinned. Constrained viewport: the pre-fix layout clipped it.
        let mut s = mk_state_with_rows(vec![]);
        s.git_auth_status = Some(GitAuthStatus::NotAuthenticated);
        s.pending_clone_source = Some(github_source());
        let long = (0..30).map(|i| format!("error line {i}")).collect::<Vec<_>>().join("\n");
        s.git_auth_error = Some(long);

        let text = render_to_string(&s, 100, 16);

        assert!(
            text.contains("Dismiss"),
            "Dismiss CTA clipped on long error:\n{text}"
        );
    }

    #[test]
    fn modal_caps_a_chatty_gh_error() {
        let mut s = mk_state_with_rows(vec![]);
        s.git_auth_status = Some(GitAuthStatus::NotAuthenticated);
        let long = (0..30).map(|i| format!("noisy line {i}")).collect::<Vec<_>>().join("\n");
        s.git_auth_error = Some(long);

        let text = render_to_string(&s, 100, 40);

        assert!(
            text.contains("more lines"),
            "error body not capped:\n{text}"
        );
    }

    #[test]
    fn checking_state_renders_spinner_line() {
        let mut s = mk_state_with_rows(one_row());
        s.git_auth_status = Some(GitAuthStatus::Checking);
        let text = render_to_string(&s, 100, 24);
        assert!(
            text.contains("Checking GitHub auth"),
            "missing checking spinner in:\n{text}"
        );
    }

    #[test]
    fn not_authenticated_enter_retries_the_check() {
        let mut s = mk_state_with_rows(one_row());
        s.git_auth_status = Some(GitAuthStatus::NotAuthenticated);
        s.pending_clone_source = Some(github_source());

        let out = handle_key(&mut s, &Chord::new(Key::Enter, Mods::NONE));

        // Enter flips back to Checking so the dispatcher re-runs `gh auth status`.
        assert_eq!(out, PickRepoOutcome::Stay);
        assert_eq!(s.git_auth_status, Some(GitAuthStatus::Checking));
        // The source is preserved across the retry.
        assert_eq!(s.pending_clone_source, Some(github_source()));
    }

    #[test]
    fn not_authenticated_skip_starts_the_clone_anyway() {
        let mut s = mk_state_with_rows(one_row());
        s.git_auth_status = Some(GitAuthStatus::NotAuthenticated);
        s.pending_clone_source = Some(github_source());

        let out = handle_key(&mut s, &Chord::new(Key::Char('s'), Mods::NONE));

        // `s` bypasses the gate and proceeds to clone with the pending source.
        assert_eq!(out, PickRepoOutcome::StartClone(github_source()));
        assert_eq!(s.git_auth_status, None);
        assert_eq!(s.pending_clone_source, None);
    }

    #[test]
    fn not_authenticated_esc_cancels_the_prompt() {
        let mut s = mk_state_with_rows(one_row());
        s.git_auth_status = Some(GitAuthStatus::NotAuthenticated);
        s.pending_clone_source = Some(github_source());

        let out = handle_key(&mut s, &Chord::new(Key::Esc, Mods::NONE));

        // Esc dismisses the prompt and clears the pending source.
        assert_eq!(out, PickRepoOutcome::Stay);
        assert_eq!(s.git_auth_status, None);
        assert_eq!(s.pending_clone_source, None);
    }

    #[test]
    fn checking_state_swallows_keys_except_esc() {
        let mut s = mk_state_with_rows(one_row());
        s.git_auth_status = Some(GitAuthStatus::Checking);
        s.pending_clone_source = Some(github_source());

        // A stray keypress while the check is in flight is ignored, state holds.
        let out = handle_key(&mut s, &Chord::new(Key::Char('x'), Mods::NONE));
        assert_eq!(out, PickRepoOutcome::Stay);
        assert_eq!(s.git_auth_status, Some(GitAuthStatus::Checking));

        // Esc aborts the in-flight check and drops the pending source.
        let out = handle_key(&mut s, &Chord::new(Key::Esc, Mods::NONE));
        assert_eq!(out, PickRepoOutcome::Stay);
        assert_eq!(s.git_auth_status, None);
        assert_eq!(s.pending_clone_source, None);
    }
}
