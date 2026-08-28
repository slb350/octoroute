//! Provider credential resolution, caching, and validation.

use crate::gateway::env::Environment;
use crate::gateway::fabric::codex::ChildEnvironment;
use secrecy::{ExposeSecret, SecretString};
use std::{
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::Mutex,
};

const MAX_CREDENTIAL_BYTES: usize = 4 * 1024;
const CREDENTIAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest a resolved provider credential is reused before being re-resolved.
const CREDENTIAL_CACHE_TTL: Duration = Duration::from_secs(300);

/// A resolved credential held for a bounded time.
///
/// Without this, `api_key_command` spawns a subprocess on every request and
/// again on every readiness refresh. The cache is invalidated whenever the
/// provider answers 401 or 403, so a rotated key is picked up without a restart.
pub(super) struct CachedCredential {
    source: ProviderCredentialSource,
    cached: Mutex<Option<(Instant, SecretString)>>,
}

impl CachedCredential {
    pub(super) fn new(source: ProviderCredentialSource) -> Self {
        Self {
            source,
            cached: Mutex::new(None),
        }
    }

    pub(super) async fn resolve(&self) -> Result<SecretString, ProviderCredentialError> {
        {
            let cached = self.cached.lock().await;
            if let Some((resolved_at, value)) = cached.as_ref()
                && resolved_at.elapsed() < CREDENTIAL_CACHE_TTL
            {
                return Ok(value.clone());
            }
        }
        let value = self.source.resolve().await?;
        *self.cached.lock().await = Some((Instant::now(), value.clone()));
        Ok(value)
    }

    /// Discard the cached credential after the provider rejected it.
    pub(super) async fn invalidate(&self) {
        self.cached.lock().await.take();
    }
}

pub(super) enum ProviderCredentialSource {
    Environment {
        name: String,
        environment: Arc<dyn Environment + Send + Sync>,
    },
    Command {
        command: Vec<String>,
        environment: ChildEnvironment,
    },
}

impl ProviderCredentialSource {
    async fn resolve(&self) -> Result<SecretString, ProviderCredentialError> {
        match self {
            Self::Environment { name, environment } => environment
                .get(name)
                .ok_or(ProviderCredentialError::Missing)
                .and_then(validate_credential),
            Self::Command {
                command,
                environment,
            } => resolve_command_credential(command, environment).await,
        }
    }
}

async fn resolve_command_credential(
    arguments: &[String],
    environment: &ChildEnvironment,
) -> Result<SecretString, ProviderCredentialError> {
    let (program, arguments) = arguments
        .split_first()
        .ok_or(ProviderCredentialError::CommandFailed)?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // `op`, `security`, `pass`, `gcloud`, and `aws` all read HOME, and several
    // need TMPDIR and locale. Restoring PATH alone makes every one of them fail
    // as an indistinguishable provider outage.
    environment.apply(&mut command);
    let mut child = command
        .spawn()
        .map_err(|_| ProviderCredentialError::CommandFailed)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(ProviderCredentialError::CommandFailed)?;
    let mut output = Vec::with_capacity(MAX_CREDENTIAL_BYTES + 1);
    let read = async {
        stdout
            .take((MAX_CREDENTIAL_BYTES + 1) as u64)
            .read_to_end(&mut output)
            .await
    };
    match tokio::time::timeout(CREDENTIAL_COMMAND_TIMEOUT, read).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => {
            terminate(&mut child).await;
            return Err(ProviderCredentialError::CommandFailed);
        }
        Err(_) => {
            terminate(&mut child).await;
            return Err(ProviderCredentialError::CommandTimeout);
        }
    }
    if output.len() > MAX_CREDENTIAL_BYTES {
        terminate(&mut child).await;
        return Err(ProviderCredentialError::CommandOutputTooLarge);
    }
    let status = match tokio::time::timeout(CREDENTIAL_COMMAND_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => return Err(ProviderCredentialError::CommandFailed),
        Err(_) => {
            terminate(&mut child).await;
            return Err(ProviderCredentialError::CommandTimeout);
        }
    };
    if !status.success() {
        return Err(ProviderCredentialError::CommandFailed);
    }
    let output = String::from_utf8(output).map_err(|_| ProviderCredentialError::Invalid)?;
    validate_credential(SecretString::from(
        output.trim_end_matches(['\r', '\n']).to_string(),
    ))
}

async fn terminate(child: &mut Child) {
    let _ = child.kill().await;
}

fn validate_credential(value: SecretString) -> Result<SecretString, ProviderCredentialError> {
    let candidate = value.expose_secret();
    if candidate.is_empty()
        || candidate.len() > MAX_CREDENTIAL_BYTES
        || !candidate.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(ProviderCredentialError::Invalid);
    }
    Ok(value)
}

#[derive(Debug, Error)]
pub(super) enum ProviderCredentialError {
    #[error("credential is missing")]
    Missing,
    #[error("credential has an invalid shape")]
    Invalid,
    #[error("credential command failed")]
    CommandFailed,
    #[error("credential command timed out")]
    CommandTimeout,
    #[error("credential command output exceeded its bound")]
    CommandOutputTooLarge,
}

impl ProviderCredentialError {
    pub(super) const fn code(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Invalid => "invalid",
            Self::CommandFailed => "command_failed",
            Self::CommandTimeout => "command_timeout",
            Self::CommandOutputTooLarge => "command_output_too_large",
        }
    }
}
