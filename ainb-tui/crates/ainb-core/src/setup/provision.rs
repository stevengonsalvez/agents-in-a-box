// ABOUTME: Provisioner engine for the setup catalog — turns an `Install` spec
// into a concrete command plan and runs it under a consent policy. Both the TUI
// (keypress -> install) and the CLI (`ainb init` interactive / --yes) call this
// so provisioning behaves identically on both surfaces.
//
// Safety policy: first-party / per-user installers (npm/uv/cargo/ainb/claude
// plugin/toolkit) may run under a bare headless `--yes`. System installers that
// touch the OS or pipe a remote script to a shell (brew/curl) require EXPLICIT
// per-item consent and are never auto-run headlessly. Manual/bundled deps are
// only ever printed.

use std::fs;
use std::process::Command;

use anyhow::{Result, bail};

use crate::setup::catalog::{Dep, Install};

/// How much consent an install requires before it may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentLevel {
    /// First-party / per-user — may run under a bare `--yes`.
    Auto,
    /// Touches OS/PATH or pipes a remote script — needs explicit per-item yes.
    Explicit,
    /// No installer (manual hint / bundled) — only ever printed.
    Never,
}

/// Caller's overall provisioning posture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionMode {
    /// Headless: auto-run anything `Auto`; print `Explicit`/`Never`.
    Yes,
    /// Interactive: confirm before running `Auto`/`Explicit`.
    Ask,
    /// Never run; just report what would happen.
    DryRun,
}

/// A resolved command plan for one install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    /// Copy-paste command line shown to the user.
    pub display: String,
    /// argv to execute; `None` when there's nothing to run (manual/bundled).
    pub argv: Option<Vec<String>>,
    pub consent: ConsentLevel,
}

/// What the consent policy decides for a (consent, mode) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run without prompting.
    Run,
    /// Prompt the user, run only if they confirm.
    Confirm,
    /// Don't run; print the command for the user to run themselves.
    Print,
}

/// Outcome of provisioning one dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionOutcome {
    /// Installer ran and exited successfully.
    Installed,
    /// Not run — here's the command to run yourself (with the reason).
    PrintOnly {
        command: String,
        reason: &'static str,
    },
    /// User declined the confirmation prompt.
    Declined,
}

/// Pure consent decision — kept separate from execution so it's unit-testable.
pub fn decide(consent: ConsentLevel, mode: ProvisionMode) -> Decision {
    match (consent, mode) {
        (ConsentLevel::Never, _) => Decision::Print,
        (_, ProvisionMode::DryRun) => Decision::Print,
        (ConsentLevel::Auto, ProvisionMode::Yes) => Decision::Run,
        (ConsentLevel::Auto, ProvisionMode::Ask) => Decision::Confirm,
        // System installers never auto-run headlessly — only on explicit consent.
        (ConsentLevel::Explicit, ProvisionMode::Yes) => Decision::Print,
        (ConsentLevel::Explicit, ProvisionMode::Ask) => Decision::Confirm,
    }
}

fn s(v: &str) -> String {
    v.to_string()
}

