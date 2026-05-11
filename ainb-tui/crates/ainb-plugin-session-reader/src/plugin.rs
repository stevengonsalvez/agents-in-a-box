//! Session-reader [`Plugin`] implementation.
//!
//! Pure data-plane plugin: no UI, no CLI namespace. On startup
//! ([`Plugin::on_init`]) it scans the four provider directories,
//! aggregates a [`UsageData`] snapshot, and publishes it on the
//! `sessions.usage_data` topic. While running it answers
//! [`SnapshotGet`](`HostClient::snapshot_get`)-style requests by simply
//! republishing the cached snapshot — but since the host already
//! tracks the latest publish per topic, plugins typically just rely on
//! the host's snapshot store. We re-scan when the host pushes a
//! `sessions.refresh_request` event.
//!
//! [`Plugin`]: ainb_plugin_sdk::Plugin
//! [`UsageData`]: ainb_plugin_types_sessions::UsageData

use ainb_plugin_sdk::{
    HandleEventParams, HostClient, Plugin, RenderParams, Result, SdkError, WireBuffer,
};
use ainb_plugin_types_sessions::{UsageDataEvent, WIRE_VERSION};
use async_trait::async_trait;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::scanner::{self, ProviderRoots};

/// Topic the plugin publishes the aggregated snapshot on. Burndown
/// (the consumer in Phase 7c-burndown) subscribes to this topic.
pub const TOPIC_USAGE_DATA: &str = "sessions.usage_data";

/// Topic any consumer can publish to to force a re-scan + re-publish.
pub const TOPIC_REFRESH_REQUEST: &str = "sessions.refresh_request";

mod manifest_text {
    pub const TOML: &str = include_str!("../manifest.toml");
}

/// Plugin state.
///
/// Holds the most recently published [`UsageDataEvent`] in memory so a
/// follow-up render or refresh can republish without re-running the
/// scan if no source files changed (rescan is still cheap on the
/// freshness path — we always rescan on `sessions.refresh_request`).
pub struct SessionReader {
    last_event: Option<UsageDataEvent>,
    roots: ProviderRoots,
}

impl SessionReader {
    /// Build a fresh reader with the canonical default provider roots.
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_event: None,
            roots: ProviderRoots::defaults(),
        }
    }

    /// Construct with explicit roots — used by unit tests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_roots(roots: ProviderRoots) -> Self {
        Self {
            last_event: None,
            roots,
        }
    }

    fn build_event(&self) -> UsageDataEvent {
        let data = scanner::scan(&self.roots);
        UsageDataEvent {
            version: WIRE_VERSION,
            published_ns: now_ns(),
            partial: false,
            data,
        }
    }

    async fn publish(&mut self, host: &HostClient) -> Result<()> {
        let event = self.build_event();
        let bytes = rmp_serde::to_vec_named(&event).map_err(|e| {
            SdkError::plugin(format!("encode UsageDataEvent: {e}"))
        })?;
        host.snapshot_publish(TOPIC_USAGE_DATA, bytes).await?;
        self.last_event = Some(event);
        Ok(())
    }
}

impl Default for SessionReader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for SessionReader {
    fn manifest(&self) -> &'static str {
        manifest_text::TOML
    }

    async fn on_init(&mut self, host: &HostClient, _granted: &[String]) -> Result<()> {
        // Diagnostic probes via host.log() — tagged with plugin name on
        // the host side. Used to bisect the subscribe-response stall;
        // remove once Bug 2 is conclusively fixed.
        let _ = host.log_info("on_init: entering, about to call snapshot_subscribe").await;
        let sub_res = host.snapshot_subscribe(TOPIC_REFRESH_REQUEST).await;
        let _ = host
            .log_info(format!(
                "on_init: snapshot_subscribe returned ok={}",
                sub_res.is_ok()
            ))
            .await;
        sub_res?;
        let _ = host.log_info("on_init: about to call publish").await;
        let pub_res = self.publish(host).await;
        let _ = host
            .log_info(format!(
                "on_init: publish returned ok={}",
                pub_res.is_ok()
            ))
            .await;
        pub_res
    }

    async fn render(
        &mut self,
        _host: &HostClient,
        _params: RenderParams,
    ) -> Result<WireBuffer> {
        // No UI surface — return an empty 0×0 buffer. The host's render
        // path tolerates empty buffers from headless plugins.
        Ok(WireBuffer::new(0, 0))
    }

    async fn handle_event(
        &mut self,
        host: &HostClient,
        params: HandleEventParams,
    ) -> Result<()> {
        if params.topic == TOPIC_REFRESH_REQUEST {
            tracing::info!("session-reader: refresh requested — rescanning");
            return self.publish(host).await;
        }
        tracing::debug!(topic = %params.topic, "session-reader: ignoring unrelated event");
        Ok(())
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_toml_parses_as_abi_v2() {
        let m: ainb_plugin_sdk::Manifest =
            toml::from_str(manifest_text::TOML).expect("manifest TOML parses");
        assert_eq!(m.plugin.name, "session-reader");
        assert_eq!(m.plugin.abi_version, 2);
        assert!(
            m.provides
                .snapshots
                .iter()
                .any(|t| t == TOPIC_USAGE_DATA),
            "expected manifest to declare publishing topic"
        );
        assert!(
            m.subscribes
                .snapshots
                .iter()
                .any(|t| t == TOPIC_REFRESH_REQUEST),
            "expected manifest to subscribe to refresh topic"
        );
    }

    #[test]
    fn build_event_has_correct_wire_version_and_partial_flag() {
        let plugin = SessionReader::with_roots(ProviderRoots::default());
        let event = plugin.build_event();
        assert_eq!(event.version, WIRE_VERSION);
        assert!(!event.partial);
    }
}
