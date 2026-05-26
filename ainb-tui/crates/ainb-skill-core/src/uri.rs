//! Unit URI grammar (spec §6.1).
//!
//! ```text
//! unit-uri    ::= <source-type> ":" <source-locator> "@" <ref> "/" <path>
//! source-type ::= "gh" | "git" | "gist" | "https" | "local" | "npm" | "marketplace"
//! ```
//!
//! Parser is permissive on the shape: any of
//! `type:locator`, `type:locator@ref`, `type:locator@ref/path` round-trips
//! through [`Uri::parse`] + [`Uri::display`]. Use [`Uri::is_unit`] to
//! check whether both `ref` and `path` are present (full unit URI), or
//! [`Uri::is_source`] for source-only URIs as stored in manifests.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// Supported source-type prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceType {
    Gh,
    Git,
    Gist,
    Https,
    Local,
    Npm,
    Marketplace,
}

impl SourceType {
    pub const fn as_str(self) -> &'static str {
        match self {
            SourceType::Gh => "gh",
            SourceType::Git => "git",
            SourceType::Gist => "gist",
            SourceType::Https => "https",
            SourceType::Local => "local",
            SourceType::Npm => "npm",
            SourceType::Marketplace => "marketplace",
        }
    }
}

impl FromStr for SourceType {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "gh" => Ok(Self::Gh),
            "git" => Ok(Self::Git),
            "gist" => Ok(Self::Gist),
            "https" => Ok(Self::Https),
            "local" => Ok(Self::Local),
            "npm" => Ok(Self::Npm),
            "marketplace" => Ok(Self::Marketplace),
            other => Err(CoreError::UnknownSourceType(other.to_string())),
        }
    }
}

impl fmt::Display for SourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parsed URI. Holds the four components from the grammar; `ref` and
/// `path` are optional so source-only URIs (no `@ref/path`) can also be
/// represented losslessly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Uri {
    pub source_type: SourceType,
    pub locator: String,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub path: Option<String>,
}

impl Uri {
    /// Parse a URI string, returning [`CoreError::InvalidUri`] or
    /// [`CoreError::UnknownSourceType`] on rejection.
    pub fn parse(input: &str) -> Result<Self> {
        if input.is_empty() {
            return Err(CoreError::InvalidUri("empty input".to_string()));
        }

        let (type_str, rest) = input
            .split_once(':')
            .ok_or_else(|| CoreError::InvalidUri(format!("missing ':' in `{input}`")))?;

        if type_str.is_empty() {
            return Err(CoreError::InvalidUri(format!(
                "empty source type in `{input}`"
            )));
        }

        let source_type: SourceType = type_str.parse()?;

        if rest.is_empty() {
            return Err(CoreError::InvalidUri(format!(
                "empty locator after `{type_str}:`"
            )));
        }

        // Split locator from `@ref/path` on the LAST `@` so URLs with
        // `user@host` in the locator parse correctly. If there is no
        // `@`, the whole `rest` is the locator (source-only URI).
        let (locator, ref_path) = match rest.rsplit_once('@') {
            Some((loc, rp)) => (loc, Some(rp)),
            None => (rest, None),
        };

        if locator.is_empty() {
            return Err(CoreError::InvalidUri(format!("empty locator in `{input}`")));
        }

        let (ref_, path) = match ref_path {
            None => (None, None),
            Some("") => {
                return Err(CoreError::InvalidUri(format!(
                    "empty ref after '@' in `{input}`"
                )));
            }
            Some(rp) => match rp.split_once('/') {
                Some((r, p)) => {
                    if r.is_empty() {
                        return Err(CoreError::InvalidUri(format!(
                            "empty ref before '/' in `{input}`"
                        )));
                    }
                    if p.is_empty() {
                        return Err(CoreError::InvalidUri(format!(
                            "empty path after ref in `{input}`"
                        )));
                    }
                    (Some(r.to_string()), Some(p.to_string()))
                }
                None => (Some(rp.to_string()), None),
            },
        };

        Ok(Uri {
            source_type,
            locator: locator.to_string(),
            ref_,
            path,
        })
    }

