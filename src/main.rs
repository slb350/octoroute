//! Octoroute version-aware local/cloud gateway.

use clap::Parser;
use octoroute::{
    calibration::{CalibrationError, MAX_ARTIFACT_BYTES, analyze_jsonl},
    cli::{Cli, Command, generate_config_template},
    gateway::{
        config::ProcessEnvironment,
        env::DotenvEnvironment,
        fabric::{FabricGatewayService, fabric_gateway_app},
        http::gateway_app,
        runtime::RuntimeConfig,
        service::GatewayService,
    },
    telemetry,
};
use std::{
    io::{Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Config { output }) => handle_config_command(output),
        Some(Command::Calibrate {
            input,
            output,
            grid_step,
        }) => handle_calibrate_command(&input, output, grid_step),
        None => run_server(Path::new(&cli.config)).await,
    }
}

fn handle_calibrate_command(
    input: &str,
    output: Option<String>,
    grid_step: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    let artifact = read_bounded_artifact(std::fs::File::open(input)?, MAX_ARTIFACT_BYTES)?;
    let report = analyze_jsonl(&artifact, grid_step)?;
    match output {
        Some(path) => {
            let path = PathBuf::from(path);
            write_new_artifact(&path, "calibration report", report.as_bytes())?;
        }
        None => println!("{report}"),
    }
    Ok(())
}

fn read_bounded_artifact(
    reader: impl Read,
    max_bytes: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    // Read one extra byte so non-regular or concurrently growing inputs cannot bypass the cap.
    let read_limit = u64::try_from(max_bytes)?.saturating_add(1);
    let mut bytes = Vec::new();
    reader.take(read_limit).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(CalibrationError::ArtifactTooLarge.into());
    }
    String::from_utf8(bytes).map_err(|_| CalibrationError::InvalidEncoding.into())
}

fn handle_config_command(output: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let template = generate_config_template();
    match output {
        Some(path) => {
            let path = PathBuf::from(path);
            write_new_artifact(&path, "configuration file", template.as_bytes())?;
            eprintln!("Octoroute v2 configuration written to {}", path.display());
            eprintln!(
                "Add OCTOROUTE_API_KEY and OPENROUTER_API_KEY to {} or the process environment.",
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
    let dotenv_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".env");
    let environment = DotenvEnvironment::from_optional_path(&dotenv_path, ProcessEnvironment)?;

    let (address, app, version, log_level) = match RuntimeConfig::from_toml(&input, &environment)? {
        RuntimeConfig::V2(config) => {
            let address = SocketAddr::from((config.server().host(), config.server().port()));
            let log_level = config.observability().log_level().as_str().to_string();
            let service = GatewayService::from_config(*config)?;
            (address, gateway_app(service), 2_u8, log_level)
        }
        RuntimeConfig::V3(config) => {
            let address = SocketAddr::from((config.server.host, config.server.port));
            let log_level = config.observability.log_level.clone();
            let service = FabricGatewayService::from_config(*config, environment)?;
            (address, fabric_gateway_app(service), 3_u8, log_level)
        }
    };
    telemetry::init(&log_level);

    tracing::info!(%address, config_version = version, "starting Octoroute gateway");
    tracing::info!("POST /v1/chat/completions");
    tracing::info!("GET /v1/models");
    tracing::info!("GET /health/live, /health/ready, /health");
    tracing::info!("GET /metrics");

    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("Octoroute shutdown complete");
    Ok(())
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

    #[test]
    fn calibration_reader_enforces_limit_during_read() {
        let accepted = read_bounded_artifact(std::io::Cursor::new(b"test"), 4)
            .expect("artifact at the byte limit");
        assert_eq!(accepted, "test");

        let error = read_bounded_artifact(std::io::repeat(b'x'), 4)
            .expect_err("an unbounded reader must stop at the byte limit");
        assert!(matches!(
            error.downcast_ref::<CalibrationError>(),
            Some(CalibrationError::ArtifactTooLarge)
        ));
    }
}