/// Resolve an `Install` into a concrete command plan (pure — no execution).
pub fn plan(install: &Install) -> InstallPlan {
    let display = install.hint();
    match install {
        Install::Npm(p) => InstallPlan {
            argv: Some(
                ["npm", "install", "-g"]
                    .iter()
                    .map(|x| s(x))
                    .chain(p.iter().map(|x| s(x)))
                    .collect(),
            ),
            consent: ConsentLevel::Auto,
            display,
        },
        Install::Uv(spec) => InstallPlan {
            argv: Some(vec![s("uv"), s("tool"), s("install"), s(spec)]),
            consent: ConsentLevel::Auto,
            display,
        },
        Install::Cargo(c) => InstallPlan {
            argv: Some(vec![s("cargo"), s("install"), s(c)]),
            consent: ConsentLevel::Auto,
            display,
        },
        // ainb subcommands invoke THIS binary (resolved at execution time).
        Install::Ainb(a) => InstallPlan {
            argv: Some(std::iter::once(s("ainb")).chain(a.iter().map(|x| s(x))).collect()),
            consent: ConsentLevel::Auto,
            display,
        },
        // Add the marketplace over HTTPS first (shorthand defaults to SSH and
        // fails without GitHub SSH keys), then install. `|| true` keeps the
        // already-added case from aborting.
        Install::ClaudePlugin { spec, marketplace: Some(url) } => InstallPlan {
            argv: Some(vec![
                s("sh"),
                s("-c"),
                format!(
                    "claude plugin marketplace add {url} 2>/dev/null || true; \
                     claude plugin install {spec}"
                ),
            ]),
            consent: ConsentLevel::Auto,
            display,
        },
        Install::ClaudePlugin { spec, marketplace: None } => InstallPlan {
            argv: Some(vec![s("claude"), s("plugin"), s("install"), s(spec)]),
            consent: ConsentLevel::Auto,
            display,
        },
        // Toolkit is a multi-step git-clone + migrate flow handled by a dedicated
        // provisioner (phase 5); surface it as explicit-consent, print for now.
        Install::Toolkit => InstallPlan {
            argv: None,
            consent: ConsentLevel::Explicit,
            display,
        },
        // System installers: runnable, but only after explicit consent.
        Install::Brew(p) => InstallPlan {
            argv: Some(
                ["brew", "install"].iter().map(|x| s(x)).chain(p.iter().map(|x| s(x))).collect(),
            ),
            consent: ConsentLevel::Explicit,
            display,
        },
        Install::Curl(url) => InstallPlan {
            argv: Some(vec![s("sh"), s("-c"), format!("curl -LsSf {url} | sh")]),
            consent: ConsentLevel::Explicit,
            display,
        },
        Install::Manual(_) | Install::BundledWith(_) => InstallPlan {
            argv: None,
            consent: ConsentLevel::Never,
            display,
        },
    }
}

/// Resolve the program for an argv, mapping the `ainb` placeholder to the current
/// executable so subcommands hit this exact binary, not whatever's on PATH.
fn program(argv0: &str) -> String {
    if argv0 == "ainb" {
        if let Ok(exe) = std::env::current_exe() {
            return exe.to_string_lossy().into_owned();
        }
    }
    argv0.to_string()
}

/// Execute a plan's argv, inheriting stdio so the user sees installer output.
fn execute(argv: &[String]) -> Result<()> {
    let (head, rest) = argv.split_first().ok_or_else(|| anyhow::anyhow!("empty argv"))?;
    let status = Command::new(program(head)).args(rest).status()?;
    if !status.success() {
        bail!("{} exited with {}", head, status);
    }
    Ok(())
}

/// Provision one dependency under the given mode. `confirm` is called with the
/// display command when the policy needs interactive consent; return `true` to
/// proceed. Pass `|_| false` for non-interactive callers.
pub fn provision(
    dep: &Dep,
    mode: ProvisionMode,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<ProvisionOutcome> {
    let p = plan(&dep.install);
    let reason: &'static str = match p.consent {
        ConsentLevel::Explicit => "needs your OK (touches your OS/PATH)",
        ConsentLevel::Never => "no automatic installer — run this yourself",
        ConsentLevel::Auto => "",
    };
    match decide(p.consent, mode) {
        Decision::Print => Ok(ProvisionOutcome::PrintOnly {
            command: p.display,
            reason,
        }),
        Decision::Confirm => {
            if confirm(&p.display) {
                match p.argv {
                    Some(argv) => execute(&argv).map(|_| ProvisionOutcome::Installed),
                    None => Ok(ProvisionOutcome::PrintOnly {
                        command: p.display,
                        reason,
                    }),
                }
            } else {
                Ok(ProvisionOutcome::Declined)
            }
        }
        Decision::Run => match p.argv {
            Some(argv) => execute(&argv).map(|_| ProvisionOutcome::Installed),
            None => Ok(ProvisionOutcome::PrintOnly {
                command: p.display,
                reason,
            }),
        },
    }
}

