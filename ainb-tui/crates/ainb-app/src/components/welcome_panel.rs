// ABOUTME: Renderer-agnostic half of the `welcome_panel` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::welcome_panel`, which re-exports this module.

/// Default markdown content for the welcome panel
pub const DEFAULT_WELCOME_CONTENT: &str = r#"# Agents in a Box

You're on the **Sessions** screen. Start an agent session, or open Setup.

---

## Start here

- `n`         **New session** — pick a repo and launch an agent
- `s`         **Sessions** — go to your running sessions
- `Enter`     Open / attach the selected session
- `1`–`9`     Jump straight to a session by number
- `?`         Help — every key for this screen
- `q`         Quit

To attach: `a` full-screen, `→` in a split pane, or `Enter`.

Each session runs in its own git worktree + tmux + agent, fully isolated.

---

💡 `Esc` in the setup wizard returns here. Press `?` for the full keymap.
"#;

/// Welcome panel state with scroll position
#[derive(Debug)]
pub struct WelcomePanelState {
    /// Whether the panel is focused
    pub is_focused: bool,
    /// Current scroll position (line offset)
    pub scroll_offset: u16,
    /// Total content height (set during render)
    pub content_height: u16,
    /// Visible height (set during render)
    pub visible_height: u16,
    /// The markdown content to display
    pub content: String,
}

impl WelcomePanelState {
    pub fn new() -> Self {
        Self {
            is_focused: false,
            scroll_offset: 0,
            content_height: 0,
            visible_height: 0,
            content: DEFAULT_WELCOME_CONTENT.to_string(),
        }
    }

    /// Scroll up by one line
    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    /// Scroll down by one line
    pub fn scroll_down(&mut self) {
        let max_scroll = self.content_height.saturating_sub(self.visible_height);
        if self.scroll_offset < max_scroll {
            self.scroll_offset += 1;
        }
    }

    /// Scroll up by a page
    pub fn page_up(&mut self) {
        let page_size = self.visible_height.saturating_sub(2);
        self.scroll_offset = self.scroll_offset.saturating_sub(page_size);
    }

    /// Scroll down by a page
    pub fn page_down(&mut self) {
        let page_size = self.visible_height.saturating_sub(2);
        let max_scroll = self.content_height.saturating_sub(self.visible_height);
        self.scroll_offset = (self.scroll_offset + page_size).min(max_scroll);
    }

    /// Set custom content
    pub fn set_content(&mut self, content: String) {
        self.content = content;
        self.scroll_offset = 0;
    }

    /// Copy the welcome panel content to the system clipboard
    pub fn copy_content_to_clipboard(&self) -> Result<(), String> {
        use arboard::Clipboard;
        let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
        clipboard.set_text(&self.content).map_err(|e| e.to_string())?;
        Ok(())
    }
}

impl Default for WelcomePanelState {
    fn default() -> Self {
        Self::new()
    }
}
