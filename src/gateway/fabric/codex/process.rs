//! Running one bounded, group-isolated Codex CLI child process.
//!
//! Split out of `codex::mod` to keep both files inside the 600-line limit; the
//! adapter above decides what to run, this decides how a child is spawned,
//! bounded, and cleaned up.

use super::{ChildEnvironment, CodexAdapterError, STDERR_CAPTURE_MAX_BYTES};
use crate::gateway::fabric::process_group::{self, ProcessGroup};
use std::{
    ffi::OsString,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::io::AsyncWriteExt;

pub(super) struct ProcessOutput {
    pub(super) stdout: Vec<u8>,
    pub(super) status: ExitStatus,
}

pub(super) async fn run_process(
    executable: &Path,
    args: &[OsString],
    environment: &ChildEnvironment,
    cwd: &Path,
    input: &[u8],
    timeout: Duration,
    stdout_limit: usize,
) -> Result<ProcessOutput, CodexAdapterError> {
    let mut stdout = BoundedCapture::new().map_err(|_| CodexAdapterError::Workspace)?;
    let stderr = BoundedCapture::new().map_err(|_| CodexAdapterError::Workspace)?;
    let mut command = tokio::process::Command::new(executable);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(
            stdout
                .child_stdio()
                .map_err(|_| CodexAdapterError::Workspace)?,
        )
        .stderr(
            stderr
                .child_stdio()
                .map_err(|_| CodexAdapterError::Workspace)?,
        )
        .kill_on_drop(true);
    environment.apply(&mut command);
    process_group::isolate(&mut command);
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CodexAdapterError::Missing
        } else {
            CodexAdapterError::Process
        }
    })?;
    let mut process_group =
        ProcessGroup::for_child(&child).map_err(|_| CodexAdapterError::Process)?;
    let mut stdin = child.stdin.take().ok_or(CodexAdapterError::Process)?;
    let send_input = async move {
        stdin.write_all(input).await?;
        stdin.shutdown().await
    };
    let execution = async {
        let mut completion = std::pin::pin!(async { tokio::join!(child.wait(), send_input) });
        let mut poll = tokio::time::interval(Duration::from_millis(10));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                results = &mut completion => return Ok::<_, CodexAdapterError>(results),
                // The bound has to be enforced while the child runs, not only
                // after it exits: a `codex exec` that streams without ever
                // finishing is exactly the case the capture bound exists for,
                // and waiting for its exit would let it write for the whole
                // timeout window.
                _ = poll.tick() => {
                    if capture_exceeded(&stdout, &stderr, stdout_limit) {
                        return Err(CodexAdapterError::OutputTooLarge);
                    }
                }
            }
        }
    };
    let (status, stdin_result) = match tokio::time::timeout(timeout, execution).await {
        Ok(Ok(results)) => results,
        Ok(Err(error)) => {
            let cleanup = stop(&mut child, &mut process_group).await;
            return Err(surviving_error(error, cleanup));
        }
        Err(_) => {
            let cleanup = stop(&mut child, &mut process_group).await;
            return Err(surviving_error(CodexAdapterError::Timeout, cleanup));
        }
    };
    let status = match status {
        Ok(status) => {
            stop(&mut child, &mut process_group).await?;
            status
        }
        Err(_) => {
            stop(&mut child, &mut process_group).await?;
            return Err(CodexAdapterError::Process);
        }
    };
    if capture_exceeded(&stdout, &stderr, stdout_limit) {
        return Err(CodexAdapterError::OutputTooLarge);
    }
    if status.success() && stdin_result.is_err() {
        return Err(CodexAdapterError::Process);
    }
    let stdout = stdout
        .read_bounded(stdout_limit)
        .map_err(|_| CodexAdapterError::OutputTooLarge)?;
    Ok(ProcessOutput { stdout, status })
}

/// Keep the error that caused cleanup, whatever cleanup itself reports.
///
/// Cleanup only runs because something already went wrong, so its failure is
/// the lesser fact. `OutputTooLarge` and `Timeout` are what the route's
/// fallback policy reads; reporting `Process` in their place would change the
/// routing decision for a request that actually hit a bound.
pub(super) fn surviving_error(
    primary: CodexAdapterError,
    cleanup: Result<(), CodexAdapterError>,
) -> CodexAdapterError {
    if cleanup.is_err() {
        tracing::warn!(reason = %primary, "codex cleanup failed after an error");
    }
    primary
}

/// Whether either captured stream has outgrown its bound.
///
/// Both streams are bounded, and stderr is not the lesser half: the CLI writes
/// its progress and diagnostics there, and nothing downstream ever reads it, so
/// an unbounded stderr is a child filling the disk with output no one will look
/// at. A capture whose size cannot be read is treated as over budget, because
/// the alternative is capturing without a bound at all.
pub(super) fn capture_exceeded(
    stdout: &BoundedCapture,
    stderr: &BoundedCapture,
    stdout_limit: usize,
) -> bool {
    stdout.exceeds(stdout_limit).unwrap_or(true)
        || stderr.exceeds(STDERR_CAPTURE_MAX_BYTES).unwrap_or(true)
}

pub(super) async fn stop(
    child: &mut tokio::process::Child,
    process_group: &mut ProcessGroup,
) -> Result<(), CodexAdapterError> {
    process_group
        .terminate(child)
        .await
        .map_err(|_| CodexAdapterError::Process)
}

pub(super) struct BoundedCapture {
    pub(super) file: std::fs::File,
}

impl BoundedCapture {
    pub(super) fn new() -> std::io::Result<Self> {
        tempfile::tempfile().map(|file| Self { file })
    }

    pub(super) fn child_stdio(&self) -> std::io::Result<Stdio> {
        self.file.try_clone().map(Stdio::from)
    }

    pub(super) fn exceeds(&self, limit: usize) -> std::io::Result<bool> {
        self.file
            .metadata()
            .map(|metadata| metadata.len() > limit as u64)
    }

    pub(super) fn read_bounded(&mut self, limit: usize) -> std::io::Result<Vec<u8>> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        self.file
            .by_ref()
            .take(limit.saturating_add(1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(std::io::Error::other("bounded capture exceeded"));
        }
        Ok(bytes)
    }
}
