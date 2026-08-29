//! Octoroute v3 inference-fabric executable.

use clap::Parser;
use octoroute::{
    cli::{Cli, Command, generate_config_template},
    gateway::{
        env::{DotenvEnvironment, ProcessEnvironment},
        fabric::{FabricConfig, FabricGatewayService, fabric_gateway_app},
    },
    telemetry,
};
use std::{io::Write, net::SocketAddr, path::Path};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Config { output }) => handle_config_command(output),
        None => run_server(Path::new(&cli.config)).await,
    };
    // Every startup failure carries a written `#[error]` message. Returning the
    // error from `main` would print its `Debug` form instead and make all of
    // them unreachable from the CLI.
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("octoroute: {error}");
            let mut source = error.source();
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            std::process::ExitCode::FAILURE
        }
    }
}

fn handle_config_command(output: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let template = generate_config_template();
    match output {
        Some(path) => {
            let path = Path::new(&path);
            write_new_artifact(path, "configuration file", template.as_bytes())?;
            eprintln!("Octoroute v3 configuration written to {}", path.display());
            eprintln!(
                "Add OCTOROUTE_API_KEY and credentials for enabled providers to {} or the process environment.",
                path.parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(".env")
                    .display()
            );
        }
        None => print!("{template}"),
    }
    Ok(())
}

fn write_new_artifact(path: &Path, label: &str, contents: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                std::io::Error::new(
                    error.kind(),
                    format!("{label} `{}` already exists", path.display()),
                )
            } else {
                error
            }
        })?;
    file.write_all(contents)
}

async fn run_server(config_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let input = std::fs::read_to_string(config_path)?;
    let config = FabricConfig::from_toml(&input)?;
    let address = SocketAddr::from((config.server.host, config.server.port));
    // Telemetry starts before service construction so warnings emitted while
    // building pools and the provider registry are not discarded.
    telemetry::init(&config.observability.log_level);
    let dotenv_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".env");
    let environment = DotenvEnvironment::from_optional_path(&dotenv_path, ProcessEnvironment)?;
    let service = FabricGatewayService::from_config(config, environment)?;
    let app = fabric_gateway_app(service);

    tracing::info!(%address, config_version = 3, "starting Octoroute gateway");
    tracing::info!("POST /v1/chat/completions");
    tracing::info!("GET /v1/models");
    tracing::info!("GET /health/live, /health/ready, /health");
    tracing::info!("GET /metrics");

    let listener = tokio::net::TcpListener::bind(address).await?;
    let (shutdown_started_tx, shutdown_started_rx) = tokio::sync::oneshot::channel();
    let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
        shutdown_signal().await;
        let _ = shutdown_started_tx.send(());
    });
    // Graceful shutdown waits for in-flight responses, and an upstream deadline
    // can be 30 minutes. Without a bound, one long generation holds the process
    // past any service-manager stop timeout and it is killed anyway - after the
    // supervisor has stopped waiting, which is the worst of both.
    if wait_for_server_shutdown(
        std::future::IntoFuture::into_future(serve),
        shutdown_started_rx,
        SHUTDOWN_GRACE,
    )
    .await?
    {
        tracing::warn!(
            timeout_s = SHUTDOWN_GRACE.as_secs(),
            "graceful shutdown deadline elapsed with responses still in flight"
        );
    }
    tracing::info!("Octoroute shutdown complete");
    Ok(())
}

/// Longest the gateway waits for in-flight responses after a stop signal.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

