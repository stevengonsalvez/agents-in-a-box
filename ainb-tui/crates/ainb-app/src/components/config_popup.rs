// ABOUTME: Renderer-agnostic half of the `config_popup` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::config_popup`, which re-exports this module.

/// Type of popup being shown
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigPopupType {
    /// Selection from a list of choices
    Choice {
        options: Vec<String>,
        selected_index: usize,
    },
    /// Text input field
    TextInput {
        value: String,
        cursor_position: usize,
    },
    /// Boolean toggle (shows Yes/No options)
    Boolean { value: bool },
    /// Number input
    NumberInput { value: i64, input_buffer: String },
}

/// State for the config popup
#[derive(Debug, Clone)]
pub struct ConfigPopupState {
    /// Whether the popup is visible
    pub show_popup: bool,
    /// Title for the popup
    pub title: String,
    /// Description/hint text
    pub description: String,
    /// The setting key being edited
    pub setting_key: String,
    /// Type and state of the popup
    pub popup_type: ConfigPopupType,
}

impl Default for ConfigPopupState {
    fn default() -> Self {
        Self {
            show_popup: false,
            title: String::new(),
            description: String::new(),
            setting_key: String::new(),
            popup_type: ConfigPopupType::Choice {
                options: vec![],
                selected_index: 0,
            },
        }
    }
}

impl ConfigPopupState {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when the popup is visible AND its variant captures
    /// character input (Text/Number). Choice and Boolean popups are
    /// navigation-only (arrow keys / Enter), so callers checking
    /// "should bare `Char(_)` keys go to the popup" should consult
    /// this — not `show_popup` alone.
    pub fn is_text_entry(&self) -> bool {
        self.show_popup
            && matches!(
                self.popup_type,
                ConfigPopupType::TextInput { .. } | ConfigPopupType::NumberInput { .. }
            )
    }

    /// Open popup for a choice setting
    pub fn open_choice(
        &mut self,
        title: &str,
        description: &str,
        key: &str,
        options: Vec<String>,
        current_index: usize,
    ) {
        self.show_popup = true;
        self.title = title.to_string();
        self.description = description.to_string();
        self.setting_key = key.to_string();
        self.popup_type = ConfigPopupType::Choice {
            options,
            selected_index: current_index,
        };
    }

    /// Open popup for a text setting
    pub fn open_text(&mut self, title: &str, description: &str, key: &str, current_value: &str) {
        self.show_popup = true;
        self.title = title.to_string();
        self.description = description.to_string();
        self.setting_key = key.to_string();
        let len = current_value.len();
        self.popup_type = ConfigPopupType::TextInput {
            value: current_value.to_string(),
            cursor_position: len,
        };
    }

    /// Open popup for a boolean setting
    pub fn open_boolean(&mut self, title: &str, description: &str, key: &str, current_value: bool) {
        self.show_popup = true;
        self.title = title.to_string();
        self.description = description.to_string();
        self.setting_key = key.to_string();
        self.popup_type = ConfigPopupType::Boolean {
            value: current_value,
        };
    }

    /// Open popup for a number setting
    pub fn open_number(&mut self, title: &str, description: &str, key: &str, current_value: i64) {
        self.show_popup = true;
        self.title = title.to_string();
        self.description = description.to_string();
        self.setting_key = key.to_string();
        self.popup_type = ConfigPopupType::NumberInput {
            value: current_value,
            input_buffer: current_value.to_string(),
        };
    }

    /// Close the popup
    pub fn close(&mut self) {
        self.show_popup = false;
    }

    /// Navigate up in choice list
    pub fn navigate_up(&mut self) {
        match &mut self.popup_type {
            ConfigPopupType::Choice {
                options,
                selected_index,
            } => {
                if !options.is_empty() {
                    *selected_index = selected_index.checked_sub(1).unwrap_or(options.len() - 1);
                }
            }
            ConfigPopupType::Boolean { value } => {
                *value = !*value;
            }
            _ => {}
        }
    }

    /// Navigate down in choice list
    pub fn navigate_down(&mut self) {
        match &mut self.popup_type {
            ConfigPopupType::Choice {
                options,
                selected_index,
            } => {
                if !options.is_empty() {
                    *selected_index = (*selected_index + 1) % options.len();
                }
            }
            ConfigPopupType::Boolean { value } => {
                *value = !*value;
            }
            _ => {}
        }
    }

