//! `ainb search <query>` — greps unit names, descriptions, and tags
//! across every enabled source's fetched checkout.
//!
//! Cache hits are driven entirely by `lockfile.yaml.fetched_path`;
//! sources without a recorded checkout (added pre-fetch, or
//! disabled) are silently skipped.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;

use ainb_adapters_source::{pick_adapter, UnitDescriptor};
use ainb_skill_core::lockfile::Lockfile;
use ainb_skill_core::manifest::{Manifest, SourceEntry};
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};

use crate::SearchArgs;

struct Hit {
    source_name: String,
    source_uri: String,
    source_ref: String,
    unit: UnitDescriptor,
}

impl Hit {
    fn full_uri(&self) -> String {
        format!(
            "{}@{}/{}",
            self.source_uri, self.source_ref, self.unit.path
        )
    }
}

pub fn dispatch(home: &Path, args: SearchArgs, out: &mut dyn io::Write) -> Result<()> {
    let manifest = Manifest::load_from(&manifest_path_in(home))?;
    let lockfile = Lockfile::load_from(&lockfile_path_in(home))?;
    let hits = collect_hits(&manifest, &lockfile, &args)?;

    if hits.is_empty() {
        writeln!(out, "no matches")?;
        return Ok(());
    }

    render_table(&hits, out)
}

fn collect_hits(
    manifest: &Manifest,
    lockfile: &Lockfile,
    args: &SearchArgs,
) -> Result<Vec<Hit>> {
    let query_lc = args.query.to_lowercase();
    let mut hits = Vec::new();

    for source in &manifest.sources {
        if !source.enabled {
            continue;
        }
        let Some(path) = checkout_path_for(source, lockfile) else {
            continue;
        };
        let Some(adapter) = pick_adapter(&path) else {
            continue;
        };

        let units = adapter.list_units(&path)?;
        for unit in units {
            if !matches_query(&unit, &query_lc) {
                continue;
            }
            if let Some(kind_filter) = &args.kind {
                if &unit.kind != kind_filter {
                    continue;
                }
            }
            hits.push(Hit {
                source_name: source.name.clone(),
                source_uri: source.uri.clone(),
                source_ref: source.r#ref.clone(),
                unit,
            });
        }
    }

    Ok(hits)
}

fn checkout_path_for(source: &SourceEntry, lockfile: &Lockfile) -> Option<PathBuf> {
    let locked = lockfile.sources.iter().find(|s| s.name == source.name)?;
    let path = PathBuf::from(locked.fetched_path.as_deref()?);
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

fn matches_query(unit: &UnitDescriptor, query_lc: &str) -> bool {
    if query_lc.is_empty() {
        return true;
    }
    if unit.name.to_lowercase().contains(query_lc) {
        return true;
    }
    if unit
        .description
        .as_deref()
        .unwrap_or("")
        .to_lowercase()
        .contains(query_lc)
    {
        return true;
    }
    unit.tags
        .iter()
        .any(|t| t.to_lowercase().contains(query_lc))
}

fn render_table(hits: &[Hit], out: &mut dyn io::Write) -> Result<()> {
    let uri_strs: Vec<String> = hits.iter().map(Hit::full_uri).collect();
    let uri_w = header_width("URI", &uri_strs);
    let kind_w = header_width("KIND", &hits.iter().map(|h| h.unit.kind.clone()).collect::<Vec<_>>());
    let src_w = header_width("SOURCE", &hits.iter().map(|h| h.source_name.clone()).collect::<Vec<_>>());

    writeln!(
        out,
        "{:<uri_w$}  {:<kind_w$}  {:<src_w$}  DESCRIPTION",
        "URI", "KIND", "SOURCE",
    )?;
    for (h, uri) in hits.iter().zip(uri_strs.iter()) {
        let desc = h
            .unit
            .description
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect::<String>();
        writeln!(
            out,
            "{:<uri_w$}  {:<kind_w$}  {:<src_w$}  {desc}",
            uri, h.unit.kind, h.source_name,
        )?;
    }
    Ok(())
}

fn header_width(header: &str, values: &[String]) -> usize {
    let max_val = values.iter().map(String::len).max().unwrap_or(0);
    header.len().max(max_val)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(name: &str, kind: &str, desc: &str, tags: &[&str]) -> UnitDescriptor {
        UnitDescriptor {
            name: name.into(),
            kind: kind.into(),
            description: if desc.is_empty() {
                None
            } else {
                Some(desc.into())
            },
            path: format!("skills/{name}"),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            requires: vec![],
        }
    }

    #[test]
    fn matches_name() {
        assert!(matches_query(&unit("commit", "skill", "", &[]), "comm"));
    }

    #[test]
    fn matches_description() {
        assert!(matches_query(
            &unit("x", "skill", "well-formed commits", &[]),
            "commit"
        ));
    }

    #[test]
    fn matches_tag() {
        assert!(matches_query(&unit("x", "skill", "", &["git"]), "git"));
    }

    #[test]
    fn empty_query_matches_everything() {
        assert!(matches_query(&unit("x", "skill", "", &[]), ""));
    }

    #[test]
    fn no_match_returns_false() {
        assert!(!matches_query(&unit("x", "skill", "y", &["z"]), "q"));
    }
}
