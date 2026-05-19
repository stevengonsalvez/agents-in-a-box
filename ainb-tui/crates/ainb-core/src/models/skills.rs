// ABOUTME: Parser and data model for user-level Claude Code skills + agents.
// Walks ~/.claude/skills/*/SKILL.md and ~/.claude/agents/**/*.md, extracts YAML
// frontmatter, and builds a skill-to-agents association map for the TUI Skills screen.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// A single skill parsed from `SKILL.md`.
#[derive(Debug, Clone, Default)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub user_invocable: Option<bool>,
    pub source_path: PathBuf,
}

/// An agent definition parsed from an `.md` file under `~/.claude/agents/`.
#[derive(Debug, Clone, Default)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub source_path: PathBuf,
}

/// Scanner-internal view of an agent that caches pre-lowercased strings used
/// during association building. Kept separate from [`AgentDef`] so the public
/// type stays lean (body text is only needed for matching, never rendered).
struct ScannedAgent {
    agent: AgentDef,
    desc_lower: String,
    body_lower: String,
    tools_lower: Vec<String>,
}

/// Complete parsed skills + agents snapshot.
#[derive(Debug, Clone, Default)]
pub struct SkillsData {
    pub skills: Vec<Skill>,
    pub agents: Vec<AgentDef>,
    /// skill name -> list of agent names that reference it.
    pub associations: HashMap<String, Vec<String>>,
}

/// Parse all skills + agents installed at the user level and return a snapshot.
/// This is designed to run off the event thread (blocking I/O).
pub fn parse_skills() -> SkillsData {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            warn!("Could not determine home directory for skills scan");
            return SkillsData::default();
        }
    };

    let skills = scan_skills(&home.join(".claude").join("skills"));
    let scanned = scan_agents(&home.join(".claude").join("agents"));
    let associations = build_associations(&skills, &scanned);
    let agents: Vec<AgentDef> = scanned.into_iter().map(|s| s.agent).collect();

    debug!(
        "Parsed {} skills, {} agents, {} skill-associations",
        skills.len(),
        agents.len(),
        associations.len()
    );

    SkillsData {
        skills,
        agents,
        associations,
    }
}

fn scan_skills(root: &Path) -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    if !root.is_dir() {
        return out;
    }

    let entries = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) => {
            warn!("Cannot read skills dir {:?}: {}", root, e);
            return out;
        }
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let content = match std::fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let (fm, _body) = split_frontmatter(&content);
        if fm.is_empty() {
            continue;
        }
        let name = fm
            .get("name")
            .cloned()
            .unwrap_or_else(|| path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string());
        if name.is_empty() {
            continue;
        }
        let description = fm.get("description").cloned().unwrap_or_default();
        let user_invocable = fm.get("user-invocable").map(|s| parse_bool(s));

        out.push(Skill {
            name,
            description,
            user_invocable,
            source_path: skill_md,
        });
    }

    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn scan_agents(root: &Path) -> Vec<ScannedAgent> {
    let mut out: Vec<ScannedAgent> = Vec::new();
    if !root.is_dir() {
        return out;
    }

    let mut files: Vec<PathBuf> = Vec::new();
    collect_agent_files(root, &mut files);

    for path in files {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let (fm, body) = split_frontmatter(&content);
        if fm.is_empty() {
            // Not a frontmatter-style agent file; skip.
            continue;
        }
        let name = fm
            .get("name")
            .cloned()
            .unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string());
        if name.is_empty() {
            continue;
        }
        let description = fm.get("description").cloned().unwrap_or_default();
        let tools = parse_tools(fm.get("tools").map(String::as_str).unwrap_or(""));

        let desc_lower = description.to_lowercase();
        let body_lower = body.to_lowercase();
        let tools_lower = tools.iter().map(|t| t.to_lowercase()).collect();

        out.push(ScannedAgent {
            agent: AgentDef {
                name,
                description,
                tools,
                source_path: path,
            },
            desc_lower,
            body_lower,
            tools_lower,
        });
    }

    out.sort_by(|a, b| a.agent.name.to_lowercase().cmp(&b.agent.name.to_lowercase()));
    out
}

fn collect_agent_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if path.is_dir() {
            // Skip noisy / non-agent subdirs.
            if matches!(name.as_str(), "archived" | "swarm") {
                continue;
            }
            collect_agent_files(&path, out);
        } else if path.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            if ext == "md" {
                out.push(path);
            }
        }
    }
}

