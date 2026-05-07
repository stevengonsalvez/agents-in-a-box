//! End-to-end test for the real `ainb_render_buffer` host-fn.
//!
//! Builds a tiny wat plugin whose `_render` export hands the host a known
//! msgpack-encoded `WireBuffer`. After `_render` returns, we assert that
//! `PluginHost::take_render(...)` returns the same buffer the plugin sent.

use ainb_plugin_api::manifest::{CapabilitiesTable, PluginTable, ProvidesTable};
use ainb_plugin_api::{Manifest, RenderTarget, WireBuffer, WireCell};
use ainb_plugin_host::PluginHost;

fn render_target_manifest() -> Manifest {
    Manifest {
        plugin: PluginTable {
            name: "render_canary".to_string(),
            version: "0.1.0".to_string(),
            ainb_min_version: "1.0.0".to_string(),
            description: None,
            author: None,
            license: None,
            homepage: None,
        },
        capabilities: CapabilitiesTable::default(),
        provides: ProvidesTable::default(),
    }
}

#[test]
fn render_buffer_decodes_and_stashes_payload() {
    // Plugin will paint a 2x1 buffer with a single styled cell.
    let mut expected = WireBuffer::empty(2, 1);
    expected.cells[0] = WireCell {
        symbol: "X".to_string(),
        fg: 1,
        bg: 2,
        modifiers: 1,
    };
    let payload = rmp_serde::to_vec_named(&expected).expect("encode WireBuffer");
    let payload_len = payload.len();

    // Embed the msgpack bytes as a wat data segment, then have `_render`
    // call ainb_render_buffer with target=Screen (0), ptr=0, len=payload_len.
    let data_literal: String = payload
        .iter()
        .map(|b| format!("\\{b:02x}"))
        .collect();
    let wat = format!(
        r#"
(module
  (import "env" "ainb_render_buffer"
    (func $render (param i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{data}")
  (func (export "_init") (result i32) i32.const 0)
  (func (export "_render") (result i32)
    i32.const 0       ;; target = RenderTarget::Screen
    i32.const 0       ;; ptr
    i32.const {len}   ;; len
    call $render
    i32.const 0)
  (func (export "_shutdown")))
"#,
        data = data_literal,
        len = payload_len
    );
    let wasm = wat::parse_str(&wat).expect("compile wat fixture");

    let mut host = PluginHost::new();
    let id = host
        .load_bytes(render_target_manifest(), &wasm)
        .expect("plugin loads");
    assert_eq!(id.0, "render_canary");

    // No buffer painted yet — _init didn't call render_buffer.
    assert!(host.take_render("render_canary", RenderTarget::Screen).is_none());

    host.render_plugin("render_canary").expect("_render runs");

    let got = host
        .take_render("render_canary", RenderTarget::Screen)
        .expect("plugin painted to Screen target");
    assert_eq!(got, expected, "host decoded the WireBuffer faithfully");

    // Second take returns None (buffer is consumed on take).
    assert!(host.take_render("render_canary", RenderTarget::Screen).is_none());
}
