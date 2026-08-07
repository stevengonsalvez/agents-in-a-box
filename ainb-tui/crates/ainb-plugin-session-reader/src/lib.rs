//! Canonical local-provider usage reader.
//!
//! The plugin binary publishes this reader's output to TUI consumers. Hangar
//! also uses the same scanner for its public Fleet usage contract, so provider
//! parsing and model-rate logic remain single-sourced.

#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
mod cache;
mod config;
mod fnv;
mod parsers;
pub mod plugin;
pub mod scanner;
