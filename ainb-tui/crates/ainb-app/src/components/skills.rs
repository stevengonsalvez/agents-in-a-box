// ABOUTME: Renderer-agnostic half of the `skills` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::skills`, which re-exports this module.

use crate::models::skills::{AgentDef, Skill, SkillsData};

/// Which agent provider's skills to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillsProvider {
    #[default]
    Claude,
    Codex,
    Gemini,
    Copilot,
}

impl SkillsProvider {
    pub fn all() -> &'static [SkillsProvider] {
        &[
            SkillsProvider::Claude,
            SkillsProvider::Codex,
            SkillsProvider::Gemini,
            SkillsProvider::Copilot,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            SkillsProvider::Claude => "✻ Claude Code",
            SkillsProvider::Codex => "✦ Codex CLI",
            SkillsProvider::Gemini => "✨ Gemini CLI",
            SkillsProvider::Copilot => "🐙 Copilot",
        }
    }

    pub fn has_data(&self) -> bool {
        matches!(self, SkillsProvider::Claude)
    }

    fn next(&self) -> Self {
        match self {
            SkillsProvider::Claude => SkillsProvider::Codex,
            SkillsProvider::Codex => SkillsProvider::Gemini,
            SkillsProvider::Gemini => SkillsProvider::Copilot,
            SkillsProvider::Copilot => SkillsProvider::Claude,
        }
    }

    fn prev(&self) -> Self {
        match self {
            SkillsProvider::Claude => SkillsProvider::Copilot,
            SkillsProvider::Codex => SkillsProvider::Claude,
            SkillsProvider::Gemini => SkillsProvider::Codex,
            SkillsProvider::Copilot => SkillsProvider::Gemini,
        }
    }
}

/// Which sub-tab is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillsTab {
    #[default]
    Skills,
    Agents,
    Associations,
}

impl SkillsTab {
    pub fn all() -> &'static [SkillsTab] {
        &[
            SkillsTab::Skills,
            SkillsTab::Agents,
            SkillsTab::Associations,
        ]
    }

    pub fn title(&self) -> &'static str {
        match self {
            SkillsTab::Skills => "Skills",
            SkillsTab::Agents => "Agents",
            SkillsTab::Associations => "Associations",
        }
    }

    fn next(&self) -> Self {
        match self {
            SkillsTab::Skills => SkillsTab::Agents,
            SkillsTab::Agents => SkillsTab::Associations,
            SkillsTab::Associations => SkillsTab::Skills,
        }
    }

    fn prev(&self) -> Self {
        match self {
            SkillsTab::Skills => SkillsTab::Associations,
            SkillsTab::Agents => SkillsTab::Skills,
            SkillsTab::Associations => SkillsTab::Agents,
        }
    }
}

/// View state for the Skills screen.
#[derive(Debug, Clone)]
pub struct SkillsViewState {
    pub provider: SkillsProvider,
    pub active_tab: SkillsTab,
    pub data: Option<SkillsData>,
    pub loading: bool,
    pub selected_index: usize,
    pub search_active: bool,
    pub search_query: String,
}

impl Default for SkillsViewState {
    fn default() -> Self {
        Self {
            provider: SkillsProvider::Claude,
            active_tab: SkillsTab::Skills,
            data: None,
            loading: false,
            selected_index: 0,
            search_active: false,
            search_query: String::new(),
        }
    }
}

impl SkillsViewState {
    // IMPORTANT: do NOT null `data` on provider switch — it stays cached,
    // mirroring the fix in UsageViewState.
    pub fn next_provider(&mut self) {
        self.provider = self.provider.next();
        self.selected_index = 0;
    }

    pub fn prev_provider(&mut self) {
        self.provider = self.provider.prev();
        self.selected_index = 0;
    }

    pub fn next_tab(&mut self) {
        self.active_tab = self.active_tab.next();
        self.selected_index = 0;
    }

    pub fn prev_tab(&mut self) {
        self.active_tab = self.active_tab.prev();
        self.selected_index = 0;
    }

    pub fn scroll_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    pub fn scroll_down(&mut self, max_rows: usize) {
        if max_rows == 0 {
            return;
        }
        if self.selected_index + 1 < max_rows {
            self.selected_index += 1;
        }
    }

    pub fn scroll_to_top(&mut self) {
        self.selected_index = 0;
    }

    pub fn scroll_to_bottom(&mut self, max_rows: usize) {
        if max_rows == 0 {
            self.selected_index = 0;
            return;
        }
        self.selected_index = max_rows - 1;
    }

    pub fn page_up(&mut self, page_size: usize) {
        self.selected_index = self.selected_index.saturating_sub(page_size);
    }

    pub fn page_down(&mut self, max_rows: usize, page_size: usize) {
        if max_rows == 0 {
            return;
        }
        self.selected_index = (self.selected_index + page_size).min(max_rows - 1);
    }

    pub fn search_push(&mut self, c: char) {
        self.search_query.push(c);
        self.selected_index = 0;
    }

    pub fn search_pop(&mut self) {
        self.search_query.pop();
        self.selected_index = 0;
    }

    /// Clamp `selected_index` to a shrinking row set (e.g. after a filter
    /// narrows results or the active provider/tab swaps).
    pub fn clamp_selection(&mut self, row_count: usize) {
        if row_count == 0 {
            self.selected_index = 0;
        } else if self.selected_index >= row_count {
            self.selected_index = row_count - 1;
        }
    }

    pub fn row_count(&self) -> usize {
        let Some(data) = &self.data else {
            return 0;
        };
        match self.active_tab {
            SkillsTab::Skills => filtered_skills(data, &self.search_query).len(),
            SkillsTab::Agents => filtered_agents(data, &self.search_query).len(),
            SkillsTab::Associations => association_rows(data, &self.search_query).len(),
        }
    }
}

fn matches_query(name: &str, description: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    name.to_lowercase().contains(&q) || description.to_lowercase().contains(&q)
}

pub fn filtered_skills<'a>(data: &'a SkillsData, query: &str) -> Vec<&'a Skill> {
    data.skills
        .iter()
        .filter(|s| matches_query(&s.name, &s.description, query))
        .collect()
}

pub fn filtered_agents<'a>(data: &'a SkillsData, query: &str) -> Vec<&'a AgentDef> {
    data.agents
        .iter()
        .filter(|a| matches_query(&a.name, &a.description, query))
        .collect()
}

/// Returns (skill_name, agent_names) pairs for skills that have at least one
/// associated agent, filtered by search query.
pub fn association_rows<'a>(data: &'a SkillsData, query: &str) -> Vec<(&'a Skill, Vec<&'a str>)> {
    let mut out: Vec<(&'a Skill, Vec<&'a str>)> = Vec::new();
    for skill in &data.skills {
        let agents = match data.associations.get(&skill.name) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };
        let joined = agents.join(" ");
        if !matches_query(&skill.name, &joined, query) {
            continue;
        }
        let refs: Vec<&str> = agents.iter().map(String::as_str).collect();
        out.push((skill, refs));
    }
    out
}
