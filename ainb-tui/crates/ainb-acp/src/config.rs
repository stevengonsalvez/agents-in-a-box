//! Static adapter configuration.
//!
//! Part 1 deliberately ships NO per-session adapter settings: the mode, the
//! command and the environment are daemon config and nothing else, which is
//! also what makes the Phase 6 "re-apply after `session/load`" step exact
//! (the same values were applied at `session/new`).

use std::path::PathBuf;

/// The two adapters the registry accepts in part 1.
pub const CLAUDE_ADAPTER: &str = "claude-agent-acp";
/// See [`CLAUDE_ADAPTER`].
pub const CODEX_ADAPTER: &str = "codex-acp";

/// Environment variables every adapter child gets, and the ONLY ones it gets
/// unless daemon config names more.
///
/// The spike's `bypassPermissions` leak was ambient-state inheritance, so the
/// child's environment is built from nothing (`env_clear`) and then filled
/// from this list plus [`AdapterConfig::env_passthrough`] /
/// [`AdapterConfig::extra_env`]. Never the daemon's whole environment.
pub const BASE_ENV_ALLOWLIST: &[&str] = &["PATH", "HOME"];

/// One adapter's static definition.
#[derive(Debug, Clone)]
pub struct AdapterConfig {
    /// Registry token, also the wire value of `fleet_acp_session.provider`.
    pub name: String,
    /// Executable to spawn. Defaults to `name` when built by [`AdapterConfig::new`].
    pub command: PathBuf,
    /// Arguments passed verbatim.
    pub args: Vec<String>,
    /// The permission mode PINNED at `session/new` and re-asserted after every
    /// `session/load` (I13). Never implicit: the spike observed
    /// `claude-agent-acp` inheriting `bypassPermissions` from ambient state,
    /// which silently disabled the whole permission surface R8 exists to build.
    pub permission_mode: String,
    /// Names of parent-environment variables to forward, on top of
    /// [`BASE_ENV_ALLOWLIST`]. This is where an adapter's own credential path
    /// variable is named (for example `CLAUDE_CODE_OAUTH_TOKEN`).
    pub env_passthrough: Vec<String>,
    /// Literal name/value pairs to set on the child, applied after the
    /// passthroughs so config always wins over the ambient value.
    pub extra_env: Vec<(String, String)>,
    /// `session/set_config_option` settings (model, reasoning effort, ...) as
    /// `(configId, valueId)` pairs.
    ///
    /// Applied at `session/new` AND re-applied after every `session/load`,
    /// because the spike proved adapter config does not survive a load. Both
    /// call sites read THIS list, which is what makes the re-application exact
    /// rather than approximate: there is no per-session config to diverge from
    /// (see "What we're NOT doing" in the plan).
    ///
    /// Empty by default: part 1 ships no configured model or reasoning value,
    /// only the mechanism that keeps one from being silently lost on resume.
    pub config_options: Vec<(String, String)>,
    /// OS-level FS confinement for this adapter's child, or `None` to spawn it
    /// unconfined (the daemon-wide adapters, which serve every chat tenant).
    ///
    /// A POLICY rather than a wrapped command, because the two platforms
    /// express confinement differently and only one of them is expressible as
    /// `(command, args)`: macOS swaps the program for `/usr/bin/sandbox-exec`,
    /// but Linux installs a Landlock ruleset in a `pre_exec` closure that lives
    /// ON the command object. Carrying the policy and building the command at
    /// spawn ([`crate::client`]) is the one shape that confines both.
    ///
    /// Set by the per-task adapter a hangar task registers, whose child is one
    /// agent working in one worktree and so has a confinable blast radius.
    pub sandbox: Option<ainb_hangar_sandbox::SandboxPolicy>,
}

