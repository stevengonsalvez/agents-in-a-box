// ABOUTME: OpenTelemetry / Grafana Cloud setup — shared logic for the
// `ainb otel` CLI and the TUI onboarding "OpenTelemetry" step.
//
// Pipeline this wires up:
//   Claude Code --OTLP http://localhost:4318--> Grafana Alloy --> Grafana Cloud
//
// Split of where things live (the ONE rule that keeps secrets out of synced
// files): ~/.claude/settings.json carries ONLY the generic, non-secret OTEL
// env (exporter/protocol/intervals/log flags) — that file is synced back to a
// PUBLIC repo, so nothing machine-specific or secret may land there. Everything
// machine-specific (the local OTLP endpoint, the host.name resource attr) and
// every secret (the Grafana Cloud creds) lives in
// ~/.agents-in-a-box/otel/grafana-cloud.env (0600), which is sourced from the
// user's shell rc and read directly by start-alloy.sh. Shell env wins over the
// settings.json env block, so the shell file is authoritative per-machine.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

// ── Embedded assets (vendored from the hand-built setup) ────────────────────

/// Grafana Alloy OTLP fan-in pipeline (Delta->Cumulative + Basic auth export).
pub const ASSET_CONFIG_ALLOY: &str = include_str!("../../assets/otel/config.alloy");
/// tmux launcher for Alloy. Reads grafana-cloud.env + config.alloy beside it.
pub const ASSET_START_ALLOY: &str = include_str!("../../assets/otel/start-alloy.sh");
/// grafana-cloud.env template with `__PLACEHOLDER__` tokens.
pub const ASSET_ENV_TEMPLATE: &str = include_str!("../../assets/otel/grafana-cloud.env.template");
/// Grafana dashboard JSON for Claude Code metrics (import into Grafana Cloud).
pub const ASSET_DASH_CLAUDE: &str = include_str!("../../assets/otel/dashboards/claude-code.json");
/// Grafana dashboard JSON for Codex metrics.
pub const ASSET_DASH_CODEX: &str = include_str!("../../assets/otel/dashboards/codex.json");

/// Generic, non-secret OTEL env merged into ~/.claude/settings.json. Safe to
/// sync to a public repo — no endpoint, host, or credential lives here.
pub const SETTINGS_ENV: &[(&str, &str)] = &[
    ("CLAUDE_CODE_ENABLE_TELEMETRY", "1"),
    ("CLAUDE_CODE_ENHANCED_TELEMETRY_BETA", "1"),
    ("OTEL_METRICS_EXPORTER", "otlp"),
    ("OTEL_LOGS_EXPORTER", "otlp"),
    ("OTEL_TRACES_EXPORTER", "otlp"),
    ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
    (
        "OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE",
        "cumulative",
    ),
    ("OTEL_METRIC_EXPORT_INTERVAL", "10000"),
    ("OTEL_LOGS_EXPORT_INTERVAL", "5000"),
    ("OTEL_TRACES_EXPORT_INTERVAL", "5000"),
    ("OTEL_LOG_USER_PROMPTS", "1"),
    ("OTEL_LOG_TOOL_DETAILS", "1"),
    ("OTEL_LOG_TOOL_CONTENT", "1"),
    ("OTEL_METRICS_INCLUDE_VERSION", "1"),
    ("OTEL_METRICS_INCLUDE_ENTRYPOINT", "1"),
];

/// Local OTLP endpoint every machine's Claude Code points at (the local Alloy).
pub const LOCAL_OTLP_ENDPOINT: &str = "http://localhost:4318";
/// Homebrew formula for Grafana Alloy.
pub const ALLOY_BREW_FORMULA: &str = "grafana/grafana/alloy";
/// tmux session name the launcher uses.
pub const ALLOY_TMUX_SESSION: &str = "otel-alloy";
/// Alloy local UI / health endpoint.
pub const ALLOY_HEALTH_URL: &str = "http://127.0.0.1:12345";

/// Telemetry provider. Only Grafana Cloud today; the enum reserves room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtelProvider {
    GrafanaCloud,
}

