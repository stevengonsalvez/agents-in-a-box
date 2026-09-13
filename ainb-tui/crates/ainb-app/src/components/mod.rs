// ABOUTME: Renderer-agnostic halves of the screen components: the state types,
// their impls and the reducers that do not draw. The draw functions stay in
// `ainb-core::components`, which re-exports this module.

pub mod onboarding;
pub mod setup_menu;
pub mod skills;
