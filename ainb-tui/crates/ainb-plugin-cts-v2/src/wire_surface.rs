//! Source-derived snapshot of `ainb-plugin-protocol`'s public wire surface.
//!
//! Ten crates depend on `ainb-plugin-protocol`. Nothing used to detect a
//! wire-type change landing without a version bump, so this module renders
//! the crate's public surface (method-name constants, error codes, serde
//! types with their field names / types / serde attributes, type aliases and
//! public function signatures) into a stable text document. The rendered
//! document is committed as `crates/ainb-plugin-protocol/wire-surface.lock`
//! and compared by `tests/wire_surface_gate.rs`.
//!
//! Rendering is done with `syn` over the crate sources rather than rustdoc
//! JSON so the gate is a plain `cargo test` on stable: no external binary, no
//! nightly, no published baseline (the crate is a path dependency and has
//! never been on crates.io).
//!
//! Deliberately covers the WHOLE public API of the crate, not just the
//! `#[derive(Serialize)]` types: a `pub const PLUGIN_RENDER: &str` IS the
//! wire, and a changed `pub fn decode_frame` signature breaks the SDK and the
//! runtime just as hard as a renamed struct field.

use std::fmt::Write as _;
use std::path::Path;

use quote::ToTokens;

/// Modules of `ainb-plugin-protocol` whose public items form the wire
/// surface, in the order they are rendered. `lib.rs` is included so a change
/// to the crate's re-exports is caught too.
pub const SURFACE_MODULES: &[&str] = &[
    "lib",
    "errors",
    "framing",
    "manifest",
    "methods",
    "params",
    "topics",
    "wire_buffer",
];

/// Header line of the rendered document; carries the crate version the
/// surface was last regenerated at.
const VERSION_KEY: &str = "protocol_version = ";

/// Errors from rendering or reading the surface.
#[derive(Debug)]
pub enum SurfaceError {
    /// A source file could not be read.
    Io(std::path::PathBuf, std::io::Error),
    /// A source file could not be parsed as Rust.
    Parse(std::path::PathBuf, syn::Error),
    /// The committed lock file has no `protocol_version = "..."` header.
    MissingVersionHeader,
}

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(p, e) => write!(f, "read {}: {e}", p.display()),
            Self::Parse(p, e) => write!(f, "parse {}: {e}", p.display()),
            Self::MissingVersionHeader => {
                f.write_str("lock file is missing its `protocol_version = \"...\"` header")
            }
        }
    }
}

/// Render the public wire surface of the protocol crate rooted at
/// `crate_dir` (the directory holding its `Cargo.toml`), stamped with
/// `version`.
///
/// The output is deterministic: items are emitted in source order within a
/// module and modules in [`SURFACE_MODULES`] order, all token streams are
/// normalised through `syn`, and doc comments / `#[cfg(test)]` modules are
/// dropped so prose edits never move the gate.
pub fn render(crate_dir: &Path, version: &str) -> Result<String, SurfaceError> {
    let mut out = String::new();
    out.push_str("# ainb-plugin-protocol public wire surface\n");
    out.push_str("#\n");
    out.push_str("# GENERATED. Do not hand-edit. Regenerate with:\n");
    out.push_str(
        "#   UPDATE_WIRE_SURFACE=1 cargo test -p ainb-plugin-cts-v2 --test wire_surface_gate\n",
    );
    out.push_str("#\n");
    out.push_str("# The regenerator REFUSES to write while the surface has changed but\n");
    out.push_str("# ainb-plugin-protocol's version has not. Bump the version first.\n");
    let _ = writeln!(out, "{VERSION_KEY}\"{version}\"");

    for module in SURFACE_MODULES {
        let path = crate_dir.join("src").join(format!("{module}.rs"));
        let src = std::fs::read_to_string(&path).map_err(|e| SurfaceError::Io(path.clone(), e))?;
        let file = syn::parse_file(&src).map_err(|e| SurfaceError::Parse(path.clone(), e))?;

        let _ = writeln!(out, "\n[{module}]");
        for item in &file.items {
            render_item(&mut out, item);
        }
    }
    Ok(out)
}

/// Read the `protocol_version` recorded in a rendered surface document.
pub fn recorded_version(document: &str) -> Result<&str, SurfaceError> {
    document
        .lines()
        .find_map(|l| l.strip_prefix(VERSION_KEY))
        .map(|v| v.trim().trim_matches('"'))
        .ok_or(SurfaceError::MissingVersionHeader)
}

