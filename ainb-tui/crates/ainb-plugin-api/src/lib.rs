//! Public SDK for ainb plugins.
//!
//! Plugins import this crate (`ainb-plugin-api`) and implement the [`Plugin`] trait.
//! At build time, plugins compile to `wasm32-wasip1` `cdylib`s and are loaded by
//! the host (`ainb-plugin-host`) via the WASM ABI declared in [`host`].
//!
//! On the host target, this crate provides the manifest, capability, and wire-type
//! schemas so the host can validate manifests, enforce capabilities at link time,
//! and round-trip render buffers / events with plugins via msgpack.

#![allow(clippy::module_name_repetitions)]

pub mod capabilities;
pub mod events;
pub mod host;
pub mod manifest;
pub mod plugin;
pub mod render;

pub use capabilities::{Capability, CapabilitySet};
pub use events::PluginEvent;
pub use manifest::{Manifest, PathsTable, SubscribesTable};
pub use plugin::{Plugin, PluginContext, PluginError, PluginResult};
pub use render::{RenderTarget, Rect, WireBuffer, WireCell};
