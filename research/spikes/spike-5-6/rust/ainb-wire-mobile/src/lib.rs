//! Spike 5/6 scratch `ainb-wire-mobile`. Not the M1 crate; the shape it tests.

uniffi::setup_scaffolding!();

pub mod client;
#[cfg(feature = "peer")]
pub mod peer;
pub mod wire;
