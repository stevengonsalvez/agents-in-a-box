//! The desktop-native path for the onboarding steps that write outside ainb
//! (#1175): install a dependency, write `~/.tmux.conf`, finish an
//! OpenTelemetry setup.
//!
//! The webview is script-reachable, so the reducer's rows for these writes
//! stay refused from the window ([`crate::intent::refused_from_webview`]) and
//! their refusal points here. The write itself runs from the shell, after a
//! confirmation the shell owns: a native dialog the window's script cannot
//! click. This module holds what the dialog asks and what the write does; the
//! Tauri binary wires the dialog in front of [`SetupWrite::run`] and never
//! exposes `run` to the webview as a command.
//!
//! Nothing here goes through the reducer: the same `ainb-app` functions the
//! onboarding wizard calls are called directly, so the key-only rule
//! (`Keymap::is_key_only`) is not relaxed and no new reducer row writes
//! outside ainb. The status the page shows is a host read, never framed.

use std::path::PathBuf;

use ainb_app::setup::{
    DepTier, RealEnv, catalog, detect_all, install_dep_capture, install_tmux_config,
};
use serde::{Deserialize, Serialize};

/// One of the writes the settings page may ask the shell to confirm.
///
/// `Debug` is written by hand: the telemetry write carries a token, and a
/// `{:?}` of it would put that token in the log `show_log` reads back into
/// the window.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SetupWrite {
    /// Install the catalog dependency `id`, the way onboarding's `i` does.
    InstallDependency { id: String },
    /// Install every unmet dependency the catalog can install itself, the way
    /// onboarding's `I` does one at a time.
    InstallAllDependencies,
    /// Write ainb's tmux configuration to `~/.tmux.conf`, keeping a backup of
    /// what was there, the way onboarding's `t` does.
    WriteTmuxConfig,
    /// Finish the OpenTelemetry setup onboarding's Next or Finish runs with
    /// the three fields filled: the env file under ainb, then Claude Code's
    /// settings and the shell rc outside it, and the collector if installed.
    FinishOpenTelemetry {
        otlp_endpoint: String,
        instance_id: String,
        api_token: String,
    },
}

impl std::fmt::Debug for SetupWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InstallDependency { id } => {
                f.debug_struct("InstallDependency").field("id", id).finish()
            }
            Self::InstallAllDependencies => f.write_str("InstallAllDependencies"),
            Self::WriteTmuxConfig => f.write_str("WriteTmuxConfig"),
            Self::FinishOpenTelemetry {
                otlp_endpoint,
                instance_id,
                ..
            } => f
                .debug_struct("FinishOpenTelemetry")
                .field("otlp_endpoint", otlp_endpoint)
                .field("instance_id", instance_id)
                .field("api_token", &"<redacted>")
                .finish(),
        }
    }
}

/// What the confirmation dialog asks: a title and the body naming exactly
/// what is written outside ainb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    pub title: String,
    pub body: String,
}

