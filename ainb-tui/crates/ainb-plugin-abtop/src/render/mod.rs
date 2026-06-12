//! Render paths for the abtop plugin.
//!
//! Just the [`empty`] module — the "abtop missing" install-hint
//! empty-state plus the "abtop installed" ready hint. There are no
//! per-tab data painters: abtop has no machine-readable mode, and the
//! live agent view is a host-side tmux full-screen attach, not an
//! in-plugin surface.
//!
//! Painters write directly into the protocol's [`WireBuffer`] via
//! absolute-coord cell pushes rather than going through `ratatui` — the
//! surfaces are a handful of lines of static text.

pub mod empty;

#[cfg(test)]
pub(crate) mod test_support;
