//! `/witr <target>` slash-command parser.
//!
//! The host owns `/witr` (declared in `manifest.toml` `[provides]
//! commands`). When the user types `/witr 1234` in an ainb input box,
//! the host routes it to this plugin; [`parse_slash`] turns the raw
//! input line into a validated target, which
//! [`crate::plugin::WitrPlugin::handle_slash`] uses to set the current
//! target + open the detail overlay.
//!
//! Pure parser — no I/O — so the command contract is unit-testable in
//! isolation from the host's slash-delivery mechanism.

use crate::exec::{TargetValidationError, WitrTarget, validate_target};

/// Why a `/witr` line failed to parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SlashError {
    /// The line wasn't a `/witr` (or `witr`) command at all.
    #[error("not a /witr command")]
    NotWitrCommand,
    /// `/witr` with no target argument.
    #[error("/witr needs a target: /witr <name|pid:N|port:N|file:P|container:C>")]
    NoTarget,
    /// The target argument failed validation.
    #[error("invalid target: {0}")]
    InvalidTarget(#[from] TargetValidationError),
}

/// Parse a raw input line into a typed [`WitrTarget`].
///
/// Accepts both the slash form (`/witr nginx`) and the bare form
/// (`witr nginx`) so the parser is reusable whether or not the host
/// strips the leading slash. Extra whitespace is tolerated; only the
/// first whitespace-delimited token after the command is taken as the
/// target (witr targets are single tokens — a name, `pid:N`,
/// `port:N`, `file:P`, or `container:C` — never multi-word).
///
/// The token's kind is decoded via [`WitrTarget::parse`]: a `pid:` /
/// `port:` / `file:` / `container:` prefix picks the kind, anything
/// else is a process name. Validation runs on the underlying value.
pub fn parse_slash(input: &str) -> Result<WitrTarget, SlashError> {
    let line = input.trim();
    let rest = line
        .strip_prefix("/witr")
        .or_else(|| line.strip_prefix("witr"))
        .ok_or(SlashError::NotWitrCommand)?;

    // After the command keyword there must be whitespace then a token
    // (guards against matching `/witrfoo`).
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return Err(SlashError::NotWitrCommand);
    }

    let token = rest.split_whitespace().next().ok_or(SlashError::NoTarget)?;
    let target = WitrTarget::parse(token);
    validate_target(target.value())?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_slash_form_as_name() {
        assert_eq!(
            parse_slash("/witr nginx").expect("parses"),
            WitrTarget::Name("nginx".into())
        );
    }

    #[test]
    fn parses_bare_form() {
        assert_eq!(
            parse_slash("witr nginx").expect("parses"),
            WitrTarget::Name("nginx".into())
        );
    }

    #[test]
    fn parses_typed_prefixes() {
        assert_eq!(
            parse_slash("/witr pid:1234").expect("parses"),
            WitrTarget::Pid("1234".into())
        );
        assert_eq!(
            parse_slash("/witr port:5432").expect("parses"),
            WitrTarget::Port("5432".into())
        );
        assert_eq!(
            parse_slash("/witr container:redis").expect("parses"),
            WitrTarget::Container("redis".into())
        );
    }

    #[test]
    fn tolerates_surrounding_whitespace() {
        assert_eq!(
            parse_slash("   /witr    nginx   ").expect("parses"),
            WitrTarget::Name("nginx".into())
        );
    }

    #[test]
    fn takes_only_first_token_as_target() {
        assert_eq!(
            parse_slash("/witr nginx extra junk").expect("parses"),
            WitrTarget::Name("nginx".into())
        );
    }

    #[test]
    fn rejects_non_witr_command() {
        assert_eq!(
            parse_slash("/usage 1").unwrap_err(),
            SlashError::NotWitrCommand
        );
        assert_eq!(
            parse_slash("hello").unwrap_err(),
            SlashError::NotWitrCommand
        );
    }

    #[test]
    fn rejects_witr_prefix_without_word_boundary() {
        // `/witrfoo` must not match — it's not the /witr command.
        assert_eq!(
            parse_slash("/witrfoo 1").unwrap_err(),
            SlashError::NotWitrCommand
        );
    }

    #[test]
    fn rejects_missing_target() {
        assert_eq!(parse_slash("/witr").unwrap_err(), SlashError::NoTarget);
        assert_eq!(parse_slash("/witr   ").unwrap_err(), SlashError::NoTarget);
    }

    #[test]
    fn rejects_invalid_target() {
        // Newline in the token can't actually survive split_whitespace,
        // but an over-long token trips validate_target.
        let long = format!("/witr {}", "x".repeat(300));
        assert!(matches!(
            parse_slash(&long).unwrap_err(),
            SlashError::InvalidTarget(_)
        ));
    }
}
