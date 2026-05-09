//! WASM ABI exports for the host (host_version 1.1.0).
//!
//! Phase 3 minimum viable surface — matches the host_version 1.1.0
//! ABI shape `ainb-plugin-host` calls into:
//!
//! | export             | sig                    | semantics                                   |
//! |--------------------|------------------------|---------------------------------------------|
//! | `_init`            | `() -> i32`            | 0 = success, ≠0 = degrade plugin            |
//! | `_render`          | `() -> i32`            | host calls per frame; plugin paints via     |
//! |                    |                        | `ainb_render_buffer` host-fn (Phase 1.5     |
//! |                    |                        | stub on host side — no-op)                  |
//! | `_handle_event`    | `(i32, i32) -> i32`    | (ptr,len) = msgpack PluginEvent; 0 = ok     |
//! | `_shutdown`        | `() -> ()`             | best-effort cleanup                         |
//! | `_alloc`           | `i32 -> i32`           | host-allocator: returns guest-memory ptr    |
//! |                    |                        | for the host to write event payloads into   |
//!
//! Real rendering / event handling lands when the host wires
//! `ainb_render_buffer` and the WASI runtime needed for SQLite / fs IO
//! (current wasmi setup has no wasi-preview1 — see Phase 3 status).

use core::sync::atomic::{AtomicBool, Ordering};

use ainb_plugin_api::{RenderTarget, WireBuffer, WireCell};
use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::Rect as RRect;

use crate::data::usage::UsageData;
use crate::ui::UsageViewState;

/// Singleton plugin state. Initialised on `_init`; touched on every
/// other call. Wrapped in static-mut + a "ready" flag rather than
/// OnceLock to keep the wasm32-wasip1 binary small (no std::sync
/// thread-locals; the plugin runs single-threaded inside wasmi).
static READY: AtomicBool = AtomicBool::new(false);
static mut STATE: Option<PluginState> = None;

#[derive(Default)]
struct PluginState {
    /// In-memory UI state — populated from host events / cache reads.
    ui: UsageViewState,
    /// Most recent UsageData snapshot the host pushed in via
    /// `_handle_event(UsageDataLoaded)`. `None` until first load.
    data: Option<UsageData>,
}

#[no_mangle]
pub extern "C" fn _init() -> i32 {
    // Construct the in-memory plugin state. No filesystem touched —
    // wasmi 0.40 (the host runtime) has no wasi-preview1 support yet,
    // so any std::fs / std::env call would trap. SQLite migration +
    // cache open are deferred until the host wires WASI; until then
    // the plugin renders against host-provided UsageData snapshots.
    unsafe {
        STATE = Some(PluginState::default());
    }
    READY.store(true, Ordering::Release);
    0
}

#[no_mangle]
pub extern "C" fn _render() -> i32 {
    if !READY.load(Ordering::Acquire) {
        return 1;
    }
    // Snapshot the (UI, data) pair, then construct an offline ratatui
    // Buffer at the host-requested size and let `ui::render` paint into
    // it exactly the way the legacy in-tree screen used to. Convert the
    // resulting Buffer to a WireBuffer + msgpack-encode + hand to the
    // host via `ainb_render_buffer`.
    //
    // Render dimensions: 80x24 default (matches the snapshot baselines).
    // A future PluginHost::render(plugin, area) call signature can carry
    // the real terminal size; for now we keep the size locked so the
    // tripwire stays deterministic.
    let area = RRect { x: 0, y: 0, width: 80, height: 24 };
    let mut rbuf = RBuffer::empty(area);

    // SAFETY: STATE is mutated only inside the four ABI exports, which
    // run single-threaded inside wasmi.
    let snapshot: Option<(UsageViewState, Option<UsageData>)> = unsafe {
        STATE.as_ref().map(|s| (s.ui.clone(), s.data.clone()))
    };
    if let Some((mut ui, data)) = snapshot {
        ui.data = data;
        crate::ui::render(&mut rbuf, area, &ui);
    } else {
        // Pre-init: paint nothing — host receives an empty buffer.
    }

    let wire = buffer_to_wire(&rbuf, area);
    let bytes = match rmp_serde::to_vec_named(&wire) {
        Ok(b) => b,
        Err(_) => return 2,
    };
    let len = match i32::try_from(bytes.len()) {
        Ok(n) => n,
        Err(_) => return 3,
    };
    let ptr = bytes.as_ptr() as i32;
    unsafe {
        host::ainb_render_buffer(RenderTarget::Screen as i32, ptr, len);
    }
    0
}