/// Grafana Cloud OTLP credentials (the three values from the OTLP page).
#[derive(Debug, Clone)]
pub struct GrafanaCloudCreds {
    /// OTLP endpoint URL (ends in `/otlp`).
    pub otlp_endpoint: String,
    /// Instance ID — the Basic-auth username on the OTLP page.
    pub instance_id: String,
    /// API/access token with metrics+logs+traces write scope.
    pub api_token: String,
}

impl GrafanaCloudCreds {
    /// True when all three fields are non-empty (after trim).
    pub fn is_complete(&self) -> bool {
        !self.otlp_endpoint.trim().is_empty()
            && !self.instance_id.trim().is_empty()
            && !self.api_token.trim().is_empty()
    }
}

// ── Paths ───────────────────────────────────────────────────────────────────

/// `~/.agents-in-a-box`.
pub fn base_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("could not determine home directory")?
        .join(".agents-in-a-box"))
}

/// `~/.agents-in-a-box/otel`.
pub fn otel_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("otel"))
}

/// `~/.agents-in-a-box/otel/grafana-cloud.env`.
pub fn env_file_path() -> Result<PathBuf> {
    Ok(otel_dir()?.join("grafana-cloud.env"))
}

/// `~/.agents-in-a-box/otel/config.alloy`.
pub fn config_path() -> Result<PathBuf> {
    Ok(otel_dir()?.join("config.alloy"))
}

/// `~/.agents-in-a-box/otel/start-alloy.sh`.
pub fn start_script_path() -> Result<PathBuf> {
    Ok(otel_dir()?.join("start-alloy.sh"))
}

/// `~/.claude/settings.json`.
pub fn claude_settings_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("could not determine home directory")?
        .join(".claude/settings.json"))
}

/// Short machine hostname (`hostname -s`), falling back to `localhost`.
pub fn detect_host_name() -> String {
    Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

// ── Asset writing ─────────────────────────────────────────────────────────

/// Write the generic (non-secret) assets into `~/.agents-in-a-box/otel/`:
/// config.alloy, start-alloy.sh (executable), and the two dashboards. Safe to
/// re-run; overwrites with the embedded canonical copy.
pub fn write_assets() -> Result<()> {
    let dir = otel_dir()?;
    let dash_dir = dir.join("dashboards");
    fs::create_dir_all(&dash_dir).with_context(|| format!("creating {}", dash_dir.display()))?;

    fs::write(config_path()?, ASSET_CONFIG_ALLOY).context("writing config.alloy")?;

    let script = start_script_path()?;
    fs::write(&script, ASSET_START_ALLOY).context("writing start-alloy.sh")?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
        .context("chmod start-alloy.sh")?;

    fs::write(dash_dir.join("claude-code.json"), ASSET_DASH_CLAUDE)
        .context("writing claude-code dashboard")?;
    fs::write(dash_dir.join("codex.json"), ASSET_DASH_CODEX).context("writing codex dashboard")?;
    Ok(())
}

/// Render the env template with creds + host and write it to grafana-cloud.env
/// with 0600 perms. Returns the path written.
pub fn write_env_file(creds: &GrafanaCloudCreds, host_name: &str) -> Result<PathBuf> {
    let dir = otel_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let rendered = ASSET_ENV_TEMPLATE
        .replace("__HOST_NAME__", host_name.trim())
        .replace("__GRAFANA_OTLP_ENDPOINT__", creds.otlp_endpoint.trim())
        .replace("__GRAFANA_INSTANCE_ID__", creds.instance_id.trim())
        .replace("__GRAFANA_API_TOKEN__", creds.api_token.trim());

    let path = env_file_path()?;
    // Create with 0600 from the start so the token is never briefly world-readable.
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.write_all(rendered.as_bytes()).context("writing grafana-cloud.env")?;
    // Re-assert perms in case the file pre-existed with looser bits.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .context("chmod grafana-cloud.env")?;
    Ok(path)
}

/// Merge the generic (non-secret) OTEL keys into ~/.claude/settings.json's
/// `env` block. Adds only missing keys (never clobbers a user override), and
/// only rewrites the file when something actually changed. Returns the keys
/// that were added.
pub fn ensure_settings_env() -> Result<Vec<String>> {
    let path = claude_settings_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    let mut root: serde_json::Value = if path.exists() {
        let raw =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        if raw.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&raw)
                .with_context(|| format!("parsing {} as JSON", path.display()))?
        }
    } else {
        serde_json::json!({})
    };

    if !root.is_object() {
        anyhow::bail!("{} is not a JSON object", path.display());
    }

    let env = root
        .as_object_mut()
        .unwrap()
        .entry("env")
        .or_insert_with(|| serde_json::json!({}));
    let env = env
        .as_object_mut()
        .context("the `env` field in settings.json is not an object")?;

    let mut added = Vec::new();
    for (k, v) in SETTINGS_ENV {
        if !env.contains_key(*k) {
            env.insert(
                (*k).to_string(),
                serde_json::Value::String((*v).to_string()),
            );
            added.push((*k).to_string());
        }
    }

    if !added.is_empty() {
        let mut out = serde_json::to_string_pretty(&root).context("serializing settings.json")?;
        out.push('\n');
        fs::write(&path, out).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(added)
}

