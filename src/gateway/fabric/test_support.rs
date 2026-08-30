//! Unix-only fixtures shared by the fabric's process-driving tests.

#![cfg(unix)]

use std::path::{Path, PathBuf};

/// Write a named executable fixture into `directory`.
///
/// The write runs in a child process, never in this one. Linux refuses to
/// `exec` a file any process holds open for writing, and `fork` copies the
/// whole descriptor table: an unrelated test thread spawning a child during
/// the write hands that child an inherited copy of the descriptor, and the
/// fixture stays busy until the child reaches its own `exec`. `O_CLOEXEC`
/// does not help - it closes at exec, after the window that matters. The
/// failure surfaces as `ETXTBSY` roughly one Linux run in three, and inside a
/// mutation sweep a spurious spawn failure records a mutant as caught that
/// nothing caught. macOS never enforces this, which is why only CI and the
/// offload host see it.
pub(super) fn write_executable_fixture(
    directory: &Path,
    filename: &str,
    contents: &str,
) -> PathBuf {
    let executable = directory.join(filename);
    // Contents travel as argv rather than through a pipe: no descriptor of
    // ours to inherit, and no deadlock surface if a forked child holds the
    // write end open. Fixtures are a few hundred bytes, far under ARG_MAX.
    let status = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"printf '%s' "$2" > "$1" && chmod 700 "$1""#)
        .arg("sh") // $0
        .arg(&executable)
        .arg(contents)
        .status()
        .expect("the fixture writer process must start");
    assert!(
        status.success(),
        "writing the executable fixture {} failed: {status}",
        executable.display()
    );
    executable
}
