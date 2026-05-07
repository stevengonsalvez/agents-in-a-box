// ABOUTME: Slash-command palette — open with `:`, type `/<cmd>`, Enter to run.
// Phase 2b stub: registry + palette state + key handling. Plugin-contributed
// slash commands hook in here in Phase 4 via `SlashCommandRegistry::register`.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

/// One discoverable slash command (e.g. `/help`).
pub trait SlashCommand: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
}

pub struct HelpCommand;
impl SlashCommand for HelpCommand {
    fn name(&self) -> &'static str {
        "help"
    }
    fn description(&self) -> &'static str {
        "Show help"
    }
}

pub struct QuitCommand;
impl SlashCommand for QuitCommand {
    fn name(&self) -> &'static str {
        "quit"
    }
    fn description(&self) -> &'static str {
        "Quit ainb"
    }
}

/// Registry of built-in + plugin-contributed slash commands.
#[derive(Default)]
pub struct SlashCommandRegistry {
    commands: Vec<Box<dyn SlashCommand>>,
}

impl SlashCommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    /// Built-ins: `/help`, `/quit`. Plugins extend via `register()` in Phase 4.
    pub fn built_ins() -> Self {
        let mut r = Self::new();
        r.register(Box::new(HelpCommand));
        r.register(Box::new(QuitCommand));
        r
    }

    pub fn register(&mut self, cmd: Box<dyn SlashCommand>) {
        self.commands.push(cmd);
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.commands.iter().map(|c| c.name()).collect()
    }

    /// Filter commands whose name starts with `prefix` (after stripping leading `/`).
    pub fn filter(&self, prefix: &str) -> Vec<(&'static str, &'static str)> {
        let needle = prefix.trim_start_matches('/');
        self.commands
            .iter()
            .filter(|c| c.name().starts_with(needle))
            .map(|c| (c.name(), c.description()))
            .collect()
    }
}

/// Action emitted by `SlashPalette::handle_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashAction {
    /// No state change.
    None,
    /// Palette opened.
    Opened,
    /// Palette closed (Esc or Enter on empty).
    Closed,
    /// Enter pressed with a non-empty buffer — caller should dispatch.
    Execute(String),
}

/// Palette state: open/closed + input buffer.
#[derive(Default)]
pub struct SlashPalette {
    open: bool,
    input: String,
    pub registry: SlashCommandRegistry,
}

impl SlashPalette {
    pub fn new(registry: SlashCommandRegistry) -> Self {
        Self {
            open: false,
            input: String::new(),
            registry,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn open(&mut self) {
        self.open = true;
        self.input.clear();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.input.clear();
    }

    /// Returns the current set of suggestions matching `input`.
    pub fn suggestions(&self) -> Vec<(&'static str, &'static str)> {
        self.registry.filter(&self.input)
    }

    /// Drive palette state from a key event. Caller should give us only
    /// `KeyEventKind::Press` events. Returns the resulting action.
    pub fn handle_key(&mut self, key: KeyEvent) -> SlashAction {
        if key.kind == KeyEventKind::Release {
            return SlashAction::None;
        }

        if !self.open {
            // `:` opens the palette (vim-style command line).
            if matches!(key.code, KeyCode::Char(':')) {
                self.open();
                return SlashAction::Opened;
            }
            return SlashAction::None;
        }

        match key.code {
            KeyCode::Esc => {
                self.close();
                SlashAction::Closed
            }
            KeyCode::Backspace => {
                self.input.pop();
                SlashAction::None
            }
            KeyCode::Enter => {
                if self.input.is_empty() {
                    self.close();
                    SlashAction::Closed
                } else {
                    let cmd = self.input.trim_start_matches('/').to_string();
                    self.close();
                    SlashAction::Execute(cmd)
                }
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                SlashAction::None
            }
            _ => SlashAction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn registry_built_ins_contain_help_and_quit() {
        let r = SlashCommandRegistry::built_ins();
        let names = r.names();
        assert!(
            names.contains(&"help"),
            "expected /help in built-ins, got {:?}",
            names
        );
        assert!(
            names.contains(&"quit"),
            "expected /quit in built-ins, got {:?}",
            names
        );
    }

    #[test]
    fn registry_register_appends_command() {
        struct Foo;
        impl SlashCommand for Foo {
            fn name(&self) -> &'static str {
                "foo"
            }
            fn description(&self) -> &'static str {
                ""
            }
        }
        let mut r = SlashCommandRegistry::new();
        r.register(Box::new(Foo));
        assert_eq!(r.names(), vec!["foo"]);
    }

    #[test]
    fn registry_filter_matches_prefix() {
        let r = SlashCommandRegistry::built_ins();
        let hits = r.filter("/he");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "help");
    }

    #[test]
    fn slash_palette_opens_on_colon_key() {
        let mut p = SlashPalette::new(SlashCommandRegistry::built_ins());
        assert!(!p.is_open(), "palette should start closed");

        let action = p.handle_key(press(KeyCode::Char(':')));
        assert_eq!(action, SlashAction::Opened);
        assert!(p.is_open(), "palette should be open after `:`");
        assert_eq!(p.input(), "", "input should be empty when freshly opened");
    }

    #[test]
    fn palette_ignores_other_keys_when_closed() {
        let mut p = SlashPalette::new(SlashCommandRegistry::built_ins());
        let action = p.handle_key(press(KeyCode::Char('q')));
        assert_eq!(action, SlashAction::None);
        assert!(!p.is_open());
    }

    #[test]
    fn palette_typing_updates_input() {
        let mut p = SlashPalette::new(SlashCommandRegistry::built_ins());
        p.handle_key(press(KeyCode::Char(':')));
        p.handle_key(press(KeyCode::Char('/')));
        p.handle_key(press(KeyCode::Char('h')));
        p.handle_key(press(KeyCode::Char('e')));
        assert_eq!(p.input(), "/he");
        let sugg = p.suggestions();
        assert_eq!(sugg.len(), 1);
        assert_eq!(sugg[0].0, "help");
    }

    #[test]
    fn palette_enter_with_input_emits_execute() {
        let mut p = SlashPalette::new(SlashCommandRegistry::built_ins());
        p.handle_key(press(KeyCode::Char(':')));
        p.handle_key(press(KeyCode::Char('/')));
        p.handle_key(press(KeyCode::Char('h')));
        p.handle_key(press(KeyCode::Char('e')));
        p.handle_key(press(KeyCode::Char('l')));
        p.handle_key(press(KeyCode::Char('p')));
        let action = p.handle_key(press(KeyCode::Enter));
        assert_eq!(action, SlashAction::Execute("help".into()));
        assert!(!p.is_open(), "palette should close after execute");
    }

    #[test]
    fn palette_esc_closes_without_executing() {
        let mut p = SlashPalette::new(SlashCommandRegistry::built_ins());
        p.handle_key(press(KeyCode::Char(':')));
        p.handle_key(press(KeyCode::Char('x')));
        let action = p.handle_key(press(KeyCode::Esc));
        assert_eq!(action, SlashAction::Closed);
        assert!(!p.is_open());
        assert_eq!(p.input(), "");
    }

    #[test]
    fn palette_backspace_removes_last_char() {
        let mut p = SlashPalette::new(SlashCommandRegistry::built_ins());
        p.handle_key(press(KeyCode::Char(':')));
        p.handle_key(press(KeyCode::Char('a')));
        p.handle_key(press(KeyCode::Char('b')));
        p.handle_key(press(KeyCode::Backspace));
        assert_eq!(p.input(), "a");
    }
}
