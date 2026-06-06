// ABOUTME: Dracula syntax highlighting for the Code Review surface.
// Bridges syntect (via two-face's extended grammars + Dracula theme) to ratatui
// Spans, then layers the diff row-tint background under the syntax foreground,
// with a brighter tint on word-emphasized sub-segments.

use std::sync::LazyLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SyntectStyle, Theme};
use syntect::parsing::SyntaxSet;
use two_face::theme::EmbeddedThemeName;

use super::model::RowKind;

/// Extended grammar set (TOML/TS/Dockerfile/… on top of syntect defaults).
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
/// The Dracula theme, owned so highlighters can borrow it for 'static.
static DRACULA: LazyLock<Theme> =
    LazyLock::new(|| two_face::theme::extra().get(EmbeddedThemeName::Dracula).clone());

/// Muted green background for added rows.
pub const ADD_TINT: Color = Color::Rgb(28, 55, 36);
/// Brighter green background for the word-emphasized part of an added row.
pub const ADD_TINT_BRIGHT: Color = Color::Rgb(46, 96, 56);
/// Muted red background for removed rows.
pub const DEL_TINT: Color = Color::Rgb(64, 30, 36);
/// Brighter red background for the word-emphasized part of a removed row.
pub const DEL_TINT_BRIGHT: Color = Color::Rgb(110, 42, 50);
/// Solid left-bar color on added rows (Dracula green).
pub const BAR_ADD: Color = Color::Rgb(80, 250, 123);
/// Solid left-bar color on removed rows (Dracula red).
pub const BAR_DEL: Color = Color::Rgb(255, 85, 85);
/// Muted gutter line-number color (Dracula comment gray).
pub const GUTTER_FG: Color = Color::Rgb(98, 114, 164);
/// Plain foreground (Dracula foreground) used when syntax highlighting is
/// skipped.
const PLAIN_FG: Color = Color::Rgb(248, 248, 242);
/// Lines longer than this many bytes skip syntax highlighting and word-emphasis
/// to stay responsive (minified bundles, data blobs).
const MAX_HIGHLIGHT_LEN: usize = 2000;

/// Syntax-highlight a single line into syntect `(style, text)` runs using the
/// Dracula theme. Falls back to plain text for unknown languages.
///
/// State does not carry across calls, so multi-line constructs (block comments)
/// are not tracked; this is acceptable for per-row diff rendering.
pub fn highlight_line(line: &str, language: Option<&str>) -> Vec<(SyntectStyle, String)> {
    let syntax = language
        .and_then(|lang| SYNTAXES.find_syntax_by_token(lang))
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, &DRACULA);
    // Some grammars require the trailing newline to terminate a line; add then
    // strip.
    let with_nl = format!("{line}\n");
    let ranges = highlighter.highlight_line(&with_nl, &SYNTAXES).unwrap_or_default();
    ranges
        .into_iter()
        .map(|(style, text)| (style, text.trim_end_matches('\n').to_string()))
        .filter(|(_, text)| !text.is_empty())
        .collect()
}

/// Highlight a diff row and layer the diff-tint background under the syntax
/// foreground.
///
/// The row tint (and a brighter tint over the `emphasis` byte ranges) becomes
/// the background while syntect's foreground is preserved; context rows get no
/// background.
pub fn highlight_row(
    raw: &str,
    language: Option<&str>,
    emphasis: &[(usize, usize)],
    kind: RowKind,
) -> Vec<Span<'static>> {
    // Pathologically long lines (minified bundles, data blobs) skip the syntect +
    // word-diff work and render as a single plain, row-tinted span.
    if raw.len() > MAX_HIGHLIGHT_LEN {
        return vec![make_span(
            raw.to_string(),
            PLAIN_FG,
            Modifier::empty(),
            bg_for(kind, false),
        )];
    }
    merge(&highlight_line(raw, language), emphasis, kind)
}

/// Merge syntect runs with word-emphasis ranges into background-tinted spans.
pub fn merge(
    ranges: &[(SyntectStyle, String)],
    emphasis: &[(usize, usize)],
    kind: RowKind,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut pos = 0usize; // byte offset into the full row text
    for (style, text) in ranges {
        let fg = syntect_fg(*style);
        let modifier = syntect_modifier(*style);
        let mut seg = String::new();
        let mut seg_emph: Option<bool> = None;
        for ch in text.chars() {
            let here = in_emphasis(pos, emphasis);
            match seg_emph {
                Some(prev) if prev != here => {
                    spans.push(make_span(
                        std::mem::take(&mut seg),
                        fg,
                        modifier,
                        bg_for(kind, prev),
                    ));
                    seg.push(ch);
                    seg_emph = Some(here);
                }
                _ => {
                    seg.push(ch);
                    seg_emph = Some(here);
                }
            }
            pos += ch.len_utf8();
        }
        if let Some(prev) = seg_emph {
            spans.push(make_span(seg, fg, modifier, bg_for(kind, prev)));
        }
    }
    spans
}

