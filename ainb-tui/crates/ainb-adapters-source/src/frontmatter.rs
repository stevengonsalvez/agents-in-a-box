//! YAML-frontmatter parser for `SKILL.md` / agent / command files.
//!
//! Looks for a leading `---\n<yaml>\n---\n` block at the very top of
//! the file. Returns `(parsed_yaml, body_offset)` so callers can read
//! the rest of the document untouched.

use serde_yaml_ng::Value;

/// Parse the frontmatter at the top of `content`. Returns the YAML
/// value (always a mapping if present) and the byte offset where the
/// document body starts. If no frontmatter is found, returns
/// `(Value::Null, 0)`.
pub fn parse(content: &str) -> (Value, usize) {
    let trimmed = content.trim_start_matches('\u{feff}'); // strip BOM
    if !trimmed.starts_with("---") {
        return (Value::Null, 0);
    }
    // Skip the opening fence line ("---" optionally followed by EOL).
    let after_open = match trimmed.find('\n') {
        Some(i) => i + 1,
        None => return (Value::Null, 0),
    };
    // Find closing fence: a line that is exactly `---` (or `---\n`).
    let rest = &trimmed[after_open..];
    let close_rel = match find_closing_fence(rest) {
        Some(i) => i,
        None => return (Value::Null, 0),
    };

    let yaml_str = &rest[..close_rel];
    let body_start = after_open + close_rel + close_fence_len(rest, close_rel);
    let offset = (trimmed.as_ptr() as usize - content.as_ptr() as usize) + body_start;

    match serde_yaml_ng::from_str::<Value>(yaml_str) {
        Ok(v) => (v, offset),
        Err(_) => (Value::Null, offset),
    }
}

fn find_closing_fence(s: &str) -> Option<usize> {
    let mut pos = 0;
    while pos < s.len() {
        let line_end = s[pos..].find('\n').map(|i| pos + i).unwrap_or(s.len());
        let line = &s[pos..line_end];
        if line.trim_end() == "---" {
            return Some(pos);
        }
        pos = line_end + 1;
    }
    None
}

fn close_fence_len(s: &str, fence_start: usize) -> usize {
    let after = &s[fence_start..];
    if let Some(nl) = after.find('\n') {
        nl + 1
    } else {
        after.len()
    }
}

/// Pull a string field out of a frontmatter mapping.
pub fn str_field<'a>(meta: &'a Value, key: &str) -> Option<&'a str> {
    meta.as_mapping()?.get(Value::String(key.to_string()))?.as_str()
}

/// Pull a list-of-strings field out of a frontmatter mapping.
pub fn str_list_field(meta: &Value, key: &str) -> Vec<String> {
    meta.as_mapping()
        .and_then(|m| m.get(Value::String(key.to_string())))
        .and_then(|v| v.as_sequence())
        .map(|seq| seq.iter().filter_map(|item| item.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_frontmatter() {
        let doc = "---\nname: foo\ndescription: hello\n---\nbody here\n";
        let (meta, off) = parse(doc);
        assert_eq!(str_field(&meta, "name"), Some("foo"));
        assert_eq!(str_field(&meta, "description"), Some("hello"));
        assert_eq!(&doc[off..], "body here\n");
    }

    #[test]
    fn parses_tags_list() {
        let doc = "---\nname: x\ntags:\n  - a\n  - b\n---\n";
        let (meta, _) = parse(doc);
        assert_eq!(str_list_field(&meta, "tags"), vec!["a", "b"]);
    }

    #[test]
    fn no_frontmatter_returns_null() {
        let (meta, off) = parse("# just markdown\n");
        assert!(meta.is_null());
        assert_eq!(off, 0);
    }

    #[test]
    fn unterminated_fence_returns_null() {
        let (meta, off) = parse("---\nname: foo\n# no closing fence");
        assert!(meta.is_null());
        assert_eq!(off, 0);
    }

    #[test]
    fn handles_bom() {
        let doc = "\u{feff}---\nname: x\n---\nbody\n";
        let (meta, _) = parse(doc);
        assert_eq!(str_field(&meta, "name"), Some("x"));
    }
}
