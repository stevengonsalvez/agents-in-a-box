use vt100::{Parser, Screen};

pub struct ScreenCapture {
    parser: Parser,
}

impl ScreenCapture {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: Parser::new(rows, cols, 0),
        }
    }

    pub fn process_output(&mut self, output: &[u8]) {
        self.parser.process(output);
    }

    pub fn screen(&self) -> &Screen {
        self.parser.screen()
    }

    pub fn contents(&self) -> String {
        self.screen().contents()
    }

    pub fn cell_at(&self, row: u16, col: u16) -> vt100::Cell {
        self.screen().cell(row, col).unwrap().clone()
    }

    pub fn cursor_position(&self) -> (u16, u16) {
        self.screen().cursor_position()
    }

    pub fn has_text(&self, text: &str) -> bool {
        self.contents().contains(text)
    }

    pub fn assert_text_at(&self, row: u16, col: u16, expected: &str) {
        let actual = self.row_text(row);
        assert!(
            actual.contains(expected),
            "Expected '{}' at row {}, but got: '{}'",
            expected, row, actual
        );
    }

    fn row_text(&self, row: u16) -> String {
        (0..self.parser.screen().size().1)
            .map(|col| self.cell_at(row, col).contents())
            .collect()
    }
}