/// Convert a ratatui Buffer to the on-the-wire WireBuffer the host expects.
///
/// Cells are emitted in row-major order. Colors are mapped down from
/// ratatui's `Color` enum to 8-bit ANSI indices (0xFF = inherit/default)
/// so the wire-format payload stays small. `Reset`/`Indexed`/`Rgb` cover
/// the cases the burndown UI actually emits.
fn buffer_to_wire(rbuf: &RBuffer, area: RRect) -> WireBuffer {
    let mut wire = WireBuffer::empty(area.width, area.height);
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = rbuf.get(area.x + x, area.y + y);
            let i = usize::from(y) * usize::from(area.width) + usize::from(x);
            wire.cells[i] = WireCell {
                symbol: cell.symbol().to_string(),
                fg: color_to_ansi(cell.fg),
                bg: color_to_ansi(cell.bg),
                modifiers: modifiers_to_byte(cell.modifier),
            };
        }
    }
    wire
}

fn color_to_ansi(c: ratatui::style::Color) -> u8 {
    use ratatui::style::Color;
    match c {
        Color::Reset => 0xFF,
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Indexed(i) => i,
        // Truecolor doesn't fit 8-bit; fold to a 6-cube approximation.
        // The byte-identical tripwire only inspects symbols, so the lossy
        // mapping is acceptable here. A future ABI bump can carry RGB.
        Color::Rgb(r, g, b) => 16 + 36 * (r / 51) + 6 * (g / 51) + (b / 51),
    }
}

fn modifiers_to_byte(m: ratatui::style::Modifier) -> u8 {
    use ratatui::style::Modifier;
    let mut out = 0_u8;
    if m.contains(Modifier::BOLD) { out |= 1; }
    if m.contains(Modifier::DIM) { out |= 2; }
    if m.contains(Modifier::ITALIC) { out |= 4; }
    if m.contains(Modifier::UNDERLINED) { out |= 8; }
    if m.contains(Modifier::REVERSED) { out |= 16; }
    out
}

/// Host-fn imports the plugin uses at runtime. `extern "C"` block lives in
/// `ainb_plugin_api::host` for plugin authors but the SDK only exposes them
/// behind `#[cfg(target_arch = "wasm32")]` (so the host build of the SDK
/// keeps compiling). Re-declared here to keep the surface tiny + visible.
#[cfg(target_arch = "wasm32")]
mod host {
    extern "C" {
        pub fn ainb_render_buffer(target: i32, ptr: i32, len: i32);
    }
}
#[cfg(not(target_arch = "wasm32"))]
mod host {
    /// Host-target stub so the plugin's lib still compiles for tests/clippy
    /// run on the build machine. Never actually called — wasm-only.
    pub unsafe fn ainb_render_buffer(_target: i32, _ptr: i32, _len: i32) {}
}

/// Pull the `--format=<text|json|csv>` (or `--format <…>`) global flag
/// out of an argv vector. Stays in sync with the host CLI's
/// `cli::root_clap_command` global flag.
fn extract_format(argv: &[String]) -> crate::output_format::OutputFormat {
    use crate::output_format::OutputFormat;
    let mut iter = argv.iter().peekable();
    while let Some(a) = iter.next() {
        if let Some(rest) = a.strip_prefix("--format=") {
            return parse_format(rest);
        }
        if a == "--format" {
            if let Some(next) = iter.peek() {
                return parse_format(next);
            }
        }
    }
    OutputFormat::default()
}

