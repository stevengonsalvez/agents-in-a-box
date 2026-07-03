// ABOUTME: Onboarding wizard module for first-time setup
// Guides users through dependency checks, git configuration, and authentication

pub mod component;
pub mod state;

pub use component::OnboardingComponent;
pub use state::{
    AgentAuthStatus, AuthAgent, AuthMethodKind, AuthPane, OnboardingFocus, OnboardingState,
    OnboardingStep, ValidatedPath,
};
