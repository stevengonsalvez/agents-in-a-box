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

/// Parse `export KEY='VALUE'` lines out of a sourced env file into a map.
/// Values are single-quoted by `write_env_file`; strip one matching pair of
/// surrounding single quotes. Lines without `export`/`=` are ignored.
fn parse_env_exports(text: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("export ") else {
            continue;
        };
        let Some((k, v)) = rest.split_once('=') else {
            continue;
        };
        let v = v.trim();
        let v = v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(v);
        out.insert(k.trim().to_string(), v.to_string());
    }
    out
}

/// Read previously-saved Grafana Cloud creds back from `grafana-cloud.env`,
/// so the onboarding OTEL form re-populates on re-open (mirrors the
/// git-directories remember-on-reopen behaviour). `None` when the file is
/// absent or missing any of the three values.
pub fn read_grafana_creds() -> Option<GrafanaCloudCreds> {
    let text = std::fs::read_to_string(env_file_path().ok()?).ok()?;
    let map = parse_env_exports(&text);
    let creds = GrafanaCloudCreds {
        otlp_endpoint: map.get("GRAFANA_OTLP_ENDPOINT").cloned().unwrap_or_default(),
        instance_id: map.get("GRAFANA_INSTANCE_ID").cloned().unwrap_or_default(),
        api_token: map.get("GRAFANA_API_TOKEN").cloned().unwrap_or_default(),
    };
    creds.is_complete().then_some(creds)
}