// ── Shell rc wiring ─────────────────────────────────────────────────────────

/// Marker line that precedes the source line in the user's rc — used to detect
/// (idempotency) and to label the block.
const RC_MARKER: &str =
    "# agents-in-a-box: Claude Code OTEL env (machine-specific + Grafana Cloud creds)";

/// Pick the shell rc to wire based on `$SHELL`.
pub fn shell_rc_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let shell = std::env::var("SHELL").unwrap_or_default();
    let file = if shell.ends_with("zsh") {
        ".zshrc"
    } else if shell.ends_with("bash") {
        ".bashrc"
    } else {
        ".profile"
    };
    Ok(home.join(file))
}

/// Ensure the user's shell rc sources grafana-cloud.env. Idempotent: a no-op if
/// the marker is already present. Backs up the rc to `<rc>.bak` before the first
/// append. Returns `true` if it appended (rc was modified).
pub fn ensure_shell_rc_sources_env() -> Result<bool> {
    let rc = shell_rc_path()?;
    let env_file = env_file_path()?;

    let existing = fs::read_to_string(&rc).unwrap_or_default();
    if existing.contains(RC_MARKER) || existing.contains("otel/grafana-cloud.env") {
        return Ok(false);
    }

    if rc.exists() {
        let _ = fs::copy(&rc, rc.with_extension("bak"));
    }

    let block = format!(
        "\n{RC_MARKER}\n[ -f \"{ef}\" ] && source \"{ef}\"\n",
        ef = env_file.display()
    );
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rc)
        .with_context(|| format!("opening {}", rc.display()))?;
    f.write_all(block.as_bytes())
        .with_context(|| format!("appending to {}", rc.display()))?;
    Ok(true)
}

// ── Alloy lifecycle ─────────────────────────────────────────────────────────

/// Is the `alloy` binary on PATH?
pub fn alloy_installed() -> bool {
    which::which("alloy").is_ok()
}

/// Best-effort `brew install grafana/grafana/alloy`. Inherits stdio so the user
/// sees brew's progress. Errors if brew is missing or the install fails.
pub fn brew_install_alloy() -> Result<()> {
    if which::which("brew").is_err() {
        anyhow::bail!(
            "Homebrew not found — install alloy yourself: brew install {ALLOY_BREW_FORMULA}"
        );
    }
    let status = Command::new("brew")
        .args(["install", ALLOY_BREW_FORMULA])
        .status()
        .context("running brew install")?;
    if !status.success() {
        anyhow::bail!("brew install {ALLOY_BREW_FORMULA} failed");
    }
    Ok(())
}

