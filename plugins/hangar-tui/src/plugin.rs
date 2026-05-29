//! ABI v2 [`Plugin`] implementation for the Hangar control plane TUI.
//!
//! P3.7 wires the daemon connection into the plugin. On `plugin/init` the
//! plugin dials the daemon socket through the host `unix_socket_dial` cap,
//! sends a `workspace/subscribe` request framed over that cap stream, and
//! moves a [`Connection`] state machine `Disconnected → Dialing →
//! Handshake → Connected`. Inbound daemon frames arrive as
//! `socket:<stream_id>` `plugin/handle_event` notifications; the plugin
//! reassembles them with a [`FrameDecoder`] and acks the subscribe to
//! reach [`ConnState::Connected`].
//!
//! `render` paints a colour-coded footer on the last viewport row:
//! `"Hangar: <state>"` — gold when Connected, muted-gray while Dialing /
//! Handshaking, red when Disconnected / Error. The body is left blank
//! (P4 fills it with the Core 5 screens).
//!
//! Everything declared in `manifest.toml` — the four P3 host caps — is
//! requested here; this phase exercises `unix_socket_dial` +
//! `unix_socket_send`. `spawn_managed_subprocess` (auto-starting the
//! daemon) and `secret_store_get` land in later phases.

use ainb_hangar_proto::{methods as daemon_methods, RpcId};
use ainb_plugin_sdk::{
    Cell, CliOutput, Color, Coord, HandleEventParams, HostClient, Plugin, RenderParams, Result,
    RpcError, UnixSocketEvent, UnixSocketEventKind, WireBuffer,
};
use async_trait::async_trait;

use crate::connection::{ConnState, Connection, DEFAULT_WORKSPACE_ID};
use crate::jsonrpc_over_socket::{encode_request, FrameDecoder};

/// Static manifest TOML loaded at compile time. The [`Server`] uses
/// this on `plugin/init` to echo `name`/`version` back to the host so
/// spawn-vs-manifest can be cross-checked.
///
/// [`Server`]: ainb_plugin_sdk::Server
pub const MANIFEST_TOML: &str = include_str!("../manifest.toml");

/// Fallback render viewport used only if a degenerate `0×0` ever
/// arrives in [`RenderParams`]. The host normally sends an explicit
/// viewport.
const FALLBACK_VIEWPORT: (u16, u16) = (1, 1);

/// The daemon socket path the plugin dials. The host `unix_socket_dial`
/// cap expands `~` and canonicalizes before checking the manifest
/// whitelist (which lists this exact path).
const DAEMON_SOCKET_PATH: &str = "~/.ainb/hangar.sock";

/// JSON-RPC id the plugin assigns to its `workspace/subscribe` request.
/// A single id is enough: the plugin issues exactly one subscribe per
/// connection, and matching the ack by id avoids treating an unrelated
/// reply as the handshake completion.
const SUBSCRIBE_REQ_ID: i64 = 1;

// Footer colours, mirroring the ainb TUI palette.
const GOLD: Color = Color::rgb(255, 215, 0);
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
const RED: Color = Color::rgb(220, 80, 80);

/// Hangar plugin state.
///
/// Holds the daemon [`Connection`] state machine and the inbound socket
/// [`FrameDecoder`]. The SDK serialises handler access behind a mutex, so
/// `&mut self` mutation here is race-free.
#[derive(Debug, Default)]
pub struct HangarPlugin {
    conn: Connection,
    decoder: FrameDecoder,
}

impl HangarPlugin {
    /// Construct a fresh, disconnected plugin.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Footer colour for the current connection state.
    const fn footer_color(state: &ConnState) -> Color {
        match state {
            ConnState::Connected => GOLD,
            ConnState::Dialing | ConnState::Handshake => MUTED_GRAY,
            ConnState::Disconnected | ConnState::Error(_) => RED,
        }
    }

    /// Dial the daemon and send the workspace subscribe. Records the
    /// resulting [`ConnState`] on `self`; transport failures land the
    /// machine in [`ConnState::Error`] (rendered Red) rather than
    /// propagating, so a downed daemon shows a clean footer instead of
    /// crashing the plugin.
    async fn connect(&mut self, host: &HostClient) {
        self.decoder = FrameDecoder::new();
        self.conn.dialing();

        let dial = match host.unix_socket_dial(DAEMON_SOCKET_PATH).await {
            Ok(r) => r,
            Err(e) => {
                self.conn.on_error(format!("dial failed: {e}"));
                let _ = host.log_info(format!("hangar: dial failed: {e}")).await;
                return;
            }
        };
        self.conn.on_dialed(dial.stream_id.clone());

        // Frame the workspace/subscribe request and write it to the cap stream.
        let body = match encode_request(
            SUBSCRIBE_REQ_ID,
            daemon_methods::WORKSPACE_SUBSCRIBE,
            serde_json::json!({ "workspace_id": DEFAULT_WORKSPACE_ID }),
        ) {
            Ok(b) => b,
            Err(e) => {
                self.conn.on_error(format!("encode subscribe failed: {e}"));
                return;
            }
        };
        if let Err(e) = host.unix_socket_send(dial.stream_id, body).await {
            self.conn.on_error(format!("send subscribe failed: {e}"));
            return;
        }
        let _ = host
            .log_info("hangar: dialed daemon, subscribe sent")
            .await;
    }

