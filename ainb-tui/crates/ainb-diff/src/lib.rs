//! ainb-diff — file diff computation + pager driver.
//!
//! Renders one or more [`InstallPlan`]s into a unified diff suitable
//! for the diff-and-confirm UX (spec §8.5). Production callers pipe
//! the rendered text through `$PAGER` (default `less -R`); tests pass
//! it back as a `String` for substring assertions.

use std::io::Write;
use std::process::{Command, Stdio};

use ainb_adapters_tool::plan::{InstallPlan, PlanOp};
use anyhow::{Context, Result};
use similar::TextDiff;

/// Render a single plan's diff as a string.
pub fn render_plan(plan: &InstallPlan) -> String {
    let mut out = String::new();
    if plan.is_empty() {
        out.push_str(&format!(
            "# {} {} — no changes (already up to date)\n",
            plan.tool, plan.unit_uri
        ));
        return out;
    }
    out.push_str(&format!(
        "# {tool} ← {uri} ({kind})\n",
        tool = plan.tool,
        uri = plan.unit_uri,
        kind = plan.kind
    ));
    for op in &plan.ops {
        out.push_str(&render_op(op));
    }
    out
}

/// Render a list of plans concatenated, with a single trailing
/// summary line (`N files changed across M plans`).
pub fn render_plans(plans: &[InstallPlan]) -> String {
    let mut out = String::new();
    let mut total_ops = 0usize;
    for plan in plans {
        out.push_str(&render_plan(plan));
        out.push('\n');
        total_ops += plan.ops.len();
    }
    out.push_str(&format!(
        "# summary: {total_ops} file op(s) across {} plan(s)\n",
        plans.len()
    ));
    out
}

fn render_op(op: &PlanOp) -> String {
    let mut s = String::new();
    let dst_display = op.destination().display().to_string();
    match op {
        PlanOp::Create { contents, .. } => {
            s.push_str(&format!(
                "+++ {} (new file, {} bytes)\n",
                dst_display,
                contents.len()
            ));
            s.push_str(&render_unified(b"", contents));
        }
        PlanOp::Update {
            previous, contents, ..
        } => {
            s.push_str(&format!("--- {dst_display} (before)\n"));
            s.push_str(&format!("+++ {dst_display} (after)\n"));
            s.push_str(&render_unified(previous, contents));
        }
        PlanOp::Delete { .. } => {
            s.push_str(&format!("--- {dst_display} (delete)\n"));
        }
    }
    s
}

/// Lightweight unified diff. Binary content is rendered as a
/// byte-count line rather than a sea of `<U+FFFD>` markers.
fn render_unified(prev: &[u8], next: &[u8]) -> String {
    if !is_text(prev) || !is_text(next) {
        return format!("(binary content, {} bytes)\n", next.len());
    }
    let prev_s = String::from_utf8_lossy(prev);
    let next_s = String::from_utf8_lossy(next);
    let diff = TextDiff::from_lines(prev_s.as_ref(), next_s.as_ref());
    let mut s = String::new();
    for change in diff.iter_all_changes() {
        let prefix = match change.tag() {
            similar::ChangeTag::Equal => " ",
            similar::ChangeTag::Delete => "-",
            similar::ChangeTag::Insert => "+",
        };
        s.push_str(prefix);
        s.push_str(change.value());
    }
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn is_text(bytes: &[u8]) -> bool {
    // Heuristic: any NUL byte → binary.
    !bytes.contains(&0)
}

/// Pipe `text` through `$PAGER` (defaults to `less -R`). If the env
/// variable resolves to something unrunnable, write the text to
/// stdout instead.
pub fn page(text: &str) -> Result<()> {
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".to_string());
    // Split on whitespace so users can set `PAGER="less -R"`.
    let mut parts = pager.split_whitespace();
    let Some(prog) = parts.next() else {
        println!("{text}");
        return Ok(());
    };
    let mut cmd = Command::new(prog);
    cmd.args(parts);
    if prog == "less" {
        // Append -R so colour escapes survive when callers wrap diff
        // output in ANSI. Harmless if already set.
        cmd.arg("-R");
    }
    cmd.stdin(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            println!("{text}");
            return Ok(());
        }
    };
    {
        let stdin =
            child.stdin.as_mut().ok_or_else(|| anyhow::anyhow!("pager stdin unavailable"))?;
        stdin.write_all(text.as_bytes()).context("write to pager")?;
    }
    child.wait().context("wait for pager")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_adapters_tool::plan::PlanOp;
    use ainb_skill_core::UnitKind;
    use std::path::PathBuf;

    fn make_plan(ops: Vec<PlanOp>) -> InstallPlan {
        InstallPlan {
            tool: "claude".into(),
            unit_uri: "gh:o/r@m/skills/foo".into(),
            kind: UnitKind::Skill,
            unit_install_path: PathBuf::from("/tmp/claude/skills/foo"),
            ops,
        }
    }

    #[test]
    fn empty_plan_renders_no_changes() {
        let plan = make_plan(vec![]);
        let s = render_plan(&plan);
        assert!(s.contains("no changes"), "got: {s}");
    }

    #[test]
    fn create_renders_new_file_marker() {
        let plan = make_plan(vec![PlanOp::Create {
            dst: PathBuf::from("/x/SKILL.md"),
            contents: b"hello\nworld\n".to_vec(),
        }]);
        let s = render_plan(&plan);
        assert!(s.contains("+++ /x/SKILL.md"), "got: {s}");
        assert!(s.contains("+hello\n+world\n"), "got: {s}");
    }

    #[test]
    fn update_renders_diff_with_minus_and_plus() {
        let plan = make_plan(vec![PlanOp::Update {
            dst: PathBuf::from("/x/SKILL.md"),
            previous: b"a\nb\n".to_vec(),
            contents: b"a\nc\n".to_vec(),
        }]);
        let s = render_plan(&plan);
        assert!(s.contains("-b\n"), "got: {s}");
        assert!(s.contains("+c\n"), "got: {s}");
    }

    #[test]
    fn delete_renders_marker() {
        let plan = make_plan(vec![PlanOp::Delete {
            dst: PathBuf::from("/x/SKILL.md"),
        }]);
        let s = render_plan(&plan);
        assert!(s.contains("--- /x/SKILL.md (delete)"), "got: {s}");
    }

    #[test]
    fn binary_content_is_byte_counted() {
        let plan = make_plan(vec![PlanOp::Create {
            dst: PathBuf::from("/x/raw.bin"),
            contents: vec![0u8, 1, 2, 3],
        }]);
        let s = render_plan(&plan);
        assert!(s.contains("(binary content, 4 bytes)"), "got: {s}");
    }

    #[test]
    fn render_plans_emits_summary() {
        let a = make_plan(vec![PlanOp::Create {
            dst: PathBuf::from("/x/a.md"),
            contents: b"x".to_vec(),
        }]);
        let b = make_plan(vec![PlanOp::Delete {
            dst: PathBuf::from("/y/b.md"),
        }]);
        let s = render_plans(&[a, b]);
        assert!(
            s.contains("summary: 2 file op(s) across 2 plan(s)"),
            "got: {s}"
        );
    }

    #[test]
    fn page_with_unrunnable_pager_falls_back_to_stdout() {
        std::env::set_var("PAGER", "/definitely/not/a/real/pager-9f7c1e3");
        let r = page("hello");
        std::env::remove_var("PAGER");
        assert!(r.is_ok(), "page should fall back, got: {:?}", r);
    }
}
