// ABOUTME: Canonical docsite (GitHub Pages) links surfaced in the TUI/CLI so
// users can jump from a tool to the page that shows what it does. Rendered as
// plain full URLs — modern terminals auto-linkify them (Cmd/Ctrl-click) and the
// text stays mouse-selectable; never truncate them.

/// Docsite root.
pub const SITE: &str = "https://stevengonsalvez.github.io/agents-in-a-box/";
/// OpenTelemetry → Grafana Cloud guide (with example dashboards).
pub const OTEL: &str = "https://stevengonsalvez.github.io/agents-in-a-box/reference/otel-grafana/";
/// witr process-causality plugin.
pub const WITR: &str = "https://stevengonsalvez.github.io/agents-in-a-box/plugins/witr/";
/// abtop agent-process monitor plugin.
pub const ABTOP: &str = "https://stevengonsalvez.github.io/agents-in-a-box/plugins/abtop/";
/// burndown usage/cost analytics plugin.
pub const BURNDOWN: &str = "https://stevengonsalvez.github.io/agents-in-a-box/plugins/burndown/";
/// Token-optimization tools (rtk + headroom).
pub const TOKEN_OPT: &str =
    "https://stevengonsalvez.github.io/agents-in-a-box/tui/token-optimization/";
/// reflect long-term memory.
pub const REFLECT: &str = "https://stevengonsalvez.github.io/agents-in-a-box/knowledge/overview/";
/// ainb-toolkit (skills + agents).
pub const TOOLKIT: &str = "https://stevengonsalvez.github.io/agents-in-a-box/toolkit/overview/";

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