/// Wait for normal server completion, applying `grace` only after shutdown starts.
async fn wait_for_server_shutdown<F, E>(
    serve: F,
    mut shutdown_started: tokio::sync::oneshot::Receiver<()>,
    grace: std::time::Duration,
) -> Result<bool, E>
where
    F: std::future::Future<Output = Result<(), E>>,
{
    tokio::pin!(serve);
    tokio::select! {
        result = &mut serve => {
            result?;
            Ok(false)
        }
        _ = &mut shutdown_started => {
            match tokio::time::timeout(grace, &mut serve).await {
                Ok(result) => {
                    result?;
                    Ok(false)
                }
                Err(_) => Ok(true),
            }
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT"),
        _ = terminate => tracing::info!("received SIGTERM"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io};
    use tempfile::TempDir;

    #[test]
    fn write_new_artifact_writes_exact_bytes() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("artifact.bin");
        let contents = b"exact artifact bytes\n\0with a binary suffix";

        write_new_artifact(&path, "test artifact", contents).expect("write new artifact");

        assert_eq!(fs::read(path).expect("read artifact"), contents);
    }

    #[test]
    fn write_new_artifact_remaps_only_already_exists_errors() {
        let directory = TempDir::new().expect("temporary directory");
        let existing_path = directory.path().join("existing.toml");
        fs::write(&existing_path, "preserve me").expect("create existing file");

        let existing_error =
            write_new_artifact(&existing_path, "configuration file", b"replacement")
                .expect_err("existing path must be refused");
        assert_eq!(existing_error.kind(), io::ErrorKind::AlreadyExists);
        let message = existing_error.to_string();
        assert!(message.contains("configuration file"), "{message}");
        assert!(message.contains("already exists"), "{message}");
        assert!(
            message.contains(&existing_path.display().to_string()),
            "{message}"
        );
        assert_eq!(
            fs::read_to_string(&existing_path).expect("read preserved file"),
            "preserve me"
        );

        let missing_parent_path = directory.path().join("missing").join("artifact.toml");
        let expected_error = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&missing_parent_path)
            .expect_err("missing parent must fail");
        let actual_error =
            write_new_artifact(&missing_parent_path, "configuration file", b"contents")
                .expect_err("missing parent must fail");

        assert_eq!(actual_error.kind(), expected_error.kind());
        assert_eq!(actual_error.raw_os_error(), expected_error.raw_os_error());
        assert_eq!(actual_error.to_string(), expected_error.to_string());
    }

    #[test]
    fn handle_config_command_writes_parseable_v3_template() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("octoroute.toml");

        handle_config_command(Some(path.display().to_string())).expect("generate configuration");

        let contents = fs::read_to_string(&path).expect("read generated configuration");
        assert_eq!(contents, generate_config_template());
        let config = FabricConfig::from_toml(&contents).expect("valid v3 configuration");
        assert_eq!(config.default_model, "auto-route");
    }

    #[test]
    fn handle_config_command_refuses_to_overwrite_existing_file() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("octoroute.toml");
        fs::write(&path, "operator-owned contents").expect("create existing configuration");

        let error = handle_config_command(Some(path.display().to_string()))
            .expect_err("existing configuration must be refused");
        let io_error = error
            .downcast_ref::<io::Error>()
            .expect("configuration error must preserve its I/O type");
        assert_eq!(io_error.kind(), io::ErrorKind::AlreadyExists);
        assert!(io_error.to_string().contains("configuration file"));
        assert!(io_error.to_string().contains("already exists"));
        assert!(io_error.to_string().contains(&path.display().to_string()));
        assert_eq!(
            fs::read_to_string(path).expect("read preserved configuration"),
            "operator-owned contents"
        );
    }

    #[tokio::test]
    async fn shutdown_grace_starts_only_after_the_stop_signal() {
        let (shutdown_started_tx, shutdown_started_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(wait_for_server_shutdown(
            std::future::pending::<std::io::Result<()>>(),
            shutdown_started_rx,
            std::time::Duration::from_millis(20),
        ));

        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert!(
            !task.is_finished(),
            "the grace period must not age while the server is healthy"
        );

        shutdown_started_tx
            .send(())
            .expect("announce the stop signal");
        assert!(
            task.await.expect("join shutdown waiter").expect("waiter"),
            "a server still draining after the grace period must time out"
        );
    }
}