/// Shell `export … && ` prefix that gives a spawned agent the FULL OTEL
/// config, for injection into a non-interactive `sh -c` pane that never
/// sourced the shell rc. Empty unless OTEL is configured (creds present).
///
/// Emits the same generic `SETTINGS_ENV` block that `~/.claude/settings.json`
/// carries (exporter/protocol/enable/log flags) PLUS the machine-specific
/// endpoint + host resource attr — so telemetry actually flows for ANY
/// OTel-capable agent, not just Claude (which alone reads settings.json). The
/// secret `GRAFANA_*` creds are NOT injected — those belong to Alloy, and the
/// agent only needs to reach the local collector.
///
/// All values are static config or quote-free (endpoint URL, `host.name=<h>`),
/// so plain single-quoting is shell-safe.
pub fn session_otlp_exports() -> String {
    // Configured ⇔ the creds file exists with a token. Cheap gate; no need to
    // parse for the endpoint (it's the fixed `LOCAL_OTLP_ENDPOINT` const).
    let Ok(path) = env_file_path() else {
        return String::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let map = parse_env_exports(&text);
    if map.get("GRAFANA_API_TOKEN").map(String::is_empty).unwrap_or(true) {
        return String::new();
    }

    let mut prefix = String::new();
    for (k, v) in SETTINGS_ENV {
        prefix.push_str(&format!("export {k}='{v}' && "));
    }
    prefix.push_str(&format!(
        "export OTEL_EXPORTER_OTLP_ENDPOINT='{LOCAL_OTLP_ENDPOINT}' && "
    ));
    if let Some(attrs) = map.get("OTEL_RESOURCE_ATTRIBUTES").filter(|v| !v.is_empty()) {
        prefix.push_str(&format!("export OTEL_RESOURCE_ATTRIBUTES='{attrs}' && "));
    }
    prefix
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

/// Short machine hostname (`hostname -s`), then `$HOSTNAME`, then `localhost`.
/// The `$HOSTNAME` fallback matters on minimal Linux containers where the
/// `hostname` binary is often absent (otherwise every host reads `localhost`).
pub fn detect_host_name() -> String {
    if let Some(h) = Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return h;
    }
    if let Ok(h) = std::env::var("HOSTNAME") {
        // `$HOSTNAME` is usually the FQDN; keep just the short label.
        let short = h.split('.').next().unwrap_or(&h).trim();
        if !short.is_empty() {
            return short.to_string();
        }
    }
    "localhost".to_string()
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

/// Escape a value for safe inclusion inside a single-quoted POSIX shell string.
/// The template wraps every value in `'...'`; this returns the inner content
/// with each `'` rewritten as `'\''` (close-quote, escaped quote, reopen). The
/// rendered file is `source`d by the user's shell + start-alloy.sh, so anything
/// less leaves a shell-injection hole (a token containing `"`, `` ` ``, `$`, or
/// `'` could otherwise execute arbitrary code at every shell startup).
fn sh_squote_inner(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// Reject values that can't be safely represented on a single shell line.
fn validate_cred_value(label: &str, v: &str) -> Result<()> {
    if v.contains('\n') || v.contains('\r') {
        anyhow::bail!("{label} contains a newline — paste the value on a single line");
    }
    Ok(())
}

/// Render the env template with creds + host and write it to grafana-cloud.env
/// with 0600 perms. Returns the path written.
pub fn write_env_file(creds: &GrafanaCloudCreds, host_name: &str) -> Result<PathBuf> {
    let dir = otel_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    write_env_file_to(&env_file_path()?, creds, host_name)
}

/// `write_env_file` against an explicit path (testable; no `$HOME` dependency).
/// The caller is responsible for the parent directory existing.
pub fn write_env_file_to(
    path: &std::path::Path,
    creds: &GrafanaCloudCreds,
    host_name: &str,
) -> Result<PathBuf> {
    validate_cred_value("host.name", host_name.trim())?;
    validate_cred_value("OTLP endpoint", creds.otlp_endpoint.trim())?;
    validate_cred_value("Instance ID", creds.instance_id.trim())?;
    validate_cred_value("API token", creds.api_token.trim())?;

    let rendered = ASSET_ENV_TEMPLATE
        .replace("__HOST_NAME__", &sh_squote_inner(host_name.trim()))
        .replace(
            "__GRAFANA_OTLP_ENDPOINT__",
            &sh_squote_inner(creds.otlp_endpoint.trim()),
        )
        .replace(
            "__GRAFANA_INSTANCE_ID__",
            &sh_squote_inner(creds.instance_id.trim()),
        )
        .replace(
            "__GRAFANA_API_TOKEN__",
            &sh_squote_inner(creds.api_token.trim()),
        );

    // Create with 0600 from the start so the token is never briefly world-readable.
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.write_all(rendered.as_bytes()).context("writing grafana-cloud.env")?;
    // Re-assert perms in case the file pre-existed with looser bits.
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("chmod grafana-cloud.env")?;
    Ok(path.to_path_buf())
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
    ensure_settings_env_at(&path)
}

/// `ensure_settings_env` against an explicit path (testable; no `$HOME`).
pub fn ensure_settings_env_at(path: &std::path::Path) -> Result<Vec<String>> {
    let mut root: serde_json::Value = if path.exists() {
        let raw =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
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
        anyhow::bail!(
            "{} is not a JSON object — fix or remove it, then re-run",
            path.display()
        );
    }
    let obj = root.as_object_mut().unwrap();

    // A null `env` is treated as absent (safe to replace). A non-null,
    // non-object `env` (array/string/number/bool) is a user value we must NOT
    // clobber — bail with an actionable message instead of overwriting.
    match obj.get("env") {
        None | Some(serde_json::Value::Null) => {
            obj.insert("env".to_string(), serde_json::json!({}));
        }
        Some(v) if v.is_object() => {}
        Some(_) => anyhow::bail!(
            "the `env` field in {} is not an object — fix or remove it, then re-run",
            path.display()
        ),
    }
    let env = obj
        .get_mut("env")
        .and_then(|e| e.as_object_mut())
        .expect("env normalized to an object above");

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
        fs::write(path, out).with_context(|| format!("writing {}", path.display()))?;
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
    ensure_shell_rc_at(&shell_rc_path()?, &env_file_path()?)
}

/// `ensure_shell_rc_sources_env` against explicit paths (testable).
///
/// Note: this read-check-append is not atomic across concurrent processes —
/// two simultaneous setups could both pass the marker check and append twice.
/// That race is acceptable for a single-user, interactive onboarding tool; if
/// it ever matters, guard with an advisory file lock.
pub fn ensure_shell_rc_at(rc: &std::path::Path, env_file: &std::path::Path) -> Result<bool> {
    // NotFound -> treat as empty (we'll create it). Any other read error
    // (permission denied, rc is a directory) is real — surface it rather than
    // silently masking it as an empty file.
    let existing = match fs::read_to_string(rc) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", rc.display())),
    };
    if existing.contains(RC_MARKER) || existing.contains("otel/grafana-cloud.env") {
        return Ok(false);
    }

    if rc.exists() {
        // Build "<name>.bak" from the full file name — `with_extension` would
        // turn `.zshrc` into `.zsh.bak` (and `.profile` into `.bak`).
        if let Some(name) = rc.file_name() {
            let bak = rc.with_file_name(format!("{}.bak", name.to_string_lossy()));
            let _ = fs::copy(rc, bak);
        }
    }

    let block = format!(
        "\n{RC_MARKER}\n[ -f \"{ef}\" ] && source \"{ef}\"\n",
        ef = env_file.display()
    );
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(rc)
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

    #[test]
    fn sh_squote_neutralizes_injection() {
        // A single quote becomes '\'' so the value can't break out of '...'.
        assert_eq!(sh_squote_inner("a'b$c`x`"), "a'\\''b$c`x`");
    }

    #[test]
    fn write_env_file_is_0600_and_shell_safe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grafana-cloud.env");
        let creds = GrafanaCloudCreds {
            otlp_endpoint: "https://e/otlp".into(),
            instance_id: "12345".into(),
            // Hostile token: quote + command-substitution + backticks.
            api_token: "a'b$(rm -rf ~)`x`".into(),
        };
        write_env_file_to(&path, &creds, "my-host").unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "env file must be 0600, got {mode:o}");

        let body = fs::read_to_string(&path).unwrap();
        assert!(!body.contains("__"), "unresolved placeholder:\n{body}");
        // The token stays inside single quotes with the embedded ' escaped, so
        // $(...) / backticks are inert.
        assert!(
            body.contains("export GRAFANA_API_TOKEN='a'\\''b$(rm -rf ~)`x`'"),
            "token not safely single-quoted:\n{body}"
        );
        assert!(body.contains("host.name=my-host"));
    }

    #[test]
    fn write_env_file_rejects_newline_in_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("env");
        let creds = GrafanaCloudCreds {
            otlp_endpoint: "https://e/otlp".into(),
            instance_id: "1".into(),
            api_token: "line1\nexport EVIL=1".into(),
        };
        assert!(write_env_file_to(&path, &creds, "h").is_err());
    }

    /// The three Grafana values written by `write_env_file` parse back out
    /// intact (remember-on-reopen), and the OTLP endpoint / resource attrs
    /// become session export lines.
    #[test]
    fn creds_round_trip_and_session_exports() {
        let text = ASSET_ENV_TEMPLATE
            .replace("__GRAFANA_OTLP_ENDPOINT__", "https://otlp.grafana.net/otlp")
            .replace("__GRAFANA_INSTANCE_ID__", "99999")
            .replace("__GRAFANA_API_TOKEN__", "glc_secret")
            .replace("__HOST_NAME__", "my-host");
        let map = parse_env_exports(&text);
        assert_eq!(
            map.get("GRAFANA_OTLP_ENDPOINT").unwrap(),
            "https://otlp.grafana.net/otlp"
        );
        assert_eq!(map.get("GRAFANA_INSTANCE_ID").unwrap(), "99999");
        assert_eq!(map.get("GRAFANA_API_TOKEN").unwrap(), "glc_secret");
        // Endpoint the agent actually posts to = the local Alloy collector.
        assert_eq!(
            map.get("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap(),
            "http://localhost:4318"
        );

        // Session exports carry the FULL generic config + endpoint + host attr,
        // and never the GRAFANA_* creds — built like session_otlp_exports.
        assert!(!map.get("GRAFANA_API_TOKEN").unwrap().is_empty());
        let exports = {
            let mut p = String::new();
            for (k, v) in SETTINGS_ENV {
                p.push_str(&format!("export {k}='{v}' && "));
            }
            p.push_str(&format!(
                "export OTEL_EXPORTER_OTLP_ENDPOINT='{LOCAL_OTLP_ENDPOINT}' && "
            ));
            if let Some(a) = map.get("OTEL_RESOURCE_ATTRIBUTES").filter(|v| !v.is_empty()) {
                p.push_str(&format!("export OTEL_RESOURCE_ATTRIBUTES='{a}' && "));
            }
            p
        };
        // Generic enable/exporter config so non-Claude agents export too.
        assert!(exports.contains("export CLAUDE_CODE_ENABLE_TELEMETRY='1'"));
        assert!(exports.contains("export OTEL_METRICS_EXPORTER='otlp'"));
        assert!(exports.contains("export OTEL_EXPORTER_OTLP_ENDPOINT='http://localhost:4318'"));
        assert!(exports.contains("host.name=my-host"));
        assert!(
            !exports.contains("GRAFANA_API_TOKEN"),
            "creds must not leak into the agent env"
        );
    }

    #[test]
    fn settings_env_merge_preserves_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{"foo":"bar","env":{"EXISTING":"keep","OTEL_LOG_USER_PROMPTS":"0"}}"#,
        )
        .unwrap();

        let added = ensure_settings_env_at(&path).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["foo"], "bar", "unrelated top-level key dropped");
        assert_eq!(v["env"]["EXISTING"], "keep", "unrelated env key dropped");
        // Must NOT clobber a user's existing value for one of our keys.
        assert_eq!(v["env"]["OTEL_LOG_USER_PROMPTS"], "0");
        assert!(!added.contains(&"OTEL_LOG_USER_PROMPTS".to_string()));
        assert!(added.contains(&"CLAUDE_CODE_ENABLE_TELEMETRY".to_string()));

        // Second run is a no-op.
        assert!(ensure_settings_env_at(&path).unwrap().is_empty());
    }

    #[test]
    fn settings_env_creates_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let added = ensure_settings_env_at(&path).unwrap();
        assert_eq!(added.len(), SETTINGS_ENV.len());
        assert!(path.exists());
    }

    #[test]
    fn settings_env_bails_on_non_object_env_without_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = r#"{"env":["x"]}"#;
        fs::write(&path, original).unwrap();
        assert!(ensure_settings_env_at(&path).is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            original,
            "must not modify a file it refuses to merge"
        );
    }

    #[test]
    fn settings_env_treats_null_env_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"env":null}"#).unwrap();
        let added = ensure_settings_env_at(&path).unwrap();
        assert_eq!(added.len(), SETTINGS_ENV.len());
    }

    #[test]
    fn shell_rc_backup_name_is_full_filename_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".zshrc");
        fs::write(&rc, "# my rc\nexport FOO=1\n").unwrap();
        let env_file = dir.path().join("grafana-cloud.env");

        assert!(ensure_shell_rc_at(&rc, &env_file).unwrap());
        // Backup must be `.zshrc.bak`, NOT `.zsh.bak`.
        let bak = dir.path().join(".zshrc.bak");
        assert!(bak.exists(), "backup not at .zshrc.bak");
        assert_eq!(fs::read_to_string(&bak).unwrap(), "# my rc\nexport FOO=1\n");
        assert!(fs::read_to_string(&rc).unwrap().contains("grafana-cloud.env"));

        // Idempotent: second run does nothing, no duplicate source block.
        assert!(!ensure_shell_rc_at(&rc, &env_file).unwrap());
        let sources = fs::read_to_string(&rc).unwrap().matches("source").count();
        assert_eq!(sources, 1, "duplicate source block appended");
    }

    #[test]
    fn shell_rc_created_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".bashrc");
        let env_file = dir.path().join("grafana-cloud.env");
        assert!(ensure_shell_rc_at(&rc, &env_file).unwrap());
        assert!(rc.exists());
    }
}