    /// Feed an inbound `socket:<stream_id>` event into the connection.
    ///
    /// Decodes the [`UnixSocketEvent`] envelope: `Data` bytes are pushed
    /// to the [`FrameDecoder`] and each complete daemon response is
    /// matched against the subscribe id to ack the handshake; `Eof`
    /// drops to `Disconnected`; `Error` lands in `Error`.
    fn on_socket_event(&mut self, event: &UnixSocketEvent) {
        match event.kind {
            UnixSocketEventKind::Data => {
                let Some(bytes) = event.bytes.as_ref() else {
                    return;
                };
                match self.decoder.push(bytes) {
                    Ok(responses) => {
                        for resp in responses {
                            self.on_daemon_response(&resp);
                        }
                    }
                    Err(e) => self.conn.on_error(format!("frame decode: {e}")),
                }
            }
            UnixSocketEventKind::Eof => self.conn.on_eof(),
            UnixSocketEventKind::Error => {
                let msg = event.error.clone().unwrap_or_else(|| "socket error".into());
                self.conn.on_error(msg);
            }
        }
    }

    /// React to one fully-decoded daemon response.
    fn on_daemon_response(&mut self, resp: &ainb_hangar_proto::RpcResponse) {
        // The subscribe ack completes the handshake.
        if resp.id == RpcId::Number(SUBSCRIBE_REQ_ID) {
            if resp.error.is_some() {
                self.conn
                    .on_error("daemon rejected workspace/subscribe".to_string());
            } else {
                self.conn.on_subscribe_ack();
            }
        } else {
            // A post-subscribe workspace event: keep the link alive.
            self.conn.on_event();
        }
    }
}

