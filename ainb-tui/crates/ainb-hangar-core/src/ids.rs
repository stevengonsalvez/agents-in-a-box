//! Strongly-typed id newtypes.
//!
//! Every Hangar entity is addressed by a `TEXT` primary key (a ULID, see
//! [`crate::idgen`]). At the type level we wrap those raw strings in
//! single-purpose newtypes so a [`WorkspaceId`] can never be passed where an
//! [`AgentId`] is expected — the compiler enforces the distinction that the
//! database schema only documents in column names.
//!
//! Each newtype carries exactly one invariant: **the inner string is never
//! empty**. Construction goes through [`from_str`](WorkspaceId::from_str), which
//! rejects the empty string with [`IdError::Empty`]; there is no public way to
//! build an empty id, so downstream code never has to defend against it.

use std::fmt;

/// Failure mode when constructing a typed id newtype.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The supplied id string was empty.
    #[error("id must not be empty")]
    Empty,
}

/// Declare a `String`-backed id newtype with a non-empty invariant.
///
/// The generated type provides:
/// - `from_str(&str) -> Result<Self, IdError>` (inherent, not the
///   [`std::str::FromStr`] trait, so call sites read `Ty::from_str("..")`),
/// - `as_str(&self) -> &str`,
/// - [`fmt::Display`] rendering the raw inner string,
/// - `From<Self> for String`.
macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Construct from a string, rejecting the empty string.
            ///
            /// This is a deliberate inherent fallible constructor (not the
            /// [`std::str::FromStr`] trait) so call sites read
            #[doc = concat!("`", stringify!($name), "::from_str(\"..\")` without importing the trait,")]
            /// and so the signature can accept `impl Into<String>` rather than
            /// only `&str`.
            ///
            /// # Errors
            ///
            /// Returns [`IdError::Empty`] when `s` is empty.
            #[allow(clippy::should_implement_trait)]
            pub fn from_str(s: impl Into<String>) -> Result<Self, IdError> {
                let s = s.into();
                if s.is_empty() {
                    return Err(IdError::Empty);
                }
                Ok(Self(s))
            }

            /// Borrow the (guaranteed non-empty) inner string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

id_newtype! {
    /// Identifies a workspace (the `workspace` table PK).
    WorkspaceId
}
id_newtype! {
    /// Identifies a human user (the `user` table PK).
    UserId
}
id_newtype! {
    /// Identifies an agent definition (the `agent` table PK).
    AgentId
}
id_newtype! {
    /// Identifies an agent runtime registration (the `agent_runtime` table PK).
    RuntimeId
}
id_newtype! {
    /// Identifies an issue (the `issue` table PK).
    IssueId
}
id_newtype! {
    /// Identifies a queued task (the `agent_task_queue` table PK).
    TaskId
}
