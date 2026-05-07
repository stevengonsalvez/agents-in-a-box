// ABOUTME: Onboarding wizard module for first-time setup
// Guides users through dependency checks, git configuration, and authentication

pub mod component;
pub mod dependency_checker;
pub mod state;

pub use component::OnboardingComponent;
pub use dependency_checker::{Dependency, DependencyCategory, DependencyChecker, DependencyCheckResult, DependencyStatus};
pub use state::{OnboardingFocus, OnboardingState, OnboardingStep, ValidatedPath};