impl SetupWrite {
    /// The variant's name, for a log line that must carry nothing else.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::InstallDependency { .. } => "install_dependency",
            Self::InstallAllDependencies => "install_all_dependencies",
            Self::WriteTmuxConfig => "write_tmux_config",
            Self::FinishOpenTelemetry { .. } => "finish_open_telemetry",
        }
    }

    /// What the write needs before a dialog is worth showing: the telemetry
    /// fields present, and the endpoint an `https://` URL, because the token
    /// is sent to it.
    ///
    /// # Errors
    ///
    /// Which field is missing or not https, for a toast.
    pub fn validate(&self) -> Result<(), String> {
        let Self::FinishOpenTelemetry {
            otlp_endpoint,
            instance_id,
            api_token,
        } = self
        else {
            return Ok(());
        };
        if otlp_endpoint.trim().is_empty()
            || instance_id.trim().is_empty()
            || api_token.trim().is_empty()
        {
            return Err(
                "OpenTelemetry needs all three fields: endpoint, instance ID and token".to_string(),
            );
        }
        if !otlp_endpoint.trim().starts_with("https://") {
            return Err("the OTLP endpoint must be an https:// URL".to_string());
        }
        Ok(())
    }

    /// The question the shell puts to the person before running this write.
    #[must_use]
    pub fn confirmation(&self) -> Confirmation {
        match self {
            Self::InstallDependency { id } => {
                let name = catalog()
                    .into_iter()
                    .flat_map(|topic| topic.deps)
                    .find(|dep| dep.id == id)
                    .map_or_else(|| id.clone(), |dep| dep.name.to_string());
                Confirmation {
                    title: format!("Install {name}?"),
                    body: format!(
                        "This runs the package manager on this machine to install {name}. \
                         It changes software outside ainb."
                    ),
                }
            }
            Self::InstallAllDependencies => Confirmation {
                title: "Install every missing dependency?".to_string(),
                body: "This runs the package manager on this machine for each dependency \
                       ainb can install itself. It changes software outside ainb."
                    .to_string(),
            },
            Self::WriteTmuxConfig => Confirmation {
                title: "Write ~/.tmux.conf?".to_string(),
                body: "This replaces ~/.tmux.conf with ainb's tmux configuration. A backup \
                       of the current file is kept beside it."
                    .to_string(),
            },
            // The dialog is the trust anchor for fields the webview typed, so
            // it shows the endpoint the token will be sent to and the instance
            // id, never the token.
            Self::FinishOpenTelemetry {
                otlp_endpoint,
                instance_id,
                ..
            } => Confirmation {
                title: "Finish the OpenTelemetry setup?".to_string(),
                body: format!(
                    "This writes the OTLP environment into Claude Code's settings.json and \
                     appends a line to your shell rc so every shell sources it. The \
                     credentials stay in ainb's own env file.\n\nEndpoint: {}\nInstance ID: {}",
                    otlp_endpoint.trim(),
                    instance_id.trim()
                ),
            },
        }
    }

    /// Run the write. Blocking, possibly for as long as a package install
    /// takes: call it off the main thread, and only after
    /// [`confirmation`](Self::confirmation) was answered yes.
    ///
    /// # Errors
    ///
    /// What the write reported, for a toast: a dependency with no automatic
    /// installer, a failed install, an OpenTelemetry field left empty, or the
    /// file write that failed.
    pub fn run(self) -> Result<String, String> {
        match self {
            Self::InstallDependency { id } => {
                let dep = catalog()
                    .into_iter()
                    .flat_map(|topic| topic.deps)
                    .find(|dep| dep.id == id)
                    .ok_or_else(|| format!("unknown dependency: {id}"))?;
                install_dep_capture(&dep).map_err(|error| format!("{}: {error}", dep.name))?;
                Ok(format!("installed {}", dep.name))
            }
            Self::InstallAllDependencies => {
                let status = detect_all(&RealEnv);
                let wanted: Vec<&str> = status
                    .topics
                    .iter()
                    .flat_map(|topic| &topic.deps)
                    .filter(|dep| !dep.satisfied && dep.auto_installable)
                    .map(|dep| dep.id)
                    .collect();
                if wanted.is_empty() {
                    return Ok("nothing to install".to_string());
                }
                let mut installed = Vec::new();
                let mut failed = Vec::new();
                for dep in catalog().into_iter().flat_map(|topic| topic.deps) {
                    if !wanted.contains(&dep.id) {
                        continue;
                    }
                    match install_dep_capture(&dep) {
                        Ok(()) => installed.push(dep.name),
                        Err(error) => failed.push(format!("{}: {error}", dep.name)),
                    }
                }
                if failed.is_empty() {
                    Ok(format!("installed {}", installed.join(", ")))
                } else {
                    Err(failed.join("; "))
                }
            }
            Self::WriteTmuxConfig => install_tmux_config()
                .map(|()| "wrote ~/.tmux.conf (backup kept if one existed)".to_string()),
            Self::FinishOpenTelemetry {
                otlp_endpoint,
                instance_id,
                api_token,
            } => {
                Self::FinishOpenTelemetry {
                    otlp_endpoint: otlp_endpoint.clone(),
                    instance_id: instance_id.clone(),
                    api_token: api_token.clone(),
                }
                .validate()?;
                let creds = ainb_app::otel::GrafanaCloudCreds {
                    otlp_endpoint: otlp_endpoint.trim().to_string(),
                    instance_id: instance_id.trim().to_string(),
                    api_token: api_token.trim().to_string(),
                };
                let host = ainb_app::otel::detect_host_name();
                let failed = |error| format!("OpenTelemetry setup failed: {error:#}");
                ainb_app::otel::write_assets().map_err(failed)?;
                ainb_app::otel::write_env_file(&creds, &host).map_err(failed)?;
                ainb_app::otel::ensure_settings_env().map_err(failed)?;
                ainb_app::otel::ensure_shell_rc_sources_env().map_err(failed)?;
                let collector = if ainb_app::otel::alloy_installed() {
                    match ainb_app::otel::start_alloy() {
                        Ok(()) => ", collector started",
                        Err(_) => ", collector not started",
                    }
                } else {
                    ""
                };
                Ok(format!("OpenTelemetry set up{collector}"))
            }
        }
    }
}

