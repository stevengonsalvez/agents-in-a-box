//! One `Emu` implementation per candidate VT crate.

use crate::canon::{Canon, Cell};

pub trait Emu {
    fn feed(&mut self, bytes: &[u8]);
    fn canon(&self) -> Canon;
}

pub fn make(name: &str, cols: usize, rows: usize, scrollback: usize) -> Box<dyn Emu> {
    match name {
        "vt100" => Box::new(Vt100Emu::new(cols, rows, scrollback)),
        "alacritty" => Box::new(AlacrittyEmu::new(cols, rows, scrollback)),
        "wezterm" => Box::new(WeztermEmu::new(cols, rows, scrollback, 9)),
        // Same crate, told to use Unicode 14 widths, which is what tmux 3.4 and
        // a modern xterm assume for emoji presentation sequences.
        "wezterm14" => Box::new(WeztermEmu::new(cols, rows, scrollback, 14)),
        other => panic!("unknown emulator backend {other}"),
    }
}



// ---------------------------------------------------------------- vt100 ----

/// vt100 0.16 keeps no window title on the screen; it only surfaces OSC 0/2
/// through this callback, so the harness stores it alongside the parser.
#[derive(Default)]
struct Vt100Cb {
    title: std::rc::Rc<std::cell::RefCell<String>>,
}

impl vt100::Callbacks for Vt100Cb {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        *self.title.borrow_mut() = String::from_utf8_lossy(title).into_owned();
    }
}

pub struct Vt100Emu {
    parser: vt100::Parser<Vt100Cb>,
    title: std::rc::Rc<std::cell::RefCell<String>>,
}

impl Vt100Emu {
    pub fn new(cols: usize, rows: usize, scrollback: usize) -> Self {
        let title = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let cb = Vt100Cb { title: title.clone() };
        Self {
            parser: vt100::Parser::new_with_callbacks(rows as u16, cols as u16, scrollback, cb),
            title,
        }
    }
}