    /// Render the URI back to its canonical string form. Round-trips
    /// any URI produced by [`Uri::parse`].
    pub fn display(&self) -> String {
        let mut out = format!("{}:{}", self.source_type, self.locator);
        if let Some(r) = &self.ref_ {
            out.push('@');
            out.push_str(r);
            if let Some(p) = &self.path {
                out.push('/');
                out.push_str(p);
            }
        }
        out
    }

    /// True when both `ref` and `path` are set (i.e. a full unit URI).
    pub fn is_unit(&self) -> bool {
        self.ref_.is_some() && self.path.is_some()
    }

    /// True when there is no path component (i.e. a source-only URI as
    /// stored in `manifest.yaml`). Ref may still be present.
    pub fn is_source(&self) -> bool {
        self.path.is_none()
    }

    /// Construct a `marketplace:<plugin>@<marketplace>[@<version>]` URI.
    ///
    /// The unversioned form maps to `locator=plugin, ref_=marketplace`.
    /// The versioned form folds the marketplace into the locator
    /// (`locator=plugin@marketplace, ref_=version`) so [`Uri::display`]
    /// round-trips and [`Uri::parse`] (which splits on the last `@`)
    /// recovers the same components.
    pub fn marketplace(plugin: &str, marketplace: &str, version: Option<&str>) -> Self {
        match version {
            None => Uri {
                source_type: SourceType::Marketplace,
                locator: plugin.to_string(),
                ref_: Some(marketplace.to_string()),
                path: None,
            },
            Some(v) => Uri {
                source_type: SourceType::Marketplace,
                locator: format!("{plugin}@{marketplace}"),
                ref_: Some(v.to_string()),
                path: None,
            },
        }
    }

    /// Decode a marketplace unit URI into its semantic
    /// `(plugin, marketplace, version)` triple.
    ///
    /// Returns `CoreError::InvalidUri` when called on a non-marketplace
    /// URI, when the marketplace segment is missing, or when a `/path`
    /// component is present (marketplace unit URIs have no path).
    pub fn as_marketplace(&self) -> Result<MarketplaceUri> {
        if self.source_type != SourceType::Marketplace {
            return Err(CoreError::InvalidUri(format!(
                "not a marketplace URI: source type is `{}`",
                self.source_type
            )));
        }
        if self.path.is_some() {
            return Err(CoreError::InvalidUri(format!(
                "marketplace unit URI must not have a `/path` component: `{}`",
                self.display()
            )));
        }
        let ref_ = self.ref_.as_deref().ok_or_else(|| {
            CoreError::InvalidUri(format!(
                "marketplace URI missing marketplace segment: `{}`",
                self.display()
            ))
        })?;
        // Versioned form has another `@` inside the locator
        // (`plugin@marketplace`); unversioned form has no `@`.
        match self.locator.split_once('@') {
            Some((plugin, marketplace)) => Ok(MarketplaceUri {
                plugin: plugin.to_string(),
                marketplace: marketplace.to_string(),
                version: Some(ref_.to_string()),
            }),
            None => Ok(MarketplaceUri {
                plugin: self.locator.clone(),
                marketplace: ref_.to_string(),
                version: None,
            }),
        }
    }
}

/// Semantic decomposition of a `marketplace:` unit URI (see
/// [`Uri::as_marketplace`] and [`Uri::marketplace`]).
///
/// Mirrors the `/plugin install <plugin>@<marketplace>` argument shape
/// from Claude Code so v1.1 discovery can flag marketplace-installed
/// plugins without re-parsing strings ad-hoc downstream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MarketplaceUri {
    pub plugin: String,
    pub marketplace: String,
    pub version: Option<String>,
}

impl From<MarketplaceUri> for Uri {
    fn from(m: MarketplaceUri) -> Self {
        Uri::marketplace(&m.plugin, &m.marketplace, m.version.as_deref())
    }
}

impl fmt::Display for Uri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

impl FromStr for Uri {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self> {
        Uri::parse(s)
    }
}

impl TryFrom<String> for Uri {
    type Error = CoreError;
    fn try_from(value: String) -> Result<Self> {
        Uri::parse(&value)
    }
}

impl From<Uri> for String {
    fn from(value: Uri) -> Self {
        value.display()
    }
}
