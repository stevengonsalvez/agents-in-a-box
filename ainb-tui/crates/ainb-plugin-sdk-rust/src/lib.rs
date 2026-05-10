//! Rust SDK for ainb plugins.
//!
//! Plugin authors implement the [`Plugin`] trait, then call
//! [`Server::new(plugin).run_stdio()`] from `#[tokio::main]`. The SDK
//! handles Content-Length stdio framing, JSON-RPC method dispatch,
//! error mapping, and the reverse-call channel back to the host
//! ([`HostClient`]).
//!
//! ## Module map
//!
//! - [`error`]       — SDK [`SdkError`] and the [`Result`] alias
//! - [`plugin`]      — the [`Plugin`] trait every plugin implements
//! - [`host_client`] — [`HostClient`] is the plugin's outbound JSON-RPC client
//! - [`server`]      — [`Server`] runs the dispatcher loop over an [`AsyncRead`]/[`AsyncWrite`] pair
//!
//! ## Wire types
//!
//! Wire types are re-exported from [`ainb_plugin_protocol`] — the SDK
//! never duplicates them.
//!
//! [`AsyncRead`]: tokio::io::AsyncRead
//! [`AsyncWrite`]: tokio::io::AsyncWrite

// Module declarations are added in subsequent commits as each piece lands.