    /// Input character for text/number input
    pub fn input_char(&mut self, c: char) {
        match &mut self.popup_type {
            ConfigPopupType::TextInput {
                value,
                cursor_position,
            } => {
                value.insert(*cursor_position, c);
                *cursor_position += c.len_utf8();
            }
            ConfigPopupType::NumberInput { input_buffer, .. } => {
                if c.is_ascii_digit() || (c == '-' && input_buffer.is_empty()) {
                    input_buffer.push(c);
                }
            }
            _ => {}
        }
    }

    /// Insert a string at the cursor (paste). Newlines and other control
    /// characters are stripped because these popups are single-line fields —
    /// a pasted path with a trailing `\n` should not break the layout.
    pub fn insert_str(&mut self, s: &str) {
        match &mut self.popup_type {
            ConfigPopupType::TextInput {
                value,
                cursor_position,
            } => {
                let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
                value.insert_str(*cursor_position, &cleaned);
                *cursor_position += cleaned.len();
            }
            ConfigPopupType::NumberInput { input_buffer, .. } => {
                for c in s.chars() {
                    if c.is_ascii_digit() || (c == '-' && input_buffer.is_empty()) {
                        input_buffer.push(c);
                    }
                }
            }
            _ => {}
        }
    }

    /// Backspace for text/number input
    pub fn backspace(&mut self) {
        match &mut self.popup_type {
            ConfigPopupType::TextInput {
                value,
                cursor_position,
            } => {
                if *cursor_position > 0 {
                    // Step back to the previous char boundary so multibyte
                    // characters are removed whole.
                    let mut new_pos = *cursor_position - 1;
                    while new_pos > 0 && !value.is_char_boundary(new_pos) {
                        new_pos -= 1;
                    }
                    value.remove(new_pos);
                    *cursor_position = new_pos;
                }
            }
            ConfigPopupType::NumberInput { input_buffer, .. } => {
                input_buffer.pop();
            }
            _ => {}
        }
    }

    /// Forward-delete the character under the cursor (Delete key).
    pub fn delete_forward(&mut self) {
        if let ConfigPopupType::TextInput {
            value,
            cursor_position,
        } = &mut self.popup_type
        {
            if *cursor_position < value.len() {
                value.remove(*cursor_position);
            }
        }
    }

    /// Move the cursor one character left (text input only).
    pub fn cursor_left(&mut self) {
        if let ConfigPopupType::TextInput {
            value,
            cursor_position,
        } = &mut self.popup_type
        {
            if *cursor_position > 0 {
                let mut new_pos = *cursor_position - 1;
                while new_pos > 0 && !value.is_char_boundary(new_pos) {
                    new_pos -= 1;
                }
                *cursor_position = new_pos;
            }
        }
    }

    /// Move the cursor one character right (text input only).
    pub fn cursor_right(&mut self) {
        if let ConfigPopupType::TextInput {
            value,
            cursor_position,
        } = &mut self.popup_type
        {
            if *cursor_position < value.len() {
                let mut new_pos = *cursor_position + 1;
                while new_pos < value.len() && !value.is_char_boundary(new_pos) {
                    new_pos += 1;
                }
                *cursor_position = new_pos;
            }
        }
    }

    /// Move the cursor to the start of the field (Home).
    pub fn cursor_home(&mut self) {
        if let ConfigPopupType::TextInput {
            cursor_position, ..
        } = &mut self.popup_type
        {
            *cursor_position = 0;
        }
    }

    /// Move the cursor to the end of the field (End).
    pub fn cursor_end(&mut self) {
        if let ConfigPopupType::TextInput {
            value,
            cursor_position,
        } = &mut self.popup_type
        {
            *cursor_position = value.len();
        }
    }

    /// Get the current value to save
    pub fn get_value(&self) -> Option<ConfigPopupValue> {
        match &self.popup_type {
            ConfigPopupType::Choice {
                options,
                selected_index,
            } => options
                .get(*selected_index)
                .map(|s| ConfigPopupValue::Choice(s.clone(), *selected_index)),
            ConfigPopupType::TextInput { value, .. } => Some(ConfigPopupValue::Text(value.clone())),
            ConfigPopupType::Boolean { value } => Some(ConfigPopupValue::Boolean(*value)),
            ConfigPopupType::NumberInput {
                input_buffer,
                value,
            } => {
                let num = input_buffer.parse::<i64>().unwrap_or(*value);
                Some(ConfigPopupValue::Number(num))
            }
        }
    }
}

/// Value returned from the popup
#[derive(Debug, Clone)]
pub enum ConfigPopupValue {
    Choice(String, usize),
    Text(String),
    Boolean(bool),
    Number(i64),
}