/// Drop any `--format` / `--format=...` token from argv (and its value
/// when the form is `--format <value>`). The host extracts `--format`
/// out-of-band via `extract_format`; the plugin's clap parser doesn't
/// declare it as a global, so it must not see it.
fn strip_format_flag(argv: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--format" {
            // Skip the flag and its value (if present).
            i += 1;
            if i < argv.len() {
                i += 1;
            }
            continue;
        }
        if a.starts_with("--format=") {
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

fn parse_format(s: &str) -> crate::output_format::OutputFormat {
    use crate::output_format::OutputFormat;
    match s {
        "json" => OutputFormat::Json,
        "csv" => OutputFormat::Csv,
        _ => OutputFormat::Text,
    }
}

#[no_mangle]
pub extern "C" fn _handle_event(ptr: i32, len: i32) -> i32 {
    if !READY.load(Ordering::Acquire) {
        return 1;
    }
    // SAFETY: ptr/len come from the host's `_alloc` + `dispatch_event_bytes`
    // pipeline; the host owns the buffer's lifetime up to and including
    // this call. We read it as a slice for the duration of the call.
    let bytes = unsafe {
        if ptr <= 0 || len <= 0 {
            return 0;
        }
        let p = ptr as usize as *const u8;
        let n = len as usize;
        core::slice::from_raw_parts(p, n)
    };
    let ev: ainb_plugin_api::PluginEvent = match rmp_serde::from_slice(bytes) {
        Ok(e) => e,
        Err(_) => return 0, // bad payload — silent drop, host already logged
    };
    // CLI dispatch: host invokes `ainb usage <subcommand>`, the
    // CommandRegistry layer converts the matched args into a
    // `PluginEvent::Command { name, args }`, and we re-parse `args`
    // through clap on the plugin side. Output flows through stdout
    // (captured by the host's fd_write shim).
    if let ainb_plugin_api::PluginEvent::Command { name, args } = ev {
        if name == "usage" {
            // Synthesise an argv: clap's `try_parse_from` expects argv[0]
            // to be the program name; we use "usage" so subcommands like
            // `report --format=json` parse naturally.
            //
            // `--format` is a host-level global flag. Pull its value out of
            // argv first, then strip it so the plugin's clap (which only
            // knows the subcommand surface) doesn't reject it as unexpected.
            // Reference: reference_plugin_clap_strip_global_flags.
            let mut argv: Vec<String> = vec!["usage".to_string()];
            argv.extend(args);
            let format = extract_format(&argv);
            argv = strip_format_flag(&argv);

            // Phase 6c-cli flow:
            //
            // 1. Test backdoor: when the host pushed a UsageData snapshot
            //    via `Custom { topic: "burndown.usage_data" }` the static
            //    state has it cached. Use that directly so the existing
            //    `plugin_burndown_cli_dispatch.rs` test (which has no
            //    session-reader loaded) keeps passing.
            // 2. Real flow: STATE.data is None — fall through to
            //    `cli::dispatch_for_plugin` which calls request_data on
            //    the session-reader plugin and decodes the wire payload.
            let injected: Option<crate::data::usage::UsageData> =
                unsafe { STATE.as_ref().and_then(|s| s.data.clone()) };
            let result = if let Some(data) = injected {
                use clap::Parser;
                #[derive(Parser)]
                #[command(name = "usage")]
                struct UsageRoot {
                    #[command(subcommand)]
                    cmd: crate::cli::UsageCommands,
                }
                match UsageRoot::try_parse_from(argv) {
                    Ok(parsed) => crate::cli::execute_for_plugin(&data, parsed.cmd, format),
                    Err(e) => {
                        eprintln!("{e}");
                        return 0;
                    }
                }
            } else {
                crate::cli::dispatch_for_plugin(argv, format)
            };
            if let Err(e) = result {
                eprintln!("{e}");
                return 0;
            }
        }
        return 0;
    }
    if let ainb_plugin_api::PluginEvent::Custom { topic, payload } = ev {
        match topic.as_str() {
            "burndown.usage_data" => {
                if let Ok(data) = serde_json::from_value::<UsageData>(payload) {
                    unsafe {
                        if let Some(state) = STATE.as_mut() {
                            state.data = Some(data);
                        }
                    }
                }
            }
            "burndown.set_tab" => {
                // Payload: {"tab":"daily"|"weekly"|"projects"|"burndown"|"optimize"}
                let tab_name = payload.get("tab").and_then(|v| v.as_str()).unwrap_or("");
                let tab = match tab_name {
                    "daily" => Some(crate::ui::UsageTab::Daily),
                    "weekly" => Some(crate::ui::UsageTab::Weekly),
                    "projects" => Some(crate::ui::UsageTab::Projects),
                    "burndown" => Some(crate::ui::UsageTab::Burndown),
                    "optimize" => Some(crate::ui::UsageTab::Optimize),
                    _ => None,
                };
                if let Some(t) = tab {
                    unsafe {
                        if let Some(state) = STATE.as_mut() {
                            state.ui.active_tab = t;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    0
}

#[no_mangle]
pub extern "C" fn _shutdown() {
    READY.store(false, Ordering::Release);
    unsafe {
        STATE = None;
    }
}

/// Host-allocator: gives the host a buffer in guest memory it can
/// write event payloads into before calling `_handle_event(ptr, len)`.
///
/// `Box::leak` is intentional — the host owns the lifetime; the plugin
/// receives the pointer back through `_handle_event` and must drop it
/// (today: implicit via the pre-allocated arena once we add one).
#[no_mangle]
pub extern "C" fn _alloc(size: i32) -> i32 {
    let n = match usize::try_from(size) {
        Ok(n) if n > 0 => n,
        _ => return 0,
    };
    let buf = vec![0_u8; n].into_boxed_slice();
    let ptr = Box::into_raw(buf) as *mut u8 as i32;
    ptr
}
