// ABOUTME: Surface-agnostic first-time-setup engine shared by the TUI onboarding
// wizard and the `ainb init` CLI. Holds the single dependency catalog, host
// detection, and (in later phases) provisioning + the shared wizard flow, so the
// check, the installer and the docs never drift between the two surfaces.

pub mod catalog;
pub mod detect;
pub mod provision;

pub use catalog::{Consumer, Dep, Detect, Install, Platform, Tier, Topic, catalog};
pub use detect::{
    DepReport, DepState, Env, RealEnv, SetupStatus, TopicReport, detect_all, detect_dep,
};
pub use provision::{
    ConsentLevel, ProvisionMode, ProvisionOutcome, install_tmux_config, provision,
};