/// Strip the version header line so two documents can be compared on their
/// surface content alone.
pub fn surface_only(document: &str) -> String {
    document
        .lines()
        .filter(|l| !l.starts_with(VERSION_KEY))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Emit one top-level item if it is public and wire-relevant.
fn render_item(out: &mut String, item: &syn::Item) {
    match item {
        syn::Item::Const(c) if is_public(&c.vis) => {
            let _ = writeln!(
                out,
                "const {}: {} = {}",
                c.ident,
                tokens(&c.ty),
                tokens(&c.expr)
            );
        }
        syn::Item::Type(t) if is_public(&t.vis) => {
            let _ = writeln!(out, "type {} = {}", t.ident, tokens(&t.ty));
        }
        syn::Item::Use(u) if is_public(&u.vis) => {
            let _ = writeln!(out, "reexport {}", tokens(&u.tree));
        }
        syn::Item::Struct(s) if is_public(&s.vis) => {
            let _ = writeln!(out, "struct {}{}", s.ident, attrs(&s.attrs));
            for field in &s.fields {
                if !is_public(&field.vis) {
                    continue;
                }
                let name = field.ident.as_ref().map_or_else(|| "_".to_owned(), ToString::to_string);
                let _ = writeln!(
                    out,
                    "  field {name}: {}{}",
                    tokens(&field.ty),
                    attrs(&field.attrs)
                );
            }
        }
        syn::Item::Enum(e) if is_public(&e.vis) => {
            let _ = writeln!(out, "enum {}{}", e.ident, attrs(&e.attrs));
            for variant in &e.variants {
                let _ = writeln!(
                    out,
                    "  variant {}{}{}",
                    variant.ident,
                    variant_shape(&variant.fields),
                    attrs(&variant.attrs)
                );
            }
        }
        syn::Item::Fn(f) if is_public(&f.vis) => {
            let _ = writeln!(out, "{}", tokens(&f.sig));
        }
        syn::Item::Impl(i) => render_impl(out, i),
        _ => {}
    }
}

/// Emit the public associated functions of an inherent impl block. Inherent
/// constructors like `RpcError::method_not_found` are part of the contract
/// the SDK and runtime code against.
fn render_impl(out: &mut String, item: &syn::ItemImpl) {
    if item.trait_.is_some() {
        return;
    }
    let ty = tokens(&item.self_ty);
    for sub in &item.items {
        if let syn::ImplItem::Fn(f) = sub {
            if is_public(&f.vis) {
                let _ = writeln!(out, "{ty}::{}", tokens(&f.sig));
            }
        }
    }
}

/// Render the shape of an enum variant's payload.
fn variant_shape(fields: &syn::Fields) -> String {
    match fields {
        syn::Fields::Unit => String::new(),
        syn::Fields::Unnamed(u) => {
            let inner = u.unnamed.iter().map(|f| tokens(&f.ty)).collect::<Vec<_>>().join(", ");
            format!("({inner})")
        }
        syn::Fields::Named(n) => {
            let inner = n
                .named
                .iter()
                .map(|f| {
                    let name = f.ident.as_ref().map_or_else(|| "_".to_owned(), ToString::to_string);
                    format!("{name}: {}{}", tokens(&f.ty), attrs(&f.attrs))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(" {{ {inner} }}")
        }
    }
}

/// Render the wire-relevant attributes of an item: `#[serde(...)]` (field
/// renames, tagging, defaults) and `#[derive(...)]` (which tells us whether
/// the type is serialised at all). Doc comments and everything else are
/// dropped so prose edits never move the gate.
fn attrs(attrs: &[syn::Attribute]) -> String {
    let mut kept: Vec<String> = Vec::new();
    for a in attrs {
        if a.path().is_ident("serde") {
            kept.push(tokens(a));
        } else if a.path().is_ident("derive") {
            let derived = tokens(a);
            // Only the (de)serialisation derives change the wire.
            if derived.contains("Serialize") || derived.contains("Deserialize") {
                kept.push(derived);
            }
        }
    }
    if kept.is_empty() {
        String::new()
    } else {
        format!(" [{}]", kept.join(" "))
    }
}

const fn is_public(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

/// Normalise any syn node to a single-line token string so formatting
/// changes in the source never move the gate.
fn tokens<T: ToTokens>(node: &T) -> String {
    node.to_token_stream().to_string()
}
