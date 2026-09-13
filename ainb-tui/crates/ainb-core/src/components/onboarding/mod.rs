// ABOUTME: Onboarding wizard module for first-time setup
// Guides users through dependency checks, git configuration, and authentication.
// The wizard state lives in `ainb_app::components::onboarding`.

pub use ainb_app::components::onboarding::*;

pub mod component;

pub use component::OnboardingComponent;