fn build_associations(skills: &[Skill], agents: &[ScannedAgent]) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for skill in skills {
        let skill_name_lower = skill.name.to_lowercase();
        let mut matches: Vec<String> = Vec::new();

        for agent in agents {
            // Tool list match: exact case-insensitive match.
            let tool_hit = agent.tools_lower.iter().any(|t| t == &skill_name_lower);

            // Text match: whole-word case-insensitive match in description or body.
            let text_hit = contains_word(&agent.desc_lower, &skill_name_lower)
                || contains_word(&agent.body_lower, &skill_name_lower);

            if tool_hit || text_hit {
                matches.push(agent.agent.name.clone());
            }
        }

        if !matches.is_empty() {
            matches.sort();
            matches.dedup();
            map.insert(skill.name.clone(), matches);
        }
    }

    map
}

/// Very small YAML-frontmatter extractor. Returns (key->value map, body).
/// Supports simple `key: value` lines and block scalars using `>` or `|`,
/// plus multi-line continuations where subsequent lines are indented.
/// Good enough for the subset of frontmatter our skills/agents use.
fn split_frontmatter(content: &str) -> (HashMap<String, String>, String) {
    let mut map: HashMap<String, String> = HashMap::new();
    // body_start defaults to 0 when the loop below never assigns (no closing
    // `---` found) — `#[allow]` because rustc's lint can't trace the
    // assign-then-read flow across the iterator-based scan.
    #[allow(unused_assignments)]
    let mut body_start = 0usize;

    let mut lines = content.lines().enumerate();
    let Some((_, first)) = lines.next() else {
        return (map, String::new());
    };

    if first.trim() != "---" {
        return (map, content.to_string());
    }

    // Collect frontmatter lines until the next `---`.
    let mut fm_lines: Vec<&str> = Vec::new();
    let mut end_idx: Option<usize> = None;
    for (idx, line) in lines {
        if line.trim() == "---" {
            end_idx = Some(idx);
            break;
        }
        fm_lines.push(line);
    }

    match end_idx {
        Some(idx) => {
            // Compute byte offset of body (line idx+1 and onward) using
            // split_inclusive so CRLF line endings are counted in full.
            let mut seen = 0usize;
            for (i, l) in content.split_inclusive('\n').enumerate() {
                if i == idx + 1 {
                    break;
                }
                seen += l.len();
            }
            body_start = seen.min(content.len());
        }
        None => {
            // Missing closing delimiter — treat whole file as frontmatter-less.
            return (HashMap::new(), content.to_string());
        }
    }

    // Parse frontmatter lines. Top-level keys live at column 0 with `key:`.
    let mut i = 0usize;
    while i < fm_lines.len() {
        let line = fm_lines[i];
        let trimmed = line.trim_end();
        // Skip blank lines or comments at top level.
        if trimmed.trim().is_empty() || trimmed.trim_start().starts_with('#') {
            i += 1;
            continue;
        }
        // Only treat as top-level key when the line starts in column 0.
        let indent = line.len() - line.trim_start().len();
        if indent != 0 {
            i += 1;
            continue;
        }
        let Some(colon) = trimmed.find(':') else {
            i += 1;
            continue;
        };
        let key = trimmed[..colon].trim().to_string();
        let raw_value = trimmed[colon + 1..].trim();

        if key.is_empty() {
            i += 1;
            continue;
        }

        // Case 1: inline value present.
        if !raw_value.is_empty() {
            // Handle block scalar folding marker on same line: `key: >` / `key: |`.
            let folded = raw_value == ">" || raw_value == "|";
            if folded {
                let mut collected = String::new();
                i += 1;
                while i < fm_lines.len() {
                    let l = fm_lines[i];
                    let li = l.len() - l.trim_start().len();
                    if li == 0 && !l.trim().is_empty() {
                        break;
                    }
                    if !collected.is_empty() {
                        collected.push(' ');
                    }
                    collected.push_str(l.trim());
                    i += 1;
                }
                map.insert(key, strip_quotes(&collected));
                continue;
            }

            // Plain value — also absorb any continuation lines that are indented
            // (so multi-line descriptions without `>` still get captured).
            let mut buf = strip_quotes(raw_value);
            i += 1;
            while i < fm_lines.len() {
                let l = fm_lines[i];
                let li = l.len() - l.trim_start().len();
                let t = l.trim();
                // Stop at the next top-level key or blank separator.
                if li == 0 {
                    break;
                }
                // Skip list entries introduced by `-` (they belong to a structured value).
                if t.starts_with('-') {
                    i += 1;
                    continue;
                }
                if !buf.is_empty() {
                    buf.push(' ');
                }
                buf.push_str(t);
                i += 1;
            }
            map.insert(key, buf);
            continue;
        }

        // Case 2: value on following lines.
        // Peek at the first non-blank indented follow-up line. Only enter
        // list-collection mode if it starts with `- ` (or bare `-`). Otherwise
        // the bare key points at a nested map / JSON block (see bitwarden's
        // `metadata:` block), so skip the whole indented region and resume
        // at the next column-0 key.
        i += 1;
        let mut first_non_blank: Option<&str> = None;
        let mut scan = i;
        while scan < fm_lines.len() {
            let l = fm_lines[scan];
            if l.trim().is_empty() {
                scan += 1;
                continue;
            }
            let li = l.len() - l.trim_start().len();
            if li == 0 {
                break;
            }
            first_non_blank = Some(l);
            break;
        }

        let is_list = first_non_blank
            .map(|l| {
                let t = l.trim_start();
                t == "-" || t.starts_with("- ")
            })
            .unwrap_or(false);

        if is_list {
            let mut collected: Vec<String> = Vec::new();
            while i < fm_lines.len() {
                let l = fm_lines[i];
                let li = l.len() - l.trim_start().len();
                if li == 0 && !l.trim().is_empty() {
                    break;
                }
                let t = l.trim();
                if t.is_empty() {
                    i += 1;
                    continue;
                }
                if let Some(item) = t.strip_prefix('-') {
                    collected.push(strip_quotes(item.trim()));
                    i += 1;
                } else {
                    // Nested descendant of a list item — don't absorb.
                    break;
                }
            }
            if !collected.is_empty() {
                map.insert(key, collected.join(", "));
            }
        } else {
            // Nested map / JSON / other block — skip all blank + indented
            // lines until the next column-0 non-blank line.
            while i < fm_lines.len() {
                let l = fm_lines[i];
                if l.trim().is_empty() {
                    i += 1;
                    continue;
                }
                let li = l.len() - l.trim_start().len();
                if li == 0 {
                    break;
                }
                i += 1;
            }
        }
    }

    let body = if body_start < content.len() {
        content[body_start..].to_string()
    } else {
        String::new()
    };

    (map, body)
}