/// Background tint for a `(kind, emphasized)` cell, or `None` for context.
const fn bg_for(kind: RowKind, emphasized: bool) -> Option<Color> {
    match (kind, emphasized) {
        (RowKind::Added, false) => Some(ADD_TINT),
        (RowKind::Added, true) => Some(ADD_TINT_BRIGHT),
        (RowKind::Removed, false) => Some(DEL_TINT),
        (RowKind::Removed, true) => Some(DEL_TINT_BRIGHT),
        (RowKind::Context, _) => None,
    }
}

fn in_emphasis(pos: usize, emphasis: &[(usize, usize)]) -> bool {
    emphasis.iter().any(|&(s, e)| pos >= s && pos < e)
}

const fn syntect_fg(style: SyntectStyle) -> Color {
    let c = style.foreground;
    Color::Rgb(c.r, c.g, c.b)
}

fn syntect_modifier(style: SyntectStyle) -> Modifier {
    let mut m = Modifier::empty();
    if style.font_style.contains(FontStyle::BOLD) {
        m |= Modifier::BOLD;
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        m |= Modifier::ITALIC;
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        m |= Modifier::UNDERLINED;
    }
    m
}

fn make_span(text: String, fg: Color, modifier: Modifier, bg: Option<Color>) -> Span<'static> {
    let mut style = Style::default().fg(fg).add_modifier(modifier);
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    Span::styled(text, style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_keyword_differs_from_identifier() {
        let ranges = highlight_line("let x = 1;", Some("rust"));
        assert!(ranges.len() >= 2, "expected multiple syntect runs");
        let colors: std::collections::HashSet<_> = ranges
            .iter()
            .map(|(s, _)| (s.foreground.r, s.foreground.g, s.foreground.b))
            .collect();
        assert!(
            colors.len() >= 2,
            "expected distinct token colors, got {colors:?}"
        );
    }

    #[test]
    fn merge_layers_emphasis_bg_over_fg() {
        let ranges = highlight_line("abcdef", None);
        let spans = merge(&ranges, &[(0, 3)], RowKind::Added);
        // First emphasized segment is the brighter tint.
        assert_eq!(spans.first().unwrap().style.bg, Some(ADD_TINT_BRIGHT));
        // The remainder carries the muted tint.
        assert!(
            spans.iter().any(|s| s.style.bg == Some(ADD_TINT)),
            "expected a muted-tint remainder span"
        );
        // Foreground is preserved on every span (never None).
        assert!(spans.iter().all(|s| s.style.fg.is_some()));
        // Reassembled text is intact.
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcdef");
    }

    #[test]
    fn context_row_has_no_background() {
        let ranges = highlight_line("hello world", None);
        let spans = merge(&ranges, &[], RowKind::Context);
        assert!(spans.iter().all(|s| s.style.bg.is_none()));
    }

    #[test]
    fn removed_row_uses_red_tints() {
        let ranges = highlight_line("xy", None);
        let spans = merge(&ranges, &[(0, 1)], RowKind::Removed);
        assert_eq!(spans.first().unwrap().style.bg, Some(DEL_TINT_BRIGHT));
        assert!(spans.iter().any(|s| s.style.bg == Some(DEL_TINT)));
    }

    #[test]
    fn unknown_language_does_not_panic() {
        let ranges = highlight_line("@@@ not real code", Some("nonsense-lang-xyz"));
        assert!(!ranges.is_empty());
    }

    #[test]
    fn long_line_skips_highlight_and_emphasis() {
        let long = "x".repeat(MAX_HIGHLIGHT_LEN + 100);
        let spans = highlight_row(&long, Some("rust"), &[(0, 50)], RowKind::Added);
        // A single plain span: no syntax split, no brighter emphasis segment.
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style.bg, Some(ADD_TINT));
        assert!(spans.iter().all(|s| s.style.bg != Some(ADD_TINT_BRIGHT)));
    }

    #[test]
    fn long_multibyte_line_skips_highlight_without_panicking() {
        // The cap is on bytes, but a multi-byte char must never be sliced
        // mid-codepoint. 700 euro signs = 2100 bytes (3 each) >
        // MAX_HIGHLIGHT_LEN, with no ASCII boundary.
        let long = "€".repeat(700);
        assert!(long.len() > MAX_HIGHLIGHT_LEN);
        let spans = highlight_row(&long, Some("rust"), &[(0, 9)], RowKind::Removed);
        // Single plain, row-tinted span; the multibyte content survives intact.
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style.bg, Some(DEL_TINT));
        assert_eq!(spans[0].content.as_ref(), long);
    }
}
