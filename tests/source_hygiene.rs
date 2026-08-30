//! Source-level invariants that keep the mutation sweep honest.
//!
//! Both rules below were already documented in CLAUDE.md and both were already
//! violated: the compound test gate was fixed once in `process_group.rs` and
//! survived in three other files, because nothing but review enforced it.

use std::{fs, path::Path};

/// Every `.rs` file under `src/`, as (repo-relative path, contents).
fn source_files() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("readable source directory") {
            let path = entry.expect("readable directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let relative = path
                    .strip_prefix(root.parent().expect("src has a parent"))
                    .expect("path below the repository root")
                    .to_string_lossy()
                    .into_owned();
                files.push((
                    relative,
                    fs::read_to_string(&path).expect("readable source"),
                ));
            }
        }
    }
    assert!(!files.is_empty(), "no sources found under src/");
    files
}

/// cargo-mutants recognizes only the literal `#[cfg(test)]` as test
/// scaffolding. Under a compound gate it mutates the helpers and reports them
/// as surviving production mutants, so the extra condition belongs inside the
/// module as an inner attribute.
#[test]
fn test_modules_use_the_bare_cfg_test_gate() {
    let mut violations = Vec::new();
    for (path, contents) in source_files() {
        for (index, line) in contents.lines().enumerate() {
            if line.trim_start().starts_with("#[cfg(all(test") {
                violations.push(format!("{path}:{}: {}", index + 1, line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "a compound test gate hides test helpers from cargo-mutants as production \
         survivors. Use `#[cfg(test)]` and move the extra condition inside: an inner \
         `#![cfg(..)]` for a file module, per-item attributes for an inline one, which \
         clippy's mixed_attributes_style requires:\n{}",
        violations.join("\n")
    );
}

/// The mirror of the rule above. A module the sweep host never compiles is one
/// no mutant inside it can be observed in, so every one reports missed with no
/// test at fault. Platform differences belong in cfg'd blocks inside shared,
/// always-compiled items, as `process_group.rs` and `main.rs` do.
#[test]
fn platform_fallbacks_are_blocks_rather_than_modules() {
    let mut violations = Vec::new();
    for (path, contents) in source_files() {
        let lines: Vec<&str> = contents.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if !(trimmed.starts_with("#[cfg(not(unix))]") || trimmed.starts_with("#[cfg(windows)]"))
            {
                continue;
            }
            // The gated item is the next line that is neither blank nor a
            // further attribute or comment.
            let gated = lines[index + 1..].iter().find(|candidate| {
                let candidate = candidate.trim_start();
                !candidate.is_empty()
                    && !candidate.starts_with("#[")
                    && !candidate.starts_with("//")
            });
            if gated.is_some_and(|gated| gated.trim_start().starts_with("mod ")) {
                violations.push(format!("{path}:{}: {}", index + 1, trimmed));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "a platform module the sweep host never compiles reports every mutant inside \
         it as missed; express the difference as a cfg'd block inside a shared item:\n{}",
        violations.join("\n")
    );
}