/// Is the Alloy tmux session running?
pub fn alloy_session_running() -> bool {
    // `tmux has-session` writes "can't find session" to stderr on a miss —
    // silence it; the exit status is the signal we care about.
    Command::new("tmux")
        .args(["has-session", "-t", ALLOY_TMUX_SESSION])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run start-alloy.sh (launches Alloy in its own tmux session). Inherits stdio.
pub fn start_alloy() -> Result<()> {
    let script = start_script_path()?;
    if !script.exists() {
        anyhow::bail!("{} missing — run write_assets() first", script.display());
    }
    let status = Command::new("bash")
        .arg(&script)
        .status()
        .with_context(|| format!("running {}", script.display()))?;
    if !status.success() {
        anyhow::bail!("start-alloy.sh exited with failure");
    }
    Ok(())
}

/// A snapshot of the OTEL pipeline's local state, for `ainb otel status` /
/// `ainb doctor`.
#[derive(Debug, Clone)]
pub struct OtelStatus {
    pub env_file_present: bool,
    pub config_present: bool,
    pub alloy_installed: bool,
    pub alloy_running: bool,
    pub settings_env_present: bool,
}

/// Probe local OTEL state without mutating anything.
pub fn status() -> OtelStatus {
    let env_file_present = env_file_path().map(|p| p.exists()).unwrap_or(false);
    let config_present = config_path().map(|p| p.exists()).unwrap_or(false);
    let settings_env_present = claude_settings_path()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| {
            v.get("env").and_then(|e| e.get("CLAUDE_CODE_ENABLE_TELEMETRY")).map(|_| true)
        })
        .unwrap_or(false);
    OtelStatus {
        env_file_present,
        config_present,
        alloy_installed: alloy_installed(),
        alloy_running: alloy_session_running(),
        settings_env_present,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_env_has_no_secret_or_machine_keys() {
        // The settings.json set must stay public-repo-safe: no endpoint, host,
        // or credential keys.
        for (k, v) in SETTINGS_ENV {
            assert!(
                !k.contains("ENDPOINT"),
                "endpoint key leaked into settings env: {k}"
            );
            assert!(!k.contains("RESOURCE_ATTRIBUTES"), "host attr leaked: {k}");
            assert!(!k.contains("GRAFANA"), "grafana cred leaked: {k}");
            assert!(!v.contains("grafana.net"), "endpoint value leaked: {v}");
        }
    }

    #[test]
    fn env_template_has_all_placeholders() {
        for token in [
            "__HOST_NAME__",
            "__GRAFANA_OTLP_ENDPOINT__",
            "__GRAFANA_INSTANCE_ID__",
            "__GRAFANA_API_TOKEN__",
        ] {
            assert!(
                ASSET_ENV_TEMPLATE.contains(token),
                "template missing {token}"
            );
        }
    }

    #[test]
    fn render_replaces_every_placeholder() {
        let creds = GrafanaCloudCreds {
            otlp_endpoint: "https://otlp-gateway-test.grafana.net/otlp".to_string(),
            instance_id: "123456".to_string(),
            api_token: "glc_secret".to_string(),
        };
        let rendered = ASSET_ENV_TEMPLATE
            .replace("__HOST_NAME__", "my-host")
            .replace("__GRAFANA_OTLP_ENDPOINT__", &creds.otlp_endpoint)
            .replace("__GRAFANA_INSTANCE_ID__", &creds.instance_id)
            .replace("__GRAFANA_API_TOKEN__", &creds.api_token);
        assert!(
            !rendered.contains("__"),
            "unresolved placeholder remains:\n{rendered}"
        );
        assert!(rendered.contains("host.name=my-host"));
        assert!(rendered.contains("glc_secret"));
    }

    #[test]
    fn creds_completeness() {
        let mut c = GrafanaCloudCreds {
            otlp_endpoint: " ".to_string(),
            instance_id: "x".to_string(),
            api_token: "y".to_string(),
        };
        assert!(!c.is_complete());
        c.otlp_endpoint = "https://e/otlp".to_string();
        assert!(c.is_complete());
    }

    #[test]
    fn config_alloy_asset_is_the_fan_in_pipeline() {
        assert!(ASSET_CONFIG_ALLOY.contains("otelcol.receiver.otlp"));
        assert!(ASSET_CONFIG_ALLOY.contains("deltatocumulative"));
    }
}