fn vt100_color(c: vt100::Color) -> String {
    match c {
        vt100::Color::Default => "-".into(),
        vt100::Color::Idx(i) => format!("i{i}"),
        vt100::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

impl Emu for Vt100Emu {
    fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    fn canon(&self) -> Canon {
        let s = self.parser.screen();
        let (rows, cols) = s.size();
        let mut grid = Vec::with_capacity(rows as usize);
        for r in 0..rows {
            let mut row = Vec::with_capacity(cols as usize);
            for c in 0..cols {
                let cell = s.cell(r, c).expect("cell in range");
                let mut attrs = Vec::new();
                if cell.bold() {
                    attrs.push("b");
                }
                if cell.italic() {
                    attrs.push("i");
                }
                if cell.underline() {
                    attrs.push("u");
                }
                if cell.inverse() {
                    attrs.push("rev");
                }
                row.push(Cell {
                    text: cell.contents().to_string(),
                    width: if cell.is_wide() { 2 } else { 1 },
                    spacer: cell.is_wide_continuation(),
                    fg: vt100_color(cell.fgcolor()),
                    bg: vt100_color(cell.bgcolor()),
                    attrs: attrs.join(","),
                    // vt100 0.16 has no OSC 8 model.
                    link: String::new(),
                });
            }
            grid.push(row);
        }
        let (cr, cc) = s.cursor_position();
        Canon {
            cols: cols as usize,
            rows: rows as usize,
            alt: s.alternate_screen(),
            cursor_row: cr as usize,
            cursor_col: cc as usize,
            cursor_visible: !s.hide_cursor(),
            title: self.title.borrow().clone(),
            mouse: format!("{:?}", s.mouse_protocol_mode()),
            grid,
        }
    }
}

// ----------------------------------------------------------- alacritty ----

use alacritty_terminal::event::{Event as AEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AColor, NamedColor, Processor};

struct Dims {
    cols: usize,
    lines: usize,
    total: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.total
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// alacritty_terminal keeps the title behind a private field and only reports
/// changes through the event listener, so the harness records them here.
#[derive(Clone, Default)]
struct TitleSink(std::sync::Arc<std::sync::Mutex<String>>);

impl EventListener for TitleSink {
    fn send_event(&self, event: AEvent) {
        if let AEvent::Title(t) = event {
            *self.0.lock().unwrap() = t;
        }
    }
}

pub struct AlacrittyEmu {
    term: Term<TitleSink>,
    parser: Processor,
    title: TitleSink,
}

impl AlacrittyEmu {
    pub fn new(cols: usize, rows: usize, scrollback: usize) -> Self {
        let config = Config { scrolling_history: scrollback, ..Config::default() };
        let dims = Dims { cols, lines: rows, total: rows + scrollback };
        let title = TitleSink::default();
        Self {
            term: Term::new(config, &dims, title.clone()),
            parser: Processor::new(),
            title,
        }
    }
}

fn alacritty_color(c: AColor) -> String {
    match c {
        AColor::Named(NamedColor::Foreground) | AColor::Named(NamedColor::Background) => "-".into(),
        AColor::Named(n) => format!("i{}", n as usize),
        AColor::Indexed(i) => format!("i{i}"),
        AColor::Spec(rgb) => format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b),
    }
}

impl Emu for AlacrittyEmu {
    fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    fn canon(&self) -> Canon {
        let cols = self.term.columns();
        let rows = self.term.screen_lines();
        let mode = self.term.mode();
        let mut grid = Vec::with_capacity(rows);
        for r in 0..rows {
            let mut row = Vec::with_capacity(cols);
            for c in 0..cols {
                let cell = &self.term.grid()[Point::new(Line(r as i32), Column(c))];
                let f = cell.flags;
                let mut attrs = Vec::new();
                if f.contains(Flags::BOLD) {
                    attrs.push("b");
                }
                if f.contains(Flags::ITALIC) {
                    attrs.push("i");
                }
                if f.intersects(Flags::ALL_UNDERLINES) {
                    attrs.push("u");
                }
                if f.contains(Flags::INVERSE) {
                    attrs.push("rev");
                }
                let spacer = f.contains(Flags::WIDE_CHAR_SPACER)
                    || f.contains(Flags::LEADING_WIDE_CHAR_SPACER);
                let mut text = String::new();
                if !spacer && cell.c != ' ' {
                    text.push(cell.c);
                    for z in cell.zerowidth().unwrap_or(&[]) {
                        text.push(*z);
                    }
                }
                row.push(Cell {
                    text,
                    width: if f.contains(Flags::WIDE_CHAR) { 2 } else { 1 },
                    spacer,
                    fg: alacritty_color(cell.fg),
                    bg: alacritty_color(cell.bg),
                    attrs: attrs.join(","),
                    link: cell.hyperlink().map(|h| h.uri().to_string()).unwrap_or_default(),
                });
            }
            grid.push(row);
        }
        let cursor = self.term.grid().cursor.point;
        let mouse = if mode.intersects(TermMode::MOUSE_MODE) {
            format!("{:?}", *mode & TermMode::MOUSE_MODE)
        } else {
            "None".into()
        };
        Canon {
            cols,
            rows,
            alt: mode.contains(TermMode::ALT_SCREEN),
            cursor_row: cursor.line.0.max(0) as usize,
            cursor_col: cursor.column.0,
            cursor_visible: mode.contains(TermMode::SHOW_CURSOR),
            title: self.title.0.lock().unwrap().clone(),
            mouse,
            grid,
        }
    }
}

// ------------------------------------------------------------- wezterm ----

use std::sync::Arc;
use wezterm_term::color::ColorPalette;
use wezterm_term::{Terminal, TerminalConfiguration, TerminalSize};

#[derive(Debug)]
struct WzConfig {
    scrollback: usize,
    unicode_version: u8,
}

impl TerminalConfiguration for WzConfig {
    fn scrollback_size(&self) -> usize {
        self.scrollback
    }
    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }
    fn unicode_version(&self) -> wezterm_term::UnicodeVersion {
        wezterm_term::UnicodeVersion {
            version: self.unicode_version,
            ambiguous_are_wide: false,
            cell_widths: None,
        }
    }
}

pub struct WeztermEmu {
    term: Terminal,
}

impl WeztermEmu {
    pub fn new(cols: usize, rows: usize, scrollback: usize, unicode_version: u8) -> Self {
        let size = TerminalSize { rows, cols, pixel_width: 0, pixel_height: 0, dpi: 0 };
        let term = Terminal::new(
            size,
            Arc::new(WzConfig { scrollback, unicode_version }),
            "spike2",
            "0",
            Box::new(std::io::sink()),
        );
        Self { term }
    }
}

fn wezterm_color(c: wezterm_term::color::ColorAttribute) -> String {
    use wezterm_term::color::ColorAttribute as CA;
    match c {
        CA::Default => "-".into(),
        CA::PaletteIndex(i) => format!("i{i}"),
        CA::TrueColorWithPaletteFallback(t, _) | CA::TrueColorWithDefaultFallback(t) => {
            let (r, g, b, _) = t.as_rgba_u8();
            format!("#{r:02x}{g:02x}{b:02x}")
        }
    }
}

impl Emu for WeztermEmu {
    fn feed(&mut self, bytes: &[u8]) {
        self.term.advance_bytes(bytes);
    }

    fn canon(&self) -> Canon {
        let screen = self.term.screen();
        let cols = screen.physical_cols;
        let rows = screen.physical_rows;
        let phys = screen.phys_range(&(0..rows as i64));
        let lines = screen.lines_in_phys_range(phys);
        let mut grid = Vec::with_capacity(rows);
        for line in lines.iter() {
            let mut row = vec![Cell { fg: "-".into(), bg: "-".into(), width: 1, ..Cell::default() }; cols];
            for cr in line.visible_cells() {
                let idx = cr.cell_index();
                if idx >= cols {
                    break;
                }
                let a = cr.attrs();
                let mut attrs = Vec::new();
                if a.intensity() == wezterm_term::Intensity::Bold {
                    attrs.push("b");
                }
                if a.italic() {
                    attrs.push("i");
                }
                if a.underline() != wezterm_term::Underline::None {
                    attrs.push("u");
                }
                if a.reverse() {
                    attrs.push("rev");
                }
                let s = cr.str();
                let width = cr.width().max(1) as u8;
                row[idx] = Cell {
                    text: if s == " " { String::new() } else { s.to_string() },
                    width,
                    spacer: false,
                    fg: wezterm_color(a.foreground()),
                    bg: wezterm_color(a.background()),
                    attrs: attrs.join(","),
                    link: a.hyperlink().map(|h| h.uri().to_string()).unwrap_or_default(),
                };
                if width == 2 && idx + 1 < cols {
                    row[idx + 1] = Cell {
                        text: String::new(),
                        width: 0,
                        spacer: true,
                        fg: row[idx].fg.clone(),
                        bg: row[idx].bg.clone(),
                        attrs: row[idx].attrs.clone(),
                        link: row[idx].link.clone(),
                    };
                }
            }
            grid.push(row);
        }
        let pos = self.term.cursor_pos();
        Canon {
            cols,
            rows,
            alt: self.term.is_alt_screen_active(),
            cursor_row: pos.y.max(0) as usize,
            cursor_col: pos.x,
            cursor_visible: format!("{:?}", pos.visibility) == "Visible",
            title: self.term.get_title().to_string(),
            // wezterm-term exposes mouse reporting only via its mouse encoder.
            mouse: "n/a".into(),
            grid,
        }
    }
}