fn strip_quotes(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
    {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

fn parse_bool(s: &str) -> bool {
    matches!(
        s.trim().to_lowercase().as_str(),
        "true" | "yes" | "on" | "1"
    )
}

fn parse_tools(s: &str) -> Vec<String> {
    if s.trim().is_empty() {
        return Vec::new();
    }
    // Quote-aware split: commas and newlines only split entries when we are
    // NOT inside a single- or double-quoted region. Handles entries like
    // `"Read,Bash(bd:*)"` where a literal comma lives inside the quotes.
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    for ch in s.chars() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                current.push(ch);
            }
            '"' if !in_single => {
                in_double = !in_double;
                current.push(ch);
            }
            ',' | '\n' if !in_single && !in_double => {
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    parts.push(current);

    parts
        .into_iter()
        .map(|t| strip_quotes(t.trim().trim_start_matches('-').trim()))
        .filter(|t| !t.is_empty())
        .collect()
}

/// Whole-word substring match. Caller must pass a pre-lowercased haystack
/// and pre-lowercased needle; this avoids redundant per-call normalization
/// when the same haystack is scanned against many needles.
fn contains_word(haystack_lower: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return false;
    }
    let needle_bytes = needle_lower.as_bytes();
    let hay_bytes = haystack_lower.as_bytes();
    let mut i = 0usize;
    while i + needle_bytes.len() <= hay_bytes.len() {
        if &hay_bytes[i..i + needle_bytes.len()] == needle_bytes {
            let before_ok = i == 0 || !is_word_char(hay_bytes[i - 1]);
            let after_idx = i + needle_bytes.len();
            let after_ok = after_idx >= hay_bytes.len() || !is_word_char(hay_bytes[after_idx]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