#[async_trait]
impl Plugin for HangarPlugin {
    fn manifest(&self) -> &'static str {
        MANIFEST_TOML
    }

    async fn on_init(&mut self, host: &HostClient, _granted: &[String]) -> Result<()> {
        self.connect(host).await;
        Ok(())
    }

    async fn handle_event(&mut self, _host: &HostClient, params: HandleEventParams) -> Result<()> {
        // Only socket:<stream_id> deliveries for our current stream concern us.
        let want = self
            .conn
            .stream_id()
            .map(|id| format!("socket:{id}"));
        if want.as_deref() != Some(params.topic.as_str()) {
            return Ok(());
        }
        match serde_json::from_slice::<UnixSocketEvent>(&params.payload) {
            Ok(event) => self.on_socket_event(&event),
            Err(e) => self.conn.on_error(format!("bad socket event: {e}")),
        }
        Ok(())
    }

    async fn render(&mut self, _host: &HostClient, params: RenderParams) -> Result<WireBuffer> {
        let (w, h) = if params.viewport.width == 0 || params.viewport.height == 0 {
            FALLBACK_VIEWPORT
        } else {
            (params.viewport.width, params.viewport.height)
        };
        let mut buf = WireBuffer::new(w, h);

        // Body stays blank (P4 fills it). Paint the footer on the last row.
        let footer = format!("Hangar: {}", self.conn.state().label());
        let color = Self::footer_color(self.conn.state());
        let row = h.saturating_sub(1);
        for (i, ch) in footer.chars().enumerate() {
            let Ok(x) = u16::try_from(i) else { break };
            if x >= w {
                break;
            }
            let mut cell = Cell::new(ch.to_string());
            cell.fg = Some(color);
            buf.push(Coord::new(x, row), cell);
        }
        Ok(buf)
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        namespace: &str,
        _argv: &[String],
    ) -> Result<CliOutput> {
        Err(RpcError::not_implemented(format!(
            "hangar CLI namespace `{namespace}` not implemented (scaffold; lands in P4)"
        ))
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_plugin_protocol::manifest::{CapabilityGrant, Manifest};

    #[test]
    fn manifest_returns_canonical_toml() {
        let p = HangarPlugin::new();
        assert_eq!(p.manifest(), MANIFEST_TOML);
        assert!(p.manifest().contains("name = \"hangar-tui\""));
    }

    #[test]
    fn manifest_parses_and_round_trips_all_four_caps() {
        let m: Manifest = toml::from_str(MANIFEST_TOML).expect("manifest parses");
        assert_eq!(m.plugin.name, "hangar-tui");
        assert_eq!(m.plugin.abi_version, 2);

        assert_eq!(
            m.capabilities.event_stream_subscribe.allow_list().unwrap(),
            ["workspace:*", "stream:*", "managed:*", "socket:*"]
        );
        assert_eq!(
            m.capabilities.spawn_managed_subprocess.allow_list().unwrap(),
            ["ainb-hangar-daemon"]
        );
        assert_eq!(
            m.capabilities.unix_socket_dial.allow_list().unwrap(),
            ["~/.ainb/hangar.sock", "${XDG_RUNTIME_DIR}/ainb-hangar.sock"]
        );
        assert_eq!(
            m.capabilities.secret_store_get.allow_list().unwrap(),
            ["ainb-hangar", "anthropic-api-key", "openai-api-key"]
        );

        let s = toml::to_string(&m).expect("serialize");
        let back: Manifest = toml::from_str(&s).expect("re-parse");
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_lifecycle_is_lazy_no_reap() {
        let m: Manifest = toml::from_str(MANIFEST_TOML).unwrap();
        assert_eq!(
            m.lifecycle.spawn,
            ainb_plugin_protocol::manifest::SpawnMode::Lazy
        );
        assert_eq!(m.lifecycle.idle_reap_secs, 0);
    }

    #[test]
    fn manifest_provides_hangar_surface() {
        let m: Manifest = toml::from_str(MANIFEST_TOML).unwrap();
        assert_eq!(m.provides.screens, ["hangar"]);
        assert_eq!(m.provides.commands, ["/hangar"]);
        assert_eq!(m.provides.cli_namespaces, ["hangar"]);
    }

    #[test]
    fn manifest_grants_event_bus_and_plugin_data() {
        let m: Manifest = toml::from_str(MANIFEST_TOML).unwrap();
        assert!(matches!(
            m.capabilities.event_bus,
            CapabilityGrant::Bool(true)
        ));
        assert!(matches!(
            m.capabilities.write_plugin_data,
            CapabilityGrant::Bool(true)
        ));
    }

    /// The footer colour is Gold only when Connected, Red when down.
    #[test]
    fn footer_colors_map_state() {
        assert_eq!(HangarPlugin::footer_color(&ConnState::Connected), GOLD);
        assert_eq!(HangarPlugin::footer_color(&ConnState::Dialing), MUTED_GRAY);
        assert_eq!(HangarPlugin::footer_color(&ConnState::Handshake), MUTED_GRAY);
        assert_eq!(HangarPlugin::footer_color(&ConnState::Disconnected), RED);
        assert_eq!(
            HangarPlugin::footer_color(&ConnState::Error("x".into())),
            RED
        );
    }

    /// A decoded subscribe ack drives the in-memory state machine to
    /// Connected without any socket — proves `on_daemon_response` wiring.
    #[test]
    fn subscribe_ack_response_reaches_connected() {
        let mut p = HangarPlugin::new();
        // Simulate the dial path's state advance.
        p.conn.dialing();
        p.conn.on_dialed("s1");
        let resp = ainb_hangar_proto::RpcResponse {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(SUBSCRIBE_REQ_ID),
            result: Some(serde_json::json!({})),
            error: None,
        };
        p.on_daemon_response(&resp);
        assert!(p.conn.is_connected());
    }

    /// An error envelope on the subscribe id fails the handshake.
    #[test]
    fn subscribe_error_response_fails_handshake() {
        let mut p = HangarPlugin::new();
        p.conn.dialing();
        p.conn.on_dialed("s1");
        let resp = ainb_hangar_proto::RpcResponse {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(SUBSCRIBE_REQ_ID),
            result: None,
            error: Some(ainb_hangar_proto::RpcError {
                code: -32601,
                message: "no such method".into(),
                data: None,
            }),
        };
        p.on_daemon_response(&resp);
        assert!(matches!(p.conn.state(), ConnState::Error(_)));
    }

    /// An EOF socket event drops a connected link back to Disconnected.
    #[test]
    fn socket_eof_disconnects() {
        let mut p = HangarPlugin::new();
        p.conn.dialing();
        p.conn.on_dialed("s1");
        p.conn.on_subscribe_ack();
        assert!(p.conn.is_connected());
        let eof = UnixSocketEvent {
            kind: UnixSocketEventKind::Eof,
            bytes: None,
            error: None,
        };
        p.on_socket_event(&eof);
        assert_eq!(*p.conn.state(), ConnState::Disconnected);
    }
}
