//! Capability gate: decides which host-fn imports get linked into a plugin's
//! wasmi `Linker` based on the plugin's manifest.
//!
//! The gate is *static*: a plugin that imports `ainb_sessions_list` without
//! declaring `read_sessions = true` fails at instantiation because wasmi sees
//! an unresolved import. There is no runtime "permission denied" — the import
//! simply isn't there.

use ainb_plugin_api::host::{GatedBy, HostFn, HOST_FN_CATALOGUE};
use ainb_plugin_api::CapabilitySet;

/// Which host-fns should be linked for a plugin holding `caps`.
#[must_use]
pub fn allowed_host_fns(caps: &CapabilitySet) -> Vec<&'static HostFn> {
    HOST_FN_CATALOGUE
        .iter()
        .filter(|f| match f.gate {
            None => true,
            Some(g) => is_granted(g, caps),
        })
        .collect()
}

fn is_granted(g: GatedBy, caps: &CapabilitySet) -> bool {
    match g {
        GatedBy::ReadSessions => caps.read_sessions,
        GatedBy::WritePluginData => caps.write_plugin_data,
        GatedBy::EventBus => caps.event_bus,
        GatedBy::ReadClaudeLogs => caps.read_claude_logs,
        GatedBy::ReadCodexLogs => caps.read_codex_logs,
        GatedBy::SpawnSubprocess => caps.spawn_subprocess,
        GatedBy::Network => !caps.network.is_empty(),
        GatedBy::Filesystem => !caps.filesystem.is_empty(),
        GatedBy::LogsRead => caps.read_claude_logs || caps.read_codex_logs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_caps_yield_baseline_only() {
        let caps = CapabilitySet::default();
        let allowed: Vec<_> = allowed_host_fns(&caps)
            .iter()
            .map(|f| f.name)
            .collect();
        // Baseline grew in Phase 6: cache_get/put are always-on (the
        // per-plugin scoping replaces a capability gate).
        assert_eq!(
            allowed,
            vec![
                "ainb_log",
                "ainb_now_ms",
                "ainb_render_buffer",
                "ainb_cache_get",
                "ainb_cache_put",
            ]
        );
    }

    #[test]
    fn read_sessions_unlocks_session_fns() {
        let caps = CapabilitySet {
            read_sessions: true,
            ..Default::default()
        };
        let names: Vec<_> = allowed_host_fns(&caps)
            .iter()
            .map(|f| f.name)
            .collect();
        assert!(names.contains(&"ainb_sessions_list"));
        assert!(names.contains(&"ainb_session_get"));
        assert!(!names.contains(&"ainb_data_read"));
    }

    #[test]
    fn empty_network_allowlist_is_no_grant() {
        let caps = CapabilitySet {
            network: Vec::new(),
            ..Default::default()
        };
        let names: Vec<_> = allowed_host_fns(&caps)
            .iter()
            .map(|f| f.name)
            .collect();
        assert!(!names.contains(&"ainb_http_request"));
    }

    #[test]
    fn nonempty_network_allowlist_grants_http() {
        let caps = CapabilitySet {
            network: vec!["api.example.com:443".into()],
            ..Default::default()
        };
        let names: Vec<_> = allowed_host_fns(&caps)
            .iter()
            .map(|f| f.name)
            .collect();
        assert!(names.contains(&"ainb_http_request"));
    }
}
