// ABOUTME: Canonical docsite links surfaced in the TUI/CLI so users can jump
// from a tool to the page that shows what it does. Rendered as plain full URLs
// — modern terminals auto-linkify them (Cmd/Ctrl-click) and the text stays
// mouse-selectable; never truncate them.
//
// The origin lives in exactly one place, `doc_url!`. It used to be pasted into
// every const, which is how the old GitHub Pages URL ended up duplicated across
// this file and two plugin crates. Change it here and everything follows.

/// Build a docsite URL from a path relative to the site root.
macro_rules! doc_url {
    ($path:literal) => {
        concat!("https://ainb.app/", $path)
    };
}

/// Docsite root.
pub const SITE: &str = doc_url!("");
/// OpenTelemetry → Grafana Cloud guide (with example dashboards).
pub const OTEL: &str = doc_url!("reference/otel-grafana/");
/// witr process-causality plugin.
pub const WITR: &str = doc_url!("plugins/witr/");
/// abtop agent-process monitor plugin.
pub const ABTOP: &str = doc_url!("plugins/abtop/");
/// burndown usage/cost analytics plugin.
pub const BURNDOWN: &str = doc_url!("plugins/burndown/");
/// Token-optimization tools (rtk + headroom).
pub const TOKEN_OPT: &str = doc_url!("tui/token-optimization/");
/// reflect long-term memory.
pub const REFLECT: &str = doc_url!("knowledge/overview/");
/// ainb-toolkit (skills + agents).
pub const TOOLKIT: &str = doc_url!("toolkit/overview/");

// Official vendor auth guides, surfaced on the onboarding Authentication step.
/// Claude Code authentication guide.
pub const AUTH_CLAUDE: &str = "https://code.claude.com/docs/en/authentication";
/// OpenAI Codex CLI authentication guide.
pub const AUTH_CODEX: &str = "https://developers.openai.com/codex/auth";
/// Google Gemini CLI authentication guide.
pub const AUTH_GEMINI: &str =
    "https://google-gemini.github.io/gemini-cli/docs/get-started/authentication.html";
/// GitHub Copilot CLI authentication guide.
pub const AUTH_COPILOT: &str = "https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/authenticate-copilot-cli";

/// Docsite page for a setup-catalog dep id, if one exists. Used by the deps
/// screen to show "what you get" on the focused row.
pub fn docs_url_for(dep_id: &str) -> Option<&'static str> {
    Some(match dep_id {
        "witr" => WITR,
        "abtop" => ABTOP,
        "alloy" => OTEL,
        "rtk" | "headroom" => TOKEN_OPT,
        "reflect-kb" | "reflect-plugin" | "uv" => REFLECT,
        "toolkit" => TOOLKIT,
        _ => return None,
    })
}
