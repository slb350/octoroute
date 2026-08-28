//! Unix-only fixtures shared by the fabric's process-driving tests.

use std::path::{Path, PathBuf};

/// Write a named executable fixture into `directory`.
pub(super) fn write_executable_fixture(
    directory: &Path,
    filename: &str,
    contents: &str,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let executable = directory.join(filename);
    std::fs::write(&executable, contents).expect("executable fixture");
    let mut permissions = std::fs::metadata(&executable)
        .expect("executable fixture metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable, permissions).expect("executable fixture permissions");
    executable
}
