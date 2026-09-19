#![allow(missing_docs)]

// ABOUTME: The fence that keeps the home-directory race from coming back: no
// source in this crate may point HOME or AINB_HOME anywhere except through the
// shared scoped-home guard, which serialises the tests that need one.

use std::path::{Path, PathBuf};

/// The two variables that decide where this crate believes home is.
const OWNED: [&str; 2] = ["HOME", "AINB_HOME"];

/// The only files allowed to write them: the guard, and this fence, which has
/// to name the calls it forbids in order to find them.
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
