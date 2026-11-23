//! Shared fixtures and cached test clients for the fabric test suite.

use super::{FabricConfig, FabricGatewayService, FabricTransport};
use crate::gateway::{env::Environment, http_client};
use reqwest::Client;
use std::sync::OnceLock;

/// One pooled rustls client shared across unit tests so each test does not
/// rebuild the Mozilla root store from scratch.
pub(super) fn shared_http_client() -> Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| http_client::build().expect("shared test HTTP client"))
        .clone()
}

/// The parsed repository `config.toml`, cached once per test process.
pub(super) fn example_config() -> FabricConfig {
    static CONFIG: OnceLock<FabricConfig> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            FabricConfig::from_toml(include_str!("../../../config.toml"))
                .expect("repository example")
        })
        .clone()
}

/// Build a production-transport service backed by [`shared_http_client`].
pub(super) fn service_from_config<E>(
    config: FabricConfig,
    environment: E,
) -> Result<FabricGatewayService<FabricTransport>, super::FabricGatewayServiceBuildError>
where
    E: Environment + Send + Sync + 'static,
{
    let client = shared_http_client();
    let transport = FabricTransport::with_client(client.clone());
    FabricGatewayService::with_client(config, environment, transport, client)
}

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
#[cfg(unix)]
pub(super) fn write_executable_fixture(
    directory: &std::path::Path,
    filename: &str,
    contents: &str,
) -> std::path::PathBuf {
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