impl AdapterConfig {
    /// Build a config for `name`, spawning `name` from `PATH` with the given
    /// pinned permission mode.
    pub fn new(name: impl Into<String>, permission_mode: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            command: PathBuf::from(&name),
            name,
            args: Vec::new(),
            permission_mode: permission_mode.into(),
            env_passthrough: Vec::new(),
            extra_env: Vec::new(),
            config_options: Vec::new(),
            sandbox: None,
        }
    }

    /// Override the executable (tests point this at the fake adapter binary).
    #[must_use]
    pub fn command(mut self, command: impl Into<PathBuf>) -> Self {
        self.command = command.into();
        self
    }

    /// Forward these parent-environment variables to the child.
    #[must_use]
    pub fn env_passthrough(mut self, names: Vec<String>) -> Self {
        self.env_passthrough = names;
        self
    }

    /// Set these literal variables on the child.
    #[must_use]
    pub fn extra_env(mut self, pairs: Vec<(String, String)>) -> Self {
        self.extra_env = pairs;
        self
    }

    /// Set these `session/set_config_option` settings on every session.
    #[must_use]
    pub fn config_options(mut self, options: Vec<(String, String)>) -> Self {
        self.config_options = options;
        self
    }

    /// Confine this adapter's child to `policy` (see [`AdapterConfig::sandbox`]).
    #[must_use]
    pub fn sandbox(mut self, policy: ainb_hangar_sandbox::SandboxPolicy) -> Self {
        self.sandbox = Some(policy);
        self
    }

    /// True when `name` is one of the adapters part 1 knows how to spawn.
    ///
    /// The RPC layer validates against this; the schema only length-checks
    /// `fleet_acp_session.provider`, so the next adapter needs no migration.
    pub fn is_known_adapter(name: &str) -> bool {
        matches!(name, CLAUDE_ADAPTER | CODEX_ADAPTER)
    }
}

/// Resolve the exact environment an adapter child is spawned with.
///
/// `lookup` reads the parent environment (injected so the allowlist is
/// testable without mutating the process). Order is base allowlist, then named
/// passthroughs, then literal `extra_env` overrides.
pub fn allowlisted_env(
    config: &AdapterConfig,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();
    let mut push = |name: &str, value: String| {
        if let Some(slot) = env.iter_mut().find(|(existing, _)| existing == name) {
            slot.1 = value;
        } else {
            env.push((name.to_string(), value));
        }
    };
    for name in BASE_ENV_ALLOWLIST {
        if let Some(value) = lookup(name) {
            push(name, value);
        }
    }
    for name in &config.env_passthrough {
        if let Some(value) = lookup(name) {
            push(name, value);
        }
    }
    for (name, value) in &config.extra_env {
        push(name, value.clone());
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn allowlist_drops_everything_not_named() {
        let config = AdapterConfig::new(CLAUDE_ADAPTER, "default");
        let env = allowlisted_env(
            &config,
            &fixed_env(&[
                ("PATH", "/usr/bin"),
                ("HOME", "/home/agent"),
                ("AINB_PLANTED_SECRET", "leaked"),
                ("AWS_SECRET_ACCESS_KEY", "leaked"),
            ]),
        );
        let names: Vec<&str> = env.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["PATH", "HOME"]);
    }

    #[test]
    fn named_passthrough_and_extra_env_are_forwarded() {
        let config = AdapterConfig::new(CLAUDE_ADAPTER, "default")
            .env_passthrough(vec!["CLAUDE_CODE_OAUTH_TOKEN".to_string()])
            .extra_env(vec![("ACP_LOG".to_string(), "debug".to_string())]);
        let env = allowlisted_env(
            &config,
            &fixed_env(&[
                ("PATH", "/usr/bin"),
                ("CLAUDE_CODE_OAUTH_TOKEN", "token"),
                ("AINB_PLANTED_SECRET", "leaked"),
            ]),
        );
        assert_eq!(
            env,
            vec![
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("CLAUDE_CODE_OAUTH_TOKEN".to_string(), "token".to_string()),
                ("ACP_LOG".to_string(), "debug".to_string()),
            ]
        );
    }

    #[test]
    fn extra_env_overrides_a_passthrough_of_the_same_name() {
        let config = AdapterConfig::new(CODEX_ADAPTER, "default")
            .env_passthrough(vec!["PATH".to_string()])
            .extra_env(vec![("PATH".to_string(), "/opt/pinned".to_string())]);
        let env = allowlisted_env(&config, &fixed_env(&[("PATH", "/usr/bin")]));
        assert_eq!(env, vec![("PATH".to_string(), "/opt/pinned".to_string())]);
    }

    #[test]
    fn only_the_two_part_one_adapters_are_known() {
        assert!(AdapterConfig::is_known_adapter(CLAUDE_ADAPTER));
        assert!(AdapterConfig::is_known_adapter(CODEX_ADAPTER));
        assert!(!AdapterConfig::is_known_adapter("gemini-acp"));
    }
}
