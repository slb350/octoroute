//! Source conventions required for cargo-mutants to distinguish test scaffolding.

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

#[test]
fn source_layout_keeps_mutation_targets_observable() {
    let mut violations = Vec::new();
    for (path, contents) in source_files() {
        let lines: Vec<&str> = contents.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("#[cfg(all(test") {
                violations.push(format!(
                    "{path}:{}: use a bare #[cfg(test)] gate with extra conditions inside",
                    index + 1
                ));
            }
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
                violations.push(format!(
                    "{path}:{}: put platform differences inside shared items, not modules",
                    index + 1
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "source layout hides scaffolding or production code from cargo-mutants:\n{}",
        violations.join("\n")
    );
}
