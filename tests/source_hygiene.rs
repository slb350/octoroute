//! Source conventions required for cargo-mutants to distinguish test scaffolding.

use std::{
    fs,
    path::{Path, PathBuf},
};

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

/// The fenced source-layout tree from the authoritative `AGENTS.md`.
fn agents_source_layout() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("AGENTS.md");
    let text = fs::read_to_string(&path).expect("readable AGENTS.md");
    let section = text
        .split("## Source layout")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("AGENTS.md keeps a `## Source layout` section");
    section
        .split("```")
        .find(|block| block.contains("src/") && block.contains("tests/"))
        .expect("the section keeps one fenced layout tree")
        .to_string()
}

/// One `(indent, name)` line of the layout tree, with comments removed.
fn layout_entry(line: &str) -> Option<(usize, String)> {
    let code = line.split('#').next().unwrap_or_default().trim_end();
    let trimmed = code.trim();
    let name = trimmed.split_whitespace().next()?;
    let indent = (code.len() - trimmed.len()) / 2;
    Some((indent, name.to_string()))
}

/// Every `.rs` file the layout names, grouped by the directory naming it.
///
/// Directory entries end in `/` and parent every deeper entry until the next
/// line at their own indentation.
fn layout_files(block: &str) -> Vec<(PathBuf, String)> {
    let mut directories: Vec<(usize, PathBuf)> = Vec::new();
    let mut files = Vec::new();
    for (indent, name) in block.lines().filter_map(layout_entry) {
        while directories.last().is_some_and(|(seen, _)| *seen >= indent) {
            directories.pop();
        }
        let parent = directories
            .last()
            .map(|(_, path)| path.clone())
            .unwrap_or_default();
        let path = parent.join(&name);
        if name.ends_with('/') {
            directories.push((indent, path));
        } else if name.ends_with(".rs") {
            files.push((parent, name));
        }
    }
    files
}

/// Sorted `.rs` file names directly inside `directory`.
fn rust_file_names(directory: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for entry in fs::read_dir(directory).expect("readable source directory") {
        let path = entry.expect("readable directory entry").path();
        if path.is_file() && path.extension().is_some_and(|extension| extension == "rs") {
            names.push(
                path.file_name()
                    .expect("directory entries are named")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    names.sort();
    names
}

/// Files the layout deliberately leaves to their directory's summary: the
/// `mod.rs` every package directory has, and the `*_tests.rs` modules that
/// exist only to be wired up behind `#[cfg(test)]`.
fn unlisted_by_convention(name: &str) -> bool {
    let stem = name.strip_suffix(".rs").unwrap_or(name);
    stem == "mod" || stem.ends_with("_tests")
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

/// The layout's first level under `src/`, `gateway/`, `fabric/`, and `tests/`
/// names every file except `mod.rs` and the `*_tests.rs` modules, so an agent
/// reading the authoritative instructions cannot miss a production module or
/// an integration test contract. Deeper levels are prose summaries. The check
/// is two-way: a stale entry names a file that no longer exists.
#[test]
fn agents_layout_names_every_first_level_file_and_nothing_stale() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let layout = layout_files(&agents_source_layout());
    for directory in ["src", "src/gateway", "src/gateway/fabric", "tests"] {
        let directory = Path::new(directory);
        let on_disk = rust_file_names(&root.join(directory));
        let named = layout
            .iter()
            .filter(|(parent, _)| parent == directory)
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>();

        for name in &on_disk {
            if unlisted_by_convention(name) {
                continue;
            }
            assert!(
                named.contains(&name.as_str()),
                "AGENTS.md source layout does not name {}/{}",
                directory.display(),
                name
            );
        }
        for name in &named {
            assert!(
                on_disk.iter().any(|file| file == name),
                "AGENTS.md source layout names {}/{}, which does not exist",
                directory.display(),
                name
            );
        }
    }
}
