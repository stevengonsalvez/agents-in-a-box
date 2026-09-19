#![allow(missing_docs)]

// ABOUTME: The fence that keeps the home-directory race from coming back: no
// source in this crate may point HOME or AINB_HOME anywhere except through the
// shared scoped-home guard, which serialises the tests that need one.

use std::path::{Path, PathBuf};

/// The variable that decides where this crate believes home is.
///
/// `AINB_HOME` is not on the list, though the guard sets it too: nineteen unit
/// tests in `session_manager` point it at a directory of their own and put it
/// back, and they do it holding the crate's one environment lock, so they are
/// ordered rather than racing. Moving them onto the guard is worth doing and is
/// not this change.
const OWNED: [&str; 1] = ["HOME"];

/// The only files allowed to write them: the guard, and this fence, which has to
/// name the calls it forbids in order to find them. Nothing under `src` is on
/// this list, and nothing should be: the crate's own unit tests reach the same
/// guard through `crate::test_home`.
const ALLOWED: [&str; 2] = ["tests/support/home.rs", "tests/home_env_fence.rs"];

#[test]
fn only_the_scoped_home_guard_points_home_anywhere() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();

    for file in rust_files(&root.join("src")).chain(rust_files(&root.join("tests"))) {
        let relative = file
            .strip_prefix(&root)
            .expect("every walked file sits under the crate")
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.contains(&relative.as_str()) {
            continue;
        }
        let source = std::fs::read_to_string(&file).expect("a readable source file");
        for (number, line) in source.lines().enumerate() {
            for variable in OWNED {
                for call in ["set_var", "remove_var"] {
                    if line.contains(&format!("{call}(\"{variable}\"")) {
                        offenders.push(format!("{relative}:{}: {}", number + 1, line.trim()));
                    }
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these write the home directory into the process instead of taking it \
         from the scoped guard, which races every test running beside them. Take \
         a `ScopedHome` (tests/support/home.rs) and use its `set` for anything \
         else the test needs:\n{}",
        offenders.join("\n")
    );
}

/// Every `.rs` file under `dir`, directories first walked then forgotten, so the
/// fence sees a module whatever file it lives in.
fn rust_files(dir: &Path) -> Box<dyn Iterator<Item = PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => panic!("read {}: {error}", dir.display()),
    };
    let mut files = Vec::new();
    let mut directories = Vec::new();
    for entry in entries {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            directories.push(path);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Box::new(
        files
            .into_iter()
            .chain(directories.into_iter().flat_map(|path| rust_files(&path))),
    )
}

/// The lock is the crate's, and there is one of it.
///
/// A module that declares its own `Mutex` around `setenv` orders its own tests
/// and nothing else: two locks around one environment still let two threads
/// write it at the same time, which is the race both were written to stop.
///
/// The check is on the type and on the environment calls in the same file, not
/// on the name: a private lock called `GUARD`, or `SETTINGS_LOCK`, or nothing in
/// particular guards exactly as little as one called `ENV_LOCK`.
#[test]
fn no_module_declares_an_environment_lock_of_its_own() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut declarations = Vec::new();

    for file in rust_files(&root.join("src")).chain(rust_files(&root.join("tests"))) {
        let relative = file
            .strip_prefix(&root)
            .expect("every walked file sits under the crate")
            .to_string_lossy()
            .replace('\\', "/");
        // The lock itself, and this fence, which has to write the shapes it
        // forbids in order to look for them.
        if relative == "src/env_lock.rs" || ALLOWED.contains(&relative.as_str()) {
            continue;
        }
        let source = std::fs::read_to_string(&file).expect("a readable source file");
        // Only a file that writes the environment can be racing on it. A
        // `Mutex<()>` anywhere else is somebody ordering something of their own.
        if !writes_the_environment(&source) {
            continue;
        }
        for (number, line) in source.lines().enumerate() {
            if declares_a_std_mutex_of_unit(line) {
                declarations.push(format!("{relative}:{}: {}", number + 1, line.trim()));
            }
        }
    }

    assert!(
        declarations.is_empty(),
        "these files write the environment and declare a lock of their own \
         beside the crate's one lock in src/env_lock.rs, which orders their own \
         tests and no others:\n{}",
        declarations.join("\n")
    );
}

/// Whether `source` calls the two functions that write the process environment.
fn writes_the_environment(source: &str) -> bool {
    source.contains("env::set_var") || source.contains("env::remove_var")
}

/// Whether `line` declares a `std::sync::Mutex<()>`, which is the shape of a
/// lock that guards a thing rather than holding data.
///
/// An async mutex is not one of these: `headroom`'s `SPAWN_LOCK` is a
/// `tokio::sync::Mutex<()>` and orders spawns, not `setenv`.
fn declares_a_std_mutex_of_unit(line: &str) -> bool {
    let declares = line.contains("static ") || line.contains("const ") || line.contains("let ");
    let unit_mutex = line.contains("Mutex<()>") || line.contains("Mutex::new(())");
    let asynchronous = line.contains("tokio::sync::Mutex") || line.contains("const_new");
    declares && unit_mutex && !asynchronous
}
