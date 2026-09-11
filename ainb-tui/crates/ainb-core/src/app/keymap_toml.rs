//! TOML override parser for `~/.agents-in-a-box/keymap.toml`.

use std::path::Path;

/// One raw, line-addressable override before it is applied to a `Keymap`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideRow {
    pub context: String,
    pub event: String,
    pub chord: String,
}

/// Parsed override document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeymapOverrides {
    rows: Vec<OverrideRow>,
}

impl KeymapOverrides {
    /// Parse `[context] event_name = "chord"` TOML.
    pub fn parse(input: &str) -> Result<Self, String> {
        let value: toml::Value = toml::from_str(input).map_err(|error| error.to_string())?;
        let root =
            value.as_table().ok_or_else(|| "keymap TOML root must be a table".to_string())?;
        let mut rows = Vec::new();
        for (context, entries) in root {
            let entries = entries
                .as_table()
                .ok_or_else(|| format!("[{context}] must contain event = \"chord\" entries"))?;
            for (event, chord) in entries {
                let chord = chord
                    .as_str()
                    .ok_or_else(|| format!("{context}.{event} must be a string chord"))?;
                rows.push(OverrideRow {
                    context: context.clone(),
                    event: event.clone(),
                    chord: chord.to_string(),
                });
            }
        }
        Ok(Self { rows })
    }

    /// Read a file without making a missing override an error.
    pub fn from_path(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Self::parse(&contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    /// Rows in document order.
    pub fn rows(&self) -> impl Iterator<Item = &OverrideRow> {
        self.rows.iter()
    }

    /// Conventional location, deliberately outside the `config/` subtree.
    #[must_use]
    pub fn default_path() -> Option<std::path::PathBuf> {
        dirs::home_dir().map(|home| home.join(".agents-in-a-box").join("keymap.toml"))
    }
}
