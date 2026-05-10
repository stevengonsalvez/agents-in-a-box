//! Smoke test: drive an SDK-based plugin with the same shape as the
//! `ainb-plugin-runtime` subprocess fixture, end-to-end.
//!
//! The runtime's fixture binary
//! (`crates/ainb-plugin-runtime/tests/fixtures/fixture_plugin.rs`) is a
//! hand-rolled stdio loop with no SDK dependency — it exists to prove
//! the runtime can speak the wire protocol against any conforming
//! peer. This test mirrors the *behaviour* of that fixture inside the
//! testkit's harness:
//!
//! - on init, publish snapshot `fixture.greeting` = `b"hello"`
//! - on `plugin/render`, return a 1×1 buffer with `X` at (0, 0)
//! - on `plugin/cli_dispatch`, return `stdout = "ok\n"`, exit code 0
//! - clean shutdown
//!
//! If this test passes, third-party plugin authors can write the same
//! shape against [`ainb_plugin_testkit::run_plugin`] without a single
//! subprocess spawn.

use std::time::Duration;

use ainb_plugin_testkit::{
    methods, run_plugin, CliDispatchParams, CliDispatchResult, HarnessError, RenderResult,
    Viewport, WireBuffer,
};
use ainb_plugin_sdk::{CliOutput, HostClient, Plugin, Result as SdkResult};
use ainb_plugin_protocol::params::RenderParams;
use ainb_plugin_protocol::wire_buffer::{Cell, Coord};
use async_trait::async_trait;

struct FixturePlugin;

#[async_trait]
impl Plugin for FixturePlugin {
    fn manifest(&self) -> &'static str {
        "[plugin]\nname = \"fixture\"\nversion = \"0.1.0\"\nabi_version = 2\n"
    }

    async fn on_init(
        &mut self,
        host: &HostClient,
        _granted: &[String],
    ) -> SdkResult<()> {
        host.snapshot_publish("fixture.greeting", bytes::Bytes::from_static(b"hello"))
            .await
    }

    async fn render(
        &mut self,
        _host: &HostClient,
        _params: RenderParams,
    ) -> SdkResult<WireBuffer> {
        let mut buf = WireBuffer::new(1, 1);
        buf.push(Coord::new(0, 0), Cell::new("X"));
        Ok(buf)
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        _namespace: &str,
        _argv: &[String],
    ) -> SdkResult<CliOutput> {
        Ok(CliOutput::ok(b"ok\n".to_vec()))
    }
}

#[tokio::test]
async fn fixture_plugin_full_behaviour_round_trip() -> Result<(), HarnessError> {
    let mut h = run_plugin(FixturePlugin);

    // 1. init
    let init = h.init().await?;
    assert_eq!(init["name"], "fixture");
    assert_eq!(init["version"], "0.1.0");

    // 2. on_init published a snapshot — the assertion drains briefly.
    h.assert_publishes_topic("fixture.greeting").await?;

    // 3. render returns the expected 1×1 X buffer.
    let buf = h.render(Viewport::new(80, 24)).await?;
    assert_eq!(buf.width, 1);
    assert_eq!(buf.height, 1);
    assert_eq!(buf.cells.len(), 1);

    // 4. cli_dispatch returns "ok\n" with exit 0.
    let value = h
        .send_request(
            methods::PLUGIN_CLI_DISPATCH,
            CliDispatchParams {
                namespace: "fixture".into(),
                argv: vec!["status".into()],
            },
        )
        .await?;
    let cli: CliDispatchResult = serde_json::from_value(value)?;
    assert_eq!(&cli.stdout[..], b"ok\n");
    assert_eq!(cli.exit_code, 0);

    // 5. round-trip determinism.
    h.assert_render_round_trip(Viewport::new(8, 4)).await?;

    h.close().await
}

#[tokio::test]
async fn harness_surfaces_default_cli_dispatch_unknown_namespace() -> Result<(), HarnessError> {
    // A plugin that does NOT override cli_dispatch falls back to the
    // SDK default (exit 2, stderr "plugin does not handle namespace ...").
    struct NoCli;
    #[async_trait]
    impl Plugin for NoCli {
        fn manifest(&self) -> &'static str {
            "[plugin]\nname = \"no-cli\"\nversion = \"0.0.0\"\nabi_version = 2\n"
        }
        async fn render(
            &mut self,
            _h: &HostClient,
            _p: RenderParams,
        ) -> SdkResult<WireBuffer> {
            Ok(WireBuffer::new(0, 0))
        }
    }

    let mut h = run_plugin(NoCli);
    h.init().await?;
    let value = h
        .send_request(
            methods::PLUGIN_CLI_DISPATCH,
            CliDispatchParams {
                namespace: "anything".into(),
                argv: vec![],
            },
        )
        .await?;
    let cli: CliDispatchResult = serde_json::from_value(value)?;
    assert_eq!(cli.exit_code, 2);
    assert!(String::from_utf8_lossy(&cli.stderr).contains("anything"));
    h.close().await
}

#[tokio::test]
async fn close_unblocks_when_plugin_did_no_work() -> Result<(), HarnessError> {
    // Edge case: prove that `close` works even on a freshly-spawned
    // plugin that never received init. Catches regressions where
    // shutdown notification handling depends on prior state.
    struct Bare;
    #[async_trait]
    impl Plugin for Bare {
        fn manifest(&self) -> &'static str {
            "[plugin]\nname = \"bare\"\nversion = \"0.0.0\"\nabi_version = 2\n"
        }
        async fn render(
            &mut self,
            _h: &HostClient,
            _p: RenderParams,
        ) -> SdkResult<WireBuffer> {
            Ok(WireBuffer::new(0, 0))
        }
    }

    let h = run_plugin(Bare);
    tokio::time::timeout(Duration::from_secs(2), h.close())
        .await
        .map_err(|_| HarnessError::Assertion("close hung past 2s".into()))?
}

#[tokio::test]
async fn render_result_is_typed_decodable() -> Result<(), HarnessError> {
    // Prove that send_request returns a Value the user can decode into
    // the typed RenderResult from the re-exported wire types.
    let mut h = run_plugin(FixturePlugin);
    h.init().await?;
    let value = h
        .send_request(
            methods::PLUGIN_RENDER,
            RenderParams {
                viewport: Viewport::new(2, 2),
                generation: 0,
            },
        )
        .await?;
    let result: RenderResult = serde_json::from_value(value)?;
    assert_eq!(result.buffer.width, 1);
    h.close().await
}