/// A catalog dependency as the page lists it: detected, never installed by
/// the page itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct DependencyView {
    pub id: String,
    pub name: String,
    pub why: String,
    pub required: bool,
    pub satisfied: bool,
    /// Whether the catalog knows how to install it here; otherwise `hint`
    /// says what to run by hand.
    pub auto_installable: bool,
    pub hint: String,
}

/// Where the OpenTelemetry setup stands, from `ainb_app::otel::status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct OtelView {
    pub env_file_present: bool,
    pub settings_env_present: bool,
    pub alloy_installed: bool,
    pub alloy_running: bool,
}

/// What the settings page's Setup panel shows: a host read at the moment it
/// was asked for, never a framed section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct SetupView {
    pub dependencies: Vec<DependencyView>,
    pub tmux_conf_present: bool,
    pub otel: OtelView,
}

/// `~/.tmux.conf`, under the home the process was given.
fn tmux_conf_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".tmux.conf"))
}

/// Detect every catalog dependency and the two files, as onboarding's check
/// does. Blocking on binary probes: call it off the main thread.
#[must_use]
pub fn status() -> SetupView {
    let detected = detect_all(&RealEnv);
    let dependencies = detected
        .topics
        .iter()
        .flat_map(|topic| &topic.deps)
        .map(|dep| DependencyView {
            id: dep.id.to_string(),
            name: dep.name.to_string(),
            why: dep.why.to_string(),
            required: dep.tier == DepTier::Required,
            satisfied: dep.satisfied,
            auto_installable: dep.auto_installable,
            hint: dep.install_hint.clone(),
        })
        .collect();
    let otel = ainb_app::otel::status();
    SetupView {
        dependencies,
        tmux_conf_present: tmux_conf_path().is_some_and(|path| path.exists()),
        otel: OtelView {
            env_file_present: otel.env_file_present,
            settings_env_present: otel.settings_env_present,
            alloy_installed: otel.alloy_installed,
            alloy_running: otel.alloy_running,
        },
    }
}

/// Where the refusal of a reducer row points once this path exists: the
/// onboarding rows, whose refused writes (the installs, `~/.tmux.conf`, the
/// wizard's Next and Finish with telemetry set up) the Setup panel runs from
/// the window. Judged by the row's id, because a host never sees the
/// reducer's events. `None` for every other refused row, whose reason stands
/// as the reducer gives it.
#[must_use]
pub fn desktop_path(id: &ainb_app::CommandId) -> Option<&'static str> {
    id.as_str().starts_with("onboarding.").then_some(DESKTOP_PATH)
}

/// The refusal reason for a row the Setup panel covers.
pub const DESKTOP_PATH: &str = "it writes outside ainb; the window runs it from Settings, Setup, behind the shell's own confirmation";
