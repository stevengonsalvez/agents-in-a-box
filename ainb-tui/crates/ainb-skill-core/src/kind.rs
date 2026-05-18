//! Unit-kind enum shared across source/tool adapters and lockfile.
//!
//! String representation matches the wire format used in manifests,
//! lockfiles, and CLI output — `skill`, `plugin`, `agent`, `command`,
//! `hook`, `mcp-server`, `statusline`. The enum is the canonical
//! representation for code that needs exhaustive matching (e.g. tool
//! adapters deciding `accepts()`); the string form is preserved on
//! the wire for forward compat.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum UnitKind {
    Skill,
    Plugin,
    Agent,
    Command,
    Hook,
    McpServer,
    Statusline,
}

impl UnitKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            UnitKind::Skill => "skill",
            UnitKind::Plugin => "plugin",
            UnitKind::Agent => "agent",
            UnitKind::Command => "command",
            UnitKind::Hook => "hook",
            UnitKind::McpServer => "mcp-server",
            UnitKind::Statusline => "statusline",
        }
    }

    pub const fn all() -> [UnitKind; 7] {
        [
            UnitKind::Skill,
            UnitKind::Plugin,
            UnitKind::Agent,
            UnitKind::Command,
            UnitKind::Hook,
            UnitKind::McpServer,
            UnitKind::Statusline,
        ]
    }
}

impl fmt::Display for UnitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for UnitKind {
    type Err = crate::CoreError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "skill" => Ok(Self::Skill),
            "plugin" => Ok(Self::Plugin),
            "agent" => Ok(Self::Agent),
            "command" => Ok(Self::Command),
            "hook" => Ok(Self::Hook),
            "mcp-server" | "mcp_server" => Ok(Self::McpServer),
            "statusline" => Ok(Self::Statusline),
            other => Err(crate::CoreError::InvalidUri(format!(
                "unknown unit kind `{other}`"
            ))),
        }
    }
}

impl TryFrom<String> for UnitKind {
    type Error = crate::CoreError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<UnitKind> for String {
    fn from(value: UnitKind) -> Self {
        value.as_str().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_all() {
        for k in UnitKind::all() {
            let s = k.as_str();
            assert_eq!(s.parse::<UnitKind>().unwrap(), k);
            assert_eq!(k.to_string(), s);
        }
    }

    #[test]
    fn rejects_unknown() {
        assert!("nope".parse::<UnitKind>().is_err());
    }

    #[test]
    fn accepts_legacy_underscore_for_mcp() {
        assert_eq!(
            "mcp_server".parse::<UnitKind>().unwrap(),
            UnitKind::McpServer
        );
    }
}
