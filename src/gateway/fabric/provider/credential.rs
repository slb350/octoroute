//! Provider credential resolution, caching, and validation.

use crate::gateway::env::Environment;
use crate::gateway::fabric::codex::ChildEnvironment;
use crate::gateway::fabric::process_group::{self, ProcessGroup};
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
        // Held across the resolve, TTL re-checked after acquiring. Dropping the
        // guard first meant every request in flight when the TTL lapsed spawned
        // its own `op`/`pass`/`gcloud`, which is the per-request spawn this
        // cache exists to avoid - just once per expiry instead of always. The
        // wait is bounded by CREDENTIAL_COMMAND_TIMEOUT.
        let mut cached = self.cached.lock().await;
        if let Some((resolved_at, value)) = cached.as_ref()
            && cache_entry_is_fresh_at(*resolved_at, Instant::now())
        {
            return Ok(value.clone());
        }
        let value = self.source.resolve().await?;
        *cached = Some((Instant::now(), value.clone()));
        Ok(value)
    }

    /// Discard the cached credential after the provider rejected it.
    pub(super) async fn invalidate(&self) {
        self.cached.lock().await.take();
    }
}

fn cache_entry_is_fresh_at(resolved_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(resolved_at) < CREDENTIAL_CACHE_TTL
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
            } => resolve_command_credential(command, environment, CREDENTIAL_COMMAND_TIMEOUT).await,
        }
    }
}

