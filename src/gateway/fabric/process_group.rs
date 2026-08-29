//! Cancellation-safe ownership for one spawned process tree.

use std::io;
use tokio::process::{Child, Command};

/// Put the child in its own process group so the whole tree can be signalled
/// at once. A no-op where process groups do not exist.
pub(crate) fn isolate(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.as_std_mut().process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

/// Own the process group led by one spawned child.
///
/// Dropping the future that owns this guard still kills the whole group.
/// Normal timeout paths use [`ProcessGroup::terminate`] so signal and reap
/// errors remain observable.
///
/// One type serves both platforms deliberately: the platform-specific pieces
/// are cfg'd blocks inside shared functions, not a `cfg(not(unix))` module,
/// because a module that no Unix build compiles is also one no Unix mutation
/// sweep can observe - every mutant inside it reports missed without any test
/// being at fault.
pub(crate) struct ProcessGroup {
    // Read only by the Unix signal path; elsewhere no group exists to signal
    // and the field stays `None`.
    #[cfg_attr(not(unix), allow(dead_code))]
    pgid: Option<i32>,
}

impl ProcessGroup {
    pub(crate) fn for_child(child: &Child) -> io::Result<Self> {
        #[cfg(unix)]
        let pgid = Some(
            child
                .id()
                .and_then(|pid| i32::try_from(pid).ok())
                .ok_or_else(|| io::Error::other("spawned child has no valid process id"))?,
        );
        #[cfg(not(unix))]
        let pgid = {
            child
                .id()
                .ok_or_else(|| io::Error::other("spawned child has no process id"))?;
            None
        };
        Ok(Self { pgid })
    }

    pub(crate) async fn terminate(&mut self, child: &mut Child) -> io::Result<()> {
        #[cfg(unix)]
        let signal = self.signal(libc::SIGKILL);
        #[cfg(not(unix))]
        let signal = child.kill().await;
        let wait = child.wait().await.map(|_| ());
        let result = signal.and(wait);
        if result.is_ok() {
            self.pgid = None;
        }
        result
    }

    #[cfg(unix)]
    fn signal(&self, signal: i32) -> io::Result<()> {
        let Some(pgid) = self.pgid else {
            return Ok(());
        };
        // SAFETY: `pgid` came from the successfully spawned group leader
        // and is negated only to select its Unix process group.
        let result = unsafe { libc::kill(-pgid, signal) };
        if result == 0 {
            Ok(())
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

// Only Unix has a group to signal. Elsewhere `kill_on_drop` on the command
// reaps the child, exactly as before this guard existed.
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // Drop cannot report an error. It is the cancellation backstop;
        // ordinary error and timeout paths call `terminate` and propagate.
        let _ = self.signal(libc::SIGKILL);
    }
}

// Plain `#[cfg(test)]`, not `cfg(all(test, unix))`: cargo-mutants recognizes
// only the exact gate as test scaffolding and would otherwise mutate the
// helpers below and report them as surviving production mutants. The unix
// boundary moves onto the individual items.
#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;
    #[cfg(unix)]
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    #[cfg(unix)]
    use tokio::process::Command;

    /// Spawn a group leader with one long-lived grandchild, returning the
    /// leader, its guard, and the grandchild's pid.
    #[cfg(unix)]
    async fn spawn_group() -> (Child, ProcessGroup, i32) {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        isolate(&mut command);
        let mut child = command.spawn().expect("spawn group leader");
        let process_group = ProcessGroup::for_child(&child).expect("process group");
        let stdout = child.stdout.take().expect("child stdout");
        let mut lines = BufReader::new(stdout).lines();
        let grandchild_pid = lines
            .next_line()
            .await
            .expect("read grandchild pid")
            .expect("grandchild pid line")
            .parse()
            .expect("numeric grandchild pid");
        (child, process_group, grandchild_pid)
    }

    /// Poll until the grandchild is gone; a survivor of the cleanup fails.
    #[cfg(unix)]
    async fn assert_grandchild_gone(grandchild_pid: i32) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            // SAFETY: signal zero performs an existence check and does not
            // alter the process.
            let exists = unsafe { libc::kill(grandchild_pid, 0) } == 0;
            if !exists {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "grandchild {grandchild_pid} survived group cleanup"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn termination_kills_a_spawned_grandchild_too() {
        let (mut child, mut process_group, grandchild_pid) = spawn_group().await;

        process_group
            .terminate(&mut child)
            .await
            .expect("terminate process group");

        assert_grandchild_gone(grandchild_pid).await;
    }

    /// Dropping the guard without `terminate` is the cancellation path: the
    /// group must still die, because a dropped future cannot report a signal
    /// failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_the_guard_kills_a_spawned_grandchild_too() {
        let (mut child, process_group, grandchild_pid) = spawn_group().await;

        drop(process_group);

        assert_grandchild_gone(grandchild_pid).await;
        // Reap the leader explicitly rather than leaving it to kill_on_drop.
        let _ = child.wait().await;
    }
}
