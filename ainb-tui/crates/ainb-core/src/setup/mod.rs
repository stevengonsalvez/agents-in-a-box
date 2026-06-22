// ABOUTME: Surface-agnostic first-time-setup engine shared by the TUI onboarding
// wizard and the `ainb init` CLI. Holds the single dependency catalog, host
// detection, and (in later phases) provisioning + the shared wizard flow, so the
// check, the installer and the docs never drift between the two surfaces.

pub mod catalog;
pub mod detect;
pub mod provision;

pub use catalog::{catalog, Consumer, Dep, Detect, Install, Platform, Tier, Topic};
pub use detect::{
    detect_all, DepReport, DepState, Env, RealEnv, SetupStatus, TopicReport,
};
pub use provision::{provision, ConsentLevel, ProvisionMode, ProvisionOutcome};