/// Run one credential command under a single wall-clock budget.
///
/// `timeout` bounds the whole call, not each half of it. Timing the stdout read
/// and the `wait` separately let a command that reads slowly and then exits
/// slowly hold the credential mutex - and with it every request and readiness
/// probe for this provider - for twice the documented bound.
async fn resolve_command_credential(
    arguments: &[String],
    environment: &ChildEnvironment,
    timeout: Duration,
) -> Result<SecretString, ProviderCredentialError> {
    let deadline = tokio::time::Instant::now() + timeout;
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
    process_group::isolate(&mut command);
    let mut child = command
        .spawn()
        .map_err(|_| ProviderCredentialError::CommandFailed)?;
    let mut process_group =
        ProcessGroup::for_child(&child).map_err(|_| ProviderCredentialError::CommandFailed)?;
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
    match tokio::time::timeout_at(deadline, read).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => {
            terminate(&mut child, &mut process_group).await?;
            return Err(ProviderCredentialError::CommandFailed);
        }
        Err(_) => {
            terminate(&mut child, &mut process_group).await?;
            return Err(ProviderCredentialError::CommandTimeout);
        }
    }
    if output.len() > MAX_CREDENTIAL_BYTES {
        terminate(&mut child, &mut process_group).await?;
        return Err(ProviderCredentialError::CommandOutputTooLarge);
    }
    let status = match tokio::time::timeout_at(deadline, child.wait()).await {
        Ok(Ok(status)) => {
            terminate(&mut child, &mut process_group).await?;
            status
        }
        Ok(Err(_)) => {
            terminate(&mut child, &mut process_group).await?;
            return Err(ProviderCredentialError::CommandFailed);
        }
        Err(_) => {
            terminate(&mut child, &mut process_group).await?;
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

async fn terminate(
    child: &mut Child,
    process_group: &mut ProcessGroup,
) -> Result<(), ProviderCredentialError> {
    process_group
        .terminate(child)
        .await
        .map_err(|_| ProviderCredentialError::CommandFailed)
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

// Exactly `#[cfg(test)]`; see the note in `gateway::fabric`. The Unix bound is
// per item rather than an inner attribute, which clippy rejects as mixed
// attribute styles - and the three tests below that drive no process do not
// need it at all.
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn output_command(bytes: usize) -> [String; 3] {
        [
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!(
                "/usr/bin/awk 'BEGIN {{ for (i = 0; i < {bytes}; i++) printf \"x\" }}'; sleep 0.1"
            ),
        ]
    }

    #[test]
    fn credential_cache_expires_exactly_at_its_ttl() {
        let resolved_at = Instant::now();

        assert!(cache_entry_is_fresh_at(resolved_at, resolved_at));
        assert!(cache_entry_is_fresh_at(
            resolved_at,
            resolved_at + CREDENTIAL_CACHE_TTL - Duration::from_nanos(1)
        ));
        assert!(!cache_entry_is_fresh_at(
            resolved_at,
            resolved_at + CREDENTIAL_CACHE_TTL
        ));
        assert!(!cache_entry_is_fresh_at(
            resolved_at,
            resolved_at + CREDENTIAL_CACHE_TTL + Duration::from_nanos(1)
        ));
    }

    #[test]
    fn credential_shape_boundaries_are_exact() {
        assert_eq!(MAX_CREDENTIAL_BYTES, 4_096);
        for (value, valid) in [
            (String::new(), false),
            ("x".repeat(MAX_CREDENTIAL_BYTES), true),
            ("x".repeat(MAX_CREDENTIAL_BYTES + 1), false),
            ("embedded space".to_string(), false),
            ("line\nfeed".to_string(), false),
            ("visible-ASCII-~".to_string(), true),
        ] {
            assert_eq!(
                validate_credential(SecretString::from(value)).is_ok(),
                valid
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_output_accepts_the_limit_and_rejects_one_more_byte() {
        let exact = resolve_command_credential(
            &output_command(MAX_CREDENTIAL_BYTES),
            &ChildEnvironment::default(),
            CREDENTIAL_COMMAND_TIMEOUT,
        )
        .await
        .expect("the exact limit is accepted");
        assert_eq!(exact.expose_secret().len(), MAX_CREDENTIAL_BYTES);

        let oversized = resolve_command_credential(
            &output_command(MAX_CREDENTIAL_BYTES + 1),
            &ChildEnvironment::default(),
            CREDENTIAL_COMMAND_TIMEOUT,
        )
        .await;
        assert!(
            matches!(
                oversized,
                Err(ProviderCredentialError::CommandOutputTooLarge)
            ),
            "unexpected oversized-command result: {oversized:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_kills_and_reaps_the_child_before_returning() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exec /bin/sleep 30"])
            .kill_on_drop(true);
        process_group::isolate(&mut command);
        let mut child = command.spawn().expect("spawn child");
        let mut process_group = ProcessGroup::for_child(&child).expect("process group");

        terminate(&mut child, &mut process_group)
            .await
            .expect("terminate group");

        assert!(
            child.try_wait().expect("poll child").is_some(),
            "terminate must reap the child before returning"
        );
    }

    #[test]
    fn every_credential_error_has_its_exact_safe_code() {
        for (error, expected) in [
            (ProviderCredentialError::Missing, "missing"),
            (ProviderCredentialError::Invalid, "invalid"),
            (ProviderCredentialError::CommandFailed, "command_failed"),
            (ProviderCredentialError::CommandTimeout, "command_timeout"),
            (
                ProviderCredentialError::CommandOutputTooLarge,
                "command_output_too_large",
            ),
        ] {
            assert_eq!(error.code(), expected);
        }
    }

    /// The one budget is a deadline in the future. A command that answers
    /// promptly has to resolve, which is the whole point of supporting
    /// `api_key_command`: `op`/`pass`/`gcloud` return in milliseconds, and a
    /// deadline computed backwards would time every one of them out and report
    /// every provider unauthenticated.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_prompt_command_resolves_within_its_budget() {
        let command = [
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'resolved-key\\n'".to_string(),
        ];

        let resolved = resolve_command_credential(
            &command,
            &ChildEnvironment::default(),
            CREDENTIAL_COMMAND_TIMEOUT,
        )
        .await
        .expect("a command that answers at once must resolve");

        // The trailing newline a shell command emits is not part of the key.
        assert_eq!(resolved.expose_secret(), "resolved-key");
    }

    /// The credential mutex is held across the whole resolve, so the command's
    /// budget has to be a single wall-clock bound. A per-half bound lets a
    /// command that reads slowly and then exits slowly hold it for twice as
    /// long as the timeout says.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_slow_command_is_bounded_in_total_not_per_phase() {
        let timeout = Duration::from_millis(500);
        let command = [
            "/bin/sh".to_string(),
            "-c".to_string(),
            // Writes and closes stdout part-way through the budget, so the read
            // returns, and then outlives any second budget of the same size.
            "sleep 0.4; printf secret-key; exec 1>&-; sleep 30".to_string(),
        ];
        let started = Instant::now();
        let result =
            resolve_command_credential(&command, &ChildEnvironment::default(), timeout).await;
        let elapsed = started.elapsed();
        assert!(
            matches!(result, Err(ProviderCredentialError::CommandTimeout)),
            "a command that never exits must time out"
        );
        // Per-phase bounds would allow the 0.4s read and then a fresh 0.5s
        // wait; one budget cuts the whole call at 0.5s.
        assert!(
            elapsed < timeout + Duration::from_millis(200),
            "the read and the wait share one budget; took {elapsed:?}"
        );
    }
}