/// Install the bundled, optimized tmux.conf (anti-flicker + clipboard), backing
/// up any existing config. Reloads it if tmux is running. This is the `I`-key
/// action in the onboarding dependency step — kept here so the setup engine owns
/// every provisioning side effect.
pub fn install_tmux_config() -> Result<(), String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    let tmux_conf_path = home.join(".tmux.conf");
    let tmux_config = include_str!("../../../../config/tmux.conf");

    if tmux_conf_path.exists() {
        let backup_path = home.join(".tmux.conf.backup");
        fs::copy(&tmux_conf_path, &backup_path)
            .map_err(|e| format!("Failed to backup existing config: {e}"))?;
    }
    fs::write(&tmux_conf_path, tmux_config)
        .map_err(|e| format!("Failed to write tmux.conf: {e}"))?;
    let _ = Command::new("tmux")
        .args(["source-file", &tmux_conf_path.to_string_lossy()])
        .output();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_matrix() {
        use ConsentLevel::*;
        use Decision::*;
        use ProvisionMode::*;
        assert_eq!(decide(Auto, Yes), Run);
        assert_eq!(decide(Auto, Ask), Confirm);
        assert_eq!(decide(Auto, DryRun), Print);
        // System installs never auto-run headlessly.
        assert_eq!(decide(Explicit, Yes), Print);
        assert_eq!(decide(Explicit, Ask), Confirm);
        assert_eq!(decide(Explicit, DryRun), Print);
        assert_eq!(decide(Never, Yes), Print);
        assert_eq!(decide(Never, Ask), Print);
    }

    #[test]
    fn plan_npm() {
        let p = plan(&Install::Npm(&["@google/gemini-cli"]));
        assert_eq!(p.consent, ConsentLevel::Auto);
        assert_eq!(
            p.argv.unwrap(),
            vec!["npm", "install", "-g", "@google/gemini-cli"]
        );
    }

    #[test]
    fn plan_uv() {
        let p = plan(&Install::Uv("headroom-ai[proxy]"));
        assert_eq!(
            p.argv.unwrap(),
            vec!["uv", "tool", "install", "headroom-ai[proxy]"]
        );
        assert_eq!(p.consent, ConsentLevel::Auto);
    }

    #[test]
    fn plan_ainb_uses_placeholder() {
        let p = plan(&Install::Ainb(&["reflect", "bootstrap", "--yes"]));
        assert_eq!(
            p.argv.unwrap(),
            vec!["ainb", "reflect", "bootstrap", "--yes"]
        );
        assert_eq!(p.consent, ConsentLevel::Auto);
    }

    #[test]
    fn plan_brew_is_explicit() {
        let p = plan(&Install::Brew(&["git", "tmux"]));
        assert_eq!(p.argv.unwrap(), vec!["brew", "install", "git", "tmux"]);
        assert_eq!(p.consent, ConsentLevel::Explicit);
    }

    #[test]
    fn plan_curl_wraps_in_shell() {
        let p = plan(&Install::Curl("https://astral.sh/uv/install.sh"));
        let argv = p.argv.unwrap();
        assert_eq!(argv[0], "sh");
        assert_eq!(argv[1], "-c");
        assert!(argv[2].contains("curl -LsSf https://astral.sh/uv/install.sh | sh"));
        assert_eq!(p.consent, ConsentLevel::Explicit);
    }

    #[test]
    fn plan_manual_and_bundled_are_never() {
        assert_eq!(
            plan(&Install::Manual("see docs")).consent,
            ConsentLevel::Never
        );
        assert!(plan(&Install::Manual("see docs")).argv.is_none());
        assert_eq!(
            plan(&Install::BundledWith("ainb")).consent,
            ConsentLevel::Never
        );
    }
}
