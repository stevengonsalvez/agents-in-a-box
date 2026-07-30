//! Forces a rebuild whenever anything under `migrations/` changes.
//!
//! `sqlx::migrate!("./migrations")` (see `src/lib.rs`) embeds the migration
//! set at COMPILE time, but on stable Rust the macro has no way to register
//! that directory as a build dependency. Without this file, cargo has no
//! signal that `migrations/` affects the crate, so adding, editing, or
//! deleting a `.sql` file leaves the compiled binary caching the OLD
//! migration set even though `cargo build` reports success.
//!
//! The failure this caused in practice: a migration was present on disk and
//! already applied in the database, but the stale binary had never embedded
//! it, so the daemon refused to boot with `migration N was previously
//! applied but is missing in the resolved migrations` (an error that
//! points at the database when the database is fine and the binary is
//! stale). The only prior workaround was `cargo clean -p ainb-hangar-store`.
//!
//! `cargo:rerun-if-changed=<dir>` tracks file creation, edits, and deletion
//! within that directory (verified empirically, not just per Cargo docs),
//! so this one line is sufficient, no per-file entries needed. Do not
//! delete this thinking it is a no-op: without it, migration changes are
//! silently invisible to the build.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
