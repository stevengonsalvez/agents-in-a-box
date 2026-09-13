// ABOUTME: Single-buffer text input with a cursor, shared by the boss-mode prompt
// and the new-session Configure form.

/// Text editor with cursor support for boss mode prompts
#[derive(Debug, Clone)]
pub struct TextEditor {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
}

impl TextEditor {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
        }
    }

    pub fn from_string(text: &str) -> Self {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.lines().map(|s| s.to_string()).collect()
        };

        Self {
            lines,
            cursor_line: 0,
            cursor_col: 0,
        }
    }

    pub fn to_string(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Return the editor contents as `Some(String)` iff non-empty, else `None`.
    ///
    /// Collapses the "empty prompt → no prompt" idiom that was inlined at
    /// multiple sites in `configure.rs` (finding #17). Callers that need
    /// `Option<&str>` can chain `.as_deref()`.
    #[must_use]
    pub fn to_non_empty_string(&self) -> Option<String> {
        let s = self.to_string();
        if s.is_empty() { None } else { Some(s) }
    }

    pub fn insert_char(&mut self, ch: char) {
        if ch == '\n' {
            self.insert_newline();
        } else {
            let line = &mut self.lines[self.cursor_line];
            line.insert(self.cursor_col, ch);
            self.cursor_col += 1;
        }
    }

    pub fn insert_newline(&mut self) {
        let current_line = self.lines[self.cursor_line].clone();
        let (left, right) = current_line.split_at(self.cursor_col);

        self.lines[self.cursor_line] = left.to_string();
        self.lines.insert(self.cursor_line + 1, right.to_string());

        self.cursor_line += 1;
        self.cursor_col = 0;
    }

    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            // Delete character before cursor
            self.lines[self.cursor_line].remove(self.cursor_col - 1);
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            // Join with previous line
            let current_line = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
            self.lines[self.cursor_line].push_str(&current_line);
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor_col < self.lines[self.cursor_line].len() {
            self.cursor_col += 1;
        } else if self.cursor_line < self.lines.len() - 1 {
            self.cursor_line += 1;
            self.cursor_col = 0;
        }
    }

    pub fn move_cursor_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_line].len());
        }
    }

    pub fn move_cursor_down(&mut self) {
        if self.cursor_line < self.lines.len() - 1 {
            self.cursor_line += 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_line].len());
        }
    }

    pub fn move_to_line_start(&mut self) {
        self.cursor_col = 0;
    }

    pub fn move_to_line_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_line].len();
    }

    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let mut lines = text.lines();

        // Insert first line of text at current cursor position
        if let Some(first_line) = lines.next() {
            self.lines[self.cursor_line].insert_str(self.cursor_col, first_line);
            self.cursor_col += first_line.len();
        }

        // Insert newlines and subsequent lines
        for line in lines {
            self.insert_newline();
            self.lines[self.cursor_line].insert_str(self.cursor_col, line);
            self.cursor_col += line.len();
        }
    }

    pub fn get_cursor_position(&self) -> (usize, usize) {
        (self.cursor_line, self.cursor_col)
    }

    pub fn get_lines(&self) -> &Vec<String> {
        &self.lines
    }

    pub fn move_cursor_to_end(&mut self) {
        if !self.lines.is_empty() {
            self.cursor_line = self.lines.len() - 1;
            self.cursor_col = self.lines[self.cursor_line].len();
        }
    }

    pub fn set_cursor_position(&mut self, line: usize, col: usize) {
        if line < self.lines.len() {
            self.cursor_line = line;
            self.cursor_col = col.min(self.lines[line].len());
        }
    }

    // Word movement methods
    pub fn move_cursor_word_forward(&mut self) {
        let current_line = &self.lines[self.cursor_line];

        // If at end of line, move to next line
        if self.cursor_col >= current_line.len() {
            if self.cursor_line < self.lines.len() - 1 {
                self.cursor_line += 1;
                self.cursor_col = 0;
                // Find first non-whitespace character
                let next_line = &self.lines[self.cursor_line];
                while self.cursor_col < next_line.len()
                    && next_line.chars().nth(self.cursor_col).unwrap().is_whitespace()
                {
                    self.cursor_col += 1;
                }
            }
            return;
        }

        let chars: Vec<char> = current_line.chars().collect();
        let mut pos = self.cursor_col;

        // Skip current word
        while pos < chars.len()
            && !chars[pos].is_whitespace()
            && chars[pos] != '.'
            && chars[pos] != ','
        {
            pos += 1;
        }

        // Skip whitespace
        while pos < chars.len() && chars[pos].is_whitespace() {
            pos += 1;
        }

        self.cursor_col = pos;
    }

    pub fn move_cursor_word_backward(&mut self) {
        // If at beginning of line, move to end of previous line
        if self.cursor_col == 0 {
            if self.cursor_line > 0 {
                self.cursor_line -= 1;
                self.cursor_col = self.lines[self.cursor_line].len();
            }
            return;
        }

        let current_line = &self.lines[self.cursor_line];
        let chars: Vec<char> = current_line.chars().collect();
        let mut pos = self.cursor_col.saturating_sub(1);

        // Skip whitespace backwards
        while pos > 0 && chars[pos].is_whitespace() {
            pos = pos.saturating_sub(1);
        }

        // Skip word backwards
        while pos > 0 && !chars[pos].is_whitespace() && chars[pos] != '.' && chars[pos] != ',' {
            pos = pos.saturating_sub(1);
        }

        // If we stopped on whitespace or punctuation, move forward one
        if pos > 0 && (chars[pos].is_whitespace() || chars[pos] == '.' || chars[pos] == ',') {
            pos += 1;
        }

        self.cursor_col = pos;
    }

    // Word deletion methods
    pub fn delete_word_forward(&mut self) {
        let current_line_text = self.lines[self.cursor_line].clone();
        let chars: Vec<char> = current_line_text.chars().collect();
        let start_pos = self.cursor_col;

        if start_pos >= chars.len() {
            return;
        }

        let mut end_pos = start_pos;

        // Skip current word
        while end_pos < chars.len()
            && !chars[end_pos].is_whitespace()
            && chars[end_pos] != '.'
            && chars[end_pos] != ','
        {
            end_pos += 1;
        }

        // Skip following whitespace
        while end_pos < chars.len() && chars[end_pos].is_whitespace() {
            end_pos += 1;
        }

        // Remove the text
        let before: String = chars[..start_pos].iter().collect();
        let after: String = chars[end_pos..].iter().collect();
        self.lines[self.cursor_line] = format!("{}{}", before, after);
    }

    pub fn delete_word_backward(&mut self) {
        if self.cursor_col == 0 {
            return;
        }

        let current_line_text = self.lines[self.cursor_line].clone();
        let chars: Vec<char> = current_line_text.chars().collect();
        let end_pos = self.cursor_col;
        let mut start_pos = end_pos.saturating_sub(1);

        // Skip whitespace backwards
        while start_pos > 0 && chars[start_pos].is_whitespace() {
            start_pos = start_pos.saturating_sub(1);
        }

        // Skip word backwards
        while start_pos > 0
            && !chars[start_pos].is_whitespace()
            && chars[start_pos] != '.'
            && chars[start_pos] != ','
        {
            start_pos = start_pos.saturating_sub(1);
        }

        // If we stopped on whitespace or punctuation, move forward one
        if start_pos > 0
            && (chars[start_pos].is_whitespace()
                || chars[start_pos] == '.'
                || chars[start_pos] == ',')
        {
            start_pos += 1;
        }

        // Remove the text
        let before: String = chars[..start_pos].iter().collect();
        let after: String = chars[end_pos..].iter().collect();
        self.lines[self.cursor_line] = format!("{}{}", before, after);
        self.cursor_col = start_pos;
    }
}
